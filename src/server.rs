//! HTTP dashboard for router telemetry.
//!
//! Spawned by `runai server [--port N] [--host H]`. Reads `~/.runai/runai.db`
//! and serves a single-page HTML dashboard plus JSON endpoints so users can
//! inspect every hook invocation: the user prompt, cwd, chosen skills, BM25
//! prefilter ratio, latency and token usage.
//!
//! No external CDN — index.html / app.js / app.css are bundled via
//! `include_str!` so the dashboard works offline (same single-binary
//! philosophy as the rest of runai).

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::core::db::{Database, RouterEvent};
use crate::core::paths::AppPaths;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");

/// Shared state for handlers. Holds only the DB path (and AppPaths if needed
/// later for other resources) — rusqlite `Connection` is `!Sync`, so each
/// handler opens its own connection per request. SQLite open is cheap
/// (microseconds for the same file in the OS page cache); this keeps the
/// server lock-free and avoids serialising readers on a Mutex.
struct AppState {
    db_path: PathBuf,
}

impl AppState {
    fn db(&self) -> Result<Database> {
        Database::open(&self.db_path)
    }
}

/// Result of `ensure_running`. `AlreadyRunning` is the hot path for repeat
/// invocations (hook / SessionStart); `Started` happens once per machine boot.
#[derive(Debug, PartialEq, Eq)]
pub enum EnsureStatus {
    AlreadyRunning,
    Started,
}

/// Idempotent "is the dashboard up? if not, spawn it" helper. Designed to be
/// called from Claude Code's SessionStart hook (or any shell rc) so the
/// dashboard is always reachable without the user remembering to start it.
///
/// Behavior:
/// - If we can TCP-connect to `host:port` within 200ms → return `AlreadyRunning`.
///   This is the steady-state hot path (< 50ms total).
/// - Otherwise spawn `runai server --port P --host H` as a detached child with
///   stdio nullified, then poll the port for up to ~2s and return `Started`
///   when it comes up. Returns an error only if the spawn itself fails or the
///   server never binds.
///
/// The detached child becomes an orphan when this process exits and is
/// reparented to init (PID 1), which keeps the server running across the
/// lifetime of the launching shell / Claude Code session.
pub fn ensure_running(host: &str, port: u16) -> Result<EnsureStatus> {
    use std::net::TcpStream;
    use std::time::Duration;

    let addr_str = format!("{host}:{port}");
    let sock: SocketAddr = addr_str
        .parse()
        .with_context(|| format!("parse {addr_str}"))?;
    if TcpStream::connect_timeout(&sock, Duration::from_millis(200)).is_ok() {
        return Ok(EnsureStatus::AlreadyRunning);
    }

    let exe = std::env::current_exe().context("locate runai binary via current_exe")?;
    std::process::Command::new(&exe)
        .arg("server")
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg(host)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawn `{}` server daemon", exe.display()))?;

    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        if TcpStream::connect_timeout(&sock, Duration::from_millis(100)).is_ok() {
            return Ok(EnsureStatus::Started);
        }
    }
    bail!("started runai server daemon but {addr_str} did not respond within 2s")
}

pub async fn serve(host: &str, port: u16) -> Result<()> {
    let paths = AppPaths::default_path();
    let state = Arc::new(AppState {
        db_path: paths.db_path(),
    });

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/app.js", get(serve_app_js))
        .route("/app.css", get(serve_app_css))
        .route("/api/summary", get(api_summary))
        .route("/api/timeline", get(api_timeline))
        .route("/api/events", get(api_events))
        .route("/api/event/{id}", get(api_event_by_id))
        .route("/api/skills", get(api_skills))
        .route("/api/skills/{name}/rating", axum::routing::post(api_set_rating).delete(api_clear_rating))
        .with_state(state);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("parse {host}:{port}"))?;
    println!("runai dashboard at http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, app)
        .await
        .context("axum::serve")?;
    Ok(())
}

async fn serve_index() -> Response {
    static_response(INDEX_HTML, "text/html; charset=utf-8")
}
async fn serve_app_js() -> Response {
    static_response(APP_JS, "application/javascript; charset=utf-8")
}
async fn serve_app_css() -> Response {
    static_response(APP_CSS, "text/css; charset=utf-8")
}

fn static_response(body: &'static str, content_type: &'static str) -> Response {
    (
        [(header::CONTENT_TYPE, content_type)],
        body.to_string(),
    )
        .into_response()
}

#[derive(Deserialize)]
struct EventsQuery {
    /// Filter to events newer than `now - hours` hours. None = all-time.
    hours: Option<i64>,
    /// Page size, default 50, hard-capped at 500.
    limit: Option<usize>,
    /// Zero-based offset.
    offset: Option<usize>,
    /// Filter by exact model name.
    model: Option<String>,
    /// Only return events where chosen != [].
    hit_only: Option<bool>,
}

#[derive(Serialize)]
struct PerModel {
    model: String,
    calls: i64,
    total_tokens: i64,
}

#[derive(Serialize)]
struct SummaryResponse {
    total: i64,
    hits: i64,
    errors: i64,
    hit_rate: f64,
    avg_latency_ms: Option<f64>,
    avg_prompt_tokens: f64,
    total_tokens: i64,
    per_model: Vec<PerModel>,
}

async fn api_summary(
    State(state): State<Arc<AppState>>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<SummaryResponse>, ApiError> {
    let since = q.hours.map(hours_to_since_ts);
    let db = state.db()?;
    let stats = db.router_stats_summary(since)?;
    // Compute hit count separately — router_stats_summary doesn't have it.
    let total_with_hit = db.router_events_count(since, None, true)?;
    let avg_prompt = if stats.total_calls > 0 {
        stats.total_prompt_tokens as f64 / stats.total_calls as f64
    } else {
        0.0
    };
    let hit_rate = if stats.total_calls > 0 {
        total_with_hit as f64 / stats.total_calls as f64
    } else {
        0.0
    };
    Ok(Json(SummaryResponse {
        total: stats.total_calls,
        hits: total_with_hit,
        errors: stats.errors,
        hit_rate,
        avg_latency_ms: stats.avg_latency_ms,
        avg_prompt_tokens: avg_prompt,
        total_tokens: stats.total_tokens,
        per_model: stats
            .per_model
            .into_iter()
            .map(|m| PerModel {
                model: m.model,
                calls: m.calls,
                total_tokens: m.total_tokens,
            })
            .collect(),
    }))
}

#[derive(Serialize)]
struct EventsResponse {
    total: i64,
    events: Vec<EventJson>,
}

#[derive(Serialize)]
struct EventJson {
    id: Option<i64>,
    ts: i64,
    model: String,
    provider: String,
    status: String,
    mode: String,
    chosen: Vec<String>,
    candidate_count: i64,
    bm25_kept: i64,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    latency_ms: i64,
    session_id: String,
    user_prompt: String,
    cwd: String,
    error_msg: Option<String>,
    /// Raw LLM response (mode tag + skill names). Empty for legacy rows.
    llm_raw_response: String,
    /// Markdown block runai injected into Claude Code via hook stdout.
    /// Empty when chosen was empty or for legacy rows.
    hook_output: String,
    /// Whether the hook actually delivered a non-empty injection. Equivalent
    /// to `chosen` non-empty + status ok, exposed as a flat boolean for the UI.
    injected: bool,
}

impl From<RouterEvent> for EventJson {
    fn from(e: RouterEvent) -> Self {
        let chosen: Vec<String> =
            serde_json::from_str(&e.chosen_skills_json).unwrap_or_default();
        let injected = e.status == "ok" && !chosen.is_empty();
        EventJson {
            id: e.id,
            ts: e.ts,
            model: e.model,
            provider: e.provider,
            status: e.status,
            mode: e.mode,
            chosen,
            candidate_count: e.candidate_count,
            bm25_kept: e.bm25_kept,
            prompt_tokens: e.prompt_tokens,
            completion_tokens: e.completion_tokens,
            total_tokens: e.total_tokens,
            latency_ms: e.latency_ms,
            session_id: e.session_id,
            user_prompt: e.user_prompt,
            cwd: e.cwd,
            error_msg: e.error_msg,
            llm_raw_response: e.llm_raw_response,
            hook_output: e.hook_output,
            injected,
        }
    }
}

async fn api_events(
    State(state): State<Arc<AppState>>,
    Query(q): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
    let since = q.hours.map(hours_to_since_ts);
    let limit = q.limit.unwrap_or(50).min(500);
    let offset = q.offset.unwrap_or(0);
    let model_ref = q.model.as_deref();
    let hit_only = q.hit_only.unwrap_or(false);
    let db = state.db()?;
    let events = db.router_events_paged(since, limit, offset, model_ref, hit_only)?;
    let total = db.router_events_count(since, model_ref, hit_only)?;
    Ok(Json(EventsResponse {
        total,
        events: events.into_iter().map(EventJson::from).collect(),
    }))
}

#[derive(Deserialize)]
struct TimelineQuery {
    /// Window length in hours. 24 -> 24 hourly buckets; 6 -> 6 hourly buckets.
    hours: Option<i64>,
    /// Optional bucket width override in seconds. Default = hours * 3600 / 24
    /// (so 24h -> hourly, 6h -> 15min, etc), capped to keep the chart legible.
    bucket_secs: Option<i64>,
}

#[derive(Serialize)]
struct TimelinePoint {
    ts_start: i64,
    total: i64,
    hits: i64,
    errors: i64,
    avg_latency_ms: f64,
}

#[derive(Serialize)]
struct TimelineResponse {
    bucket_secs: i64,
    points: Vec<TimelinePoint>,
}

async fn api_timeline(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TimelineQuery>,
) -> Result<Json<TimelineResponse>, ApiError> {
    let hours = q.hours.unwrap_or(24).clamp(1, 720);
    let target_buckets = 48i64;
    let default_bucket = ((hours * 3600) / target_buckets).max(60);
    let bucket_secs = q.bucket_secs.unwrap_or(default_bucket).max(60);
    let buckets = ((hours * 3600) / bucket_secs).max(1);
    let db = state.db()?;
    let raw = db.router_timeline(bucket_secs, buckets)?;
    Ok(Json(TimelineResponse {
        bucket_secs,
        points: raw
            .into_iter()
            .map(|b| TimelinePoint {
                ts_start: b.ts_start,
                total: b.total,
                hits: b.hits,
                errors: b.errors,
                avg_latency_ms: b.avg_latency_ms,
            })
            .collect(),
    }))
}

#[derive(Serialize)]
struct SkillRow {
    name: String,
    description: String,
    usage_count: i64,
    summary: String,
    llm_score: i64,
    user_stars: Option<i64>,
    combined_score: Option<i64>,
}

#[derive(Serialize)]
struct SkillsResponse {
    total: usize,
    enriched: usize,
    rated: usize,
    skills: Vec<SkillRow>,
}

async fn api_skills(State(state): State<Arc<AppState>>) -> Result<Json<SkillsResponse>, ApiError> {
    use crate::core::manager::SkillManager;
    use crate::core::resource::ResourceKind;

    // SkillManager reads from the same DB but also touches other state; for
    // a read-only listing it's fine to open it here on demand.
    let mgr = SkillManager::with_base(state.db_path.parent().unwrap().to_path_buf())
        .map_err(|e| ApiError::Internal(e))?;
    let resources = mgr
        .list_resources(None, None)
        .map_err(|e| ApiError::Internal(e))?;
    let db = state.db()?;
    let summaries = db.skill_ai_summary_all().unwrap_or_default();
    let scores = db.skill_scores_all().unwrap_or_default();

    let mut skills = Vec::new();
    let mut enriched = 0usize;
    let mut rated = 0usize;
    for r in resources {
        if r.kind != ResourceKind::Skill {
            continue;
        }
        let summary = summaries.get(&r.name).cloned().unwrap_or_default();
        let (llm, user) = scores.get(&r.name).copied().unwrap_or((50, None));
        if !summary.is_empty() {
            enriched += 1;
        }
        if user.is_some() {
            rated += 1;
        }
        let combined: Option<i64> = match user {
            Some(stars) => {
                let user100 = stars * 20;
                Some(((llm as f64) * 0.4 + (user100 as f64) * 0.6).round() as i64)
            }
            None => {
                if scores.contains_key(&r.name) {
                    Some(llm)
                } else {
                    None
                }
            }
        };
        skills.push(SkillRow {
            name: r.name.clone(),
            description: r.description.clone(),
            usage_count: r.usage_count as i64,
            summary,
            llm_score: llm,
            user_stars: user,
            combined_score: combined,
        });
    }
    let total = skills.len();
    // Highest combined score first; un-scored at the bottom
    skills.sort_by(|a, b| {
        b.combined_score
            .unwrap_or(-1)
            .cmp(&a.combined_score.unwrap_or(-1))
            .then(a.name.cmp(&b.name))
    });
    Ok(Json(SkillsResponse { total, enriched, rated, skills }))
}

#[derive(Deserialize)]
struct RatingBody {
    stars: i64,
    #[serde(default)]
    note: String,
}

async fn api_set_rating(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Json(body): Json<RatingBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db()?;
    db.set_user_rating(&name, body.stars, &body.note)?;
    Ok(Json(serde_json::json!({"ok": true, "name": name, "stars": body.stars})))
}

async fn api_clear_rating(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let db = state.db()?;
    db.delete_user_rating(&name)?;
    Ok(Json(serde_json::json!({"ok": true, "name": name})))
}

async fn api_event_by_id(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Json<EventJson>, ApiError> {
    let db = state.db()?;
    match db.router_event_by_id(id)? {
        Some(ev) => Ok(Json(ev.into())),
        None => Err(ApiError::NotFound),
    }
}

fn hours_to_since_ts(hours: i64) -> i64 {
    let now = chrono::Utc::now().timestamp();
    now - hours.max(0) * 3600
}

/// API error wrapper that maps anyhow into proper HTTP responses.
enum ApiError {
    Internal(anyhow::Error),
    NotFound,
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Internal(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response(),
            ApiError::NotFound => (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "not found"})),
            )
                .into_response(),
        }
    }
}

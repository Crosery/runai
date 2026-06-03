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
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::core::db::{Database, RouterEvent};
use crate::core::manager::SkillManager;
use crate::core::paths::AppPaths;
use crate::core::recommend;
use crate::core::recommend::local_ipv4;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");
/// Client-side install / uninstall scripts. The server serves these from
/// GET /install and GET /uninstall after replacing the `{SERVER_URL}`
/// placeholder with the URL the teammate just curl'd from, so the
/// resulting bash script already knows where to point the hook wrapper.
/// See scripts/runai-client-install.sh for the full doc.
const CLIENT_INSTALL_SH: &str = include_str!("../scripts/runai-client-install.sh");
const CLIENT_UNINSTALL_SH: &str = include_str!("../scripts/runai-client-uninstall.sh");
/// Windows / PowerShell variants of install / uninstall scripts. Served
/// from GET /install.ps1 + /uninstall.ps1 so teammates on Windows can run
/// `irm http://<server>/install.ps1 | iex` (PowerShell equivalent of
/// `curl ... | bash`). Same {SERVER_URL} placeholder substitution.
const CLIENT_INSTALL_PS1: &str = include_str!("../scripts/runai-client-install.ps1");
const CLIENT_UNINSTALL_PS1: &str = include_str!("../scripts/runai-client-uninstall.ps1");

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
        .route("/api/skill/{name}", get(api_skill_detail))
        .route("/api/skill/{name}/files", get(api_skill_files))
        .route("/api/skill/{name}/file", get(api_skill_file))
        // Remote-hook protocol: teammates' Claude Code UserPromptSubmit hooks
        // POST their standard hook JSON here and pipe stdout back into the
        // agent. See scripts/runai-client-install.sh for the wrapper they run.
        .route("/recommend", post(handle_recommend))
        .route("/skills/get/{name}", post(handle_skill_get))
        // GET /skills/file/{name}/{*path} — raw file body for any path
        // inside a skill directory. Acts as a Read-tool replacement so
        // remote teammates can fetch references/X.md, scripts/Y.py, etc.
        // referenced from SKILL.md without having the binary or the
        // skill files on disk locally.
        .route("/skills/file/{name}/{*path}", get(handle_skill_file))
        // GET /skills/bundle/{name} — gzipped tarball of the whole skill
        // directory (SKILL.md + scripts/ + references/ + anything else).
        // Client install scripts and hook wrappers fetch this once and
        // unpack into a local cache so SKILL.md's `scripts/foo.sh`
        // references work without round-tripping every file.
        .route("/skills/bundle/{name}", get(handle_skill_bundle))
        .route("/feedback", post(handle_feedback))
        .route("/install", get(handle_install_script))
        .route("/uninstall", get(handle_uninstall_script))
        .route("/install.ps1", get(handle_install_ps1))
        .route("/uninstall.ps1", get(handle_uninstall_ps1))
        // Settings — recommend config + providers CRUD for the dashboard
        // Settings tab. All endpoints return / accept JSON; api_key bytes
        // are never sent back to the browser (only `has_api_key: bool`).
        // v15: admin-gated (compat: open when no users exist yet).
        .route("/api/settings", get(api_get_settings).post(api_post_settings))
        .route("/api/providers", post(api_upsert_provider))
        .route("/api/providers/{id}", delete(api_delete_provider))
        .route("/api/providers/{id}/activate", post(api_activate_provider))
        // v15 admin: per-user management (list / promote / disable)
        .route("/api/admin/users", get(api_admin_users_list))
        .route(
            "/api/admin/users/{user_id}",
            post(api_admin_users_update).delete(api_admin_users_delete),
        )
        // v15 marketplace: any logged-in user can browse / install. Newly
        // installed skills go to the public pool AND are auto-subscribed
        // to the installing user's library.
        .route("/api/market", get(api_market_list))
        .route("/api/market/refresh", post(api_market_refresh))
        .route("/api/market/preview", get(api_market_preview))
        .route("/api/market/preview-files", get(api_market_preview_files))
        .route("/api/market/install", post(api_market_install))
        .route("/api/install/github", post(api_install_github))
        .route("/api/parse/github", post(api_parse_github))
        // ---- v15 multi-user (auth + per-user library + per-user prefs) ----
        .route("/users/register", post(api_register))
        .route("/auth/login", post(api_login))
        .route("/auth/logout", post(api_logout))
        .route("/api/me", get(api_me))
        .route("/api/prefs", get(api_get_prefs).post(api_post_prefs))
        .route(
            "/api/skills/library",
            get(api_library_list).post(api_library_mutate),
        )
        .route("/api/skills/library/clear", post(api_library_clear))
        .route("/api/skills/library/fill", post(api_library_fill))
        .route(
            "/api/skills/library/import-from-usage",
            post(api_library_import_from_usage),
        )
        .with_state(state);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("parse {host}:{port}"))?;
    println!("runai dashboard at http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, app).await.context("axum::serve")?;
    Ok(())
}

/// Process-lifetime cache-buster. Generated once when the server boots from
/// the current unix timestamp; injected into every `<link href="...">` /
/// `<script src="...">` URL in `index.html`. Every `runai server` restart
/// produces a fresh value, so the browser sees a different URL for the CSS
/// and JS and is forced to fetch the new bytes — no Cmd+Shift+R needed even
/// the first time after upgrade.
static BUILD_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
fn build_id() -> &'static str {
    BUILD_ID.get_or_init(|| chrono::Utc::now().timestamp().to_string())
}

async fn serve_index() -> Response {
    // Rewrite static asset URLs to include the per-boot build_id query
    // string so cached entries from a prior server boot can never satisfy
    // a request for this boot's assets.
    let bid = build_id();
    let patched = INDEX_HTML
        .replace("\"/app.css\"", &format!("\"/app.css?v={bid}\""))
        .replace("\"/app.js\"", &format!("\"/app.js?v={bid}\""));
    dynamic_response(patched, "text/html; charset=utf-8")
}
async fn serve_app_js() -> Response {
    static_response(APP_JS, "application/javascript; charset=utf-8")
}
async fn serve_app_css() -> Response {
    static_response(APP_CSS, "text/css; charset=utf-8")
}

fn static_response(body: &'static str, content_type: &'static str) -> Response {
    // `no-store` + must-revalidate: assets are bundled into the binary via
    // `include_str!` so the only way they change is when the binary is
    // rebuilt. Cache-Control = no-store stops the browser from reading its
    // disk cache without revalidating; the cache-busting query string in
    // `serve_index` is the belt-and-braces defense that handles browsers
    // that ignored a no-store directive on prior responses.
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store, must-revalidate"),
        ],
        body.to_string(),
    )
        .into_response()
}

fn dynamic_response(body: String, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store, must-revalidate"),
        ],
        body,
    )
        .into_response()
}

#[derive(Deserialize)]
struct EventsQuery {
    /// Filter to events newer than `now - hours` hours. None = all-time.
    /// Rolling window — events age out the back as time moves forward.
    hours: Option<i64>,
    /// Absolute unix-seconds cutoff. Preferred over `hours` when both are
    /// present. Lets the frontend pin "today since 00:00 local" without
    /// flipping to a rolling window — the count only moves up until
    /// midnight, never down.
    since_ts: Option<i64>,
    /// Page size, default 50, hard-capped at 500.
    limit: Option<usize>,
    /// Zero-based offset.
    offset: Option<usize>,
    /// Filter by exact model name.
    model: Option<String>,
    /// Only return events where chosen != [].
    hit_only: Option<bool>,
    /// v15 multi-user: who's events to show.
    /// - None / absent → default: current Bearer user only (per-user privacy)
    /// - "all"          → admin only, every user (global view)
    /// - "<user_id>"    → admin only, that specific user
    /// Non-admins get 403 if they try anything other than absent / their own uid.
    user: Option<String>,
}

impl EventsQuery {
    /// Resolve the "since" cutoff that all event queries gate on.
    /// `since_ts` wins over `hours` (calendar-aware queries from the
    /// Overview hero), `hours` is the legacy rolling-window mode used by
    /// the Activity tab's dropdown.
    fn since(&self) -> Option<i64> {
        if let Some(ts) = self.since_ts {
            return Some(ts);
        }
        self.hours.map(hours_to_since_ts)
    }
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
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Result<Json<SummaryResponse>, ApiError> {
    let since = q.since();
    let db = state.db()?;
    let view = resolve_view_user(&headers, &db, q.user.as_deref())?;
    let stats = db.router_stats_summary_filtered(since, view.as_deref())?;
    // Compute hit count separately — router_stats_summary doesn't have it.
    let total_with_hit = db.router_events_count_filtered(since, None, true, view.as_deref())?;
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
    /// Full user message sent to the router LLM (history + already_routed +
    /// candidate listing + user prompt). Empty for pre-schema-v13 rows.
    llm_input: String,
    /// Whether the hook actually delivered a non-empty injection. Equivalent
    /// to `chosen` non-empty + status ok, exposed as a flat boolean for the UI.
    injected: bool,
}

impl From<RouterEvent> for EventJson {
    fn from(e: RouterEvent) -> Self {
        let chosen: Vec<String> = serde_json::from_str(&e.chosen_skills_json).unwrap_or_default();
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
            llm_input: e.llm_input,
            injected,
        }
    }
}

async fn api_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
) -> Result<Json<EventsResponse>, ApiError> {
    let since = q.since();
    let limit = q.limit.unwrap_or(50).min(500);
    let offset = q.offset.unwrap_or(0);
    let model_ref = q.model.as_deref();
    let hit_only = q.hit_only.unwrap_or(false);
    let db = state.db()?;
    let view = resolve_view_user(&headers, &db, q.user.as_deref())?;
    let events = db
        .router_events_paged_filtered(since, limit, offset, model_ref, hit_only, view.as_deref())?;
    let total = db.router_events_count_filtered(since, model_ref, hit_only, view.as_deref())?;
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
    /// Same semantics as `EventsQuery::user`.
    user: Option<String>,
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
    headers: HeaderMap,
    Query(q): Query<TimelineQuery>,
) -> Result<Json<TimelineResponse>, ApiError> {
    let hours = q.hours.unwrap_or(24).clamp(1, 720);
    let target_buckets = 48i64;
    let default_bucket = ((hours * 3600) / target_buckets).max(60);
    let bucket_secs = q.bucket_secs.unwrap_or(default_bucket).max(60);
    let buckets = ((hours * 3600) / bucket_secs).max(1);
    let db = state.db()?;
    let view = resolve_view_user(&headers, &db, q.user.as_deref())?;
    let raw = db.router_timeline_filtered(bucket_secs, buckets, view.as_deref())?;
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
    llm_score: Option<i64>,
    /// Phase F: owner_user_id of the row backing this entry. `None` =
    /// public-pool; `Some(uid)` = private to that user. The frontend
    /// renders a "私有" / "公共" badge from this.
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_user_id: Option<String>,
}

#[derive(Serialize)]
struct SkillsResponse {
    total: usize,
    enriched: usize,
    skills: Vec<SkillRow>,
}

async fn api_skills(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SkillsResponse>, ApiError> {
    use crate::core::resource::ResourceKind;

    let db = state.db()?;
    // Phase D: scope the skill list per request owner so a remote user
    // sees public-pool skills plus their own private ones, not other
    // users' privates. Admin (`is_admin`) sees everything via "*".
    let me = current_user(&headers, &db).ok().flatten();
    let owner_scope: Option<String> = match &me {
        Some(u) if u.is_admin => Some("*".into()),
        Some(u) => Some(u.user_id.clone()),
        None => None,
    };
    let resources = db
        .list_resources_for_user(None, owner_scope.as_deref())
        .map_err(ApiError::Internal)?;
    let summaries = db.skill_ai_summary_all().unwrap_or_default();
    let scores = db.skill_llm_scores_all().unwrap_or_default();

    let mut skills = Vec::new();
    let mut enriched = 0usize;
    for r in resources {
        if r.kind != ResourceKind::Skill {
            continue;
        }
        let summary = summaries.get(&r.name).cloned().unwrap_or_default();
        if !summary.is_empty() {
            enriched += 1;
        }
        let llm_score = scores.get(&r.name).copied();
        skills.push(SkillRow {
            name: r.name.clone(),
            description: r.description.clone(),
            usage_count: r.usage_count as i64,
            summary,
            llm_score,
            owner_user_id: r.owner_user_id.clone(),
        });
    }
    let total = skills.len();
    skills.sort_by(|a, b| {
        b.llm_score
            .unwrap_or(-1)
            .cmp(&a.llm_score.unwrap_or(-1))
            .then(a.name.cmp(&b.name))
    });
    Ok(Json(SkillsResponse {
        total,
        enriched,
        skills,
    }))
}

#[derive(Serialize)]
struct SkillDetailResponse {
    name: String,
    description: String,
    usage_count: i64,
    summary: String,
    llm_score: Option<i64>,
    skill_md_path: String,
    skill_md_content: String,
    skill_md_size: usize,
    skill_md_truncated: bool,
    /// router_events where this skill was chosen, newest first, up to 50.
    events: Vec<EventJson>,
    events_total: usize,
}

async fn api_skill_detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<SkillDetailResponse>, ApiError> {
    use crate::core::resource::ResourceKind;

    let db = state.db()?;
    // Phase D: owner-aware lookup. Private rows win over public ones of
    // the same name for the current user; admin sees first private match.
    let me = current_user(&headers, &db).ok().flatten();
    let owner_scope: Option<String> = match &me {
        Some(u) if u.is_admin => Some("*".into()),
        Some(u) => Some(u.user_id.clone()),
        None => None,
    };
    let resource = db
        .find_resource_by_name_for_user(ResourceKind::Skill, &name, owner_scope.as_deref())
        .map_err(ApiError::Internal)?
        .ok_or(ApiError::NotFound)?;
    let summary = db.skill_ai_summary(&name).unwrap_or_default();
    let llm_score = if summary.is_empty() {
        None
    } else {
        Some(db.skill_llm_score(&name).unwrap_or(5))
    };
    let skill_md_path = resource.directory.join("SKILL.md");
    const MAX_BYTES: usize = 60_000;
    let (skill_md_content, truncated, total_size) = match std::fs::read_to_string(&skill_md_path) {
        Ok(body) => {
            let total = body.len();
            if total > MAX_BYTES {
                let trunc: String = body.chars().take(MAX_BYTES).collect();
                (trunc, true, total)
            } else {
                (body, false, total)
            }
        }
        Err(_) => (String::new(), false, 0),
    };
    let event_rows = db.router_events_for_skill(&name, 50).unwrap_or_default();
    let events_total = event_rows.len();
    let events: Vec<EventJson> = event_rows.into_iter().map(EventJson::from).collect();
    Ok(Json(SkillDetailResponse {
        name: resource.name.clone(),
        description: resource.description.clone(),
        usage_count: resource.usage_count as i64,
        summary,
        llm_score,
        skill_md_path: skill_md_path.display().to_string(),
        skill_md_content,
        skill_md_size: total_size,
        skill_md_truncated: truncated,
        events,
        events_total,
    }))
}

#[derive(Serialize)]
struct SkillFileEntry {
    /// Path relative to the skill directory (forward slashes).
    path: String,
    size: u64,
    is_text: bool,
}

#[derive(Serialize)]
struct SkillFilesResponse {
    name: String,
    skill_dir: String,
    entries: Vec<SkillFileEntry>,
}

#[derive(Serialize)]
struct SkillFileResponse {
    path: String,
    size: u64,
    /// File contents. Empty for binaries; binary files only return metadata.
    content: String,
    /// True if the file content was cut off due to size cap.
    truncated: bool,
    /// True if we returned content. False for binary/unsupported types —
    /// `content` will be empty and the UI should display a placeholder.
    is_text: bool,
}

#[derive(Deserialize)]
struct SkillFileQuery {
    path: String,
}

async fn api_skill_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<SkillFilesResponse>, ApiError> {
    use crate::core::manager::SkillManager;
    let mgr = SkillManager::with_base(state.db_path.parent().unwrap().to_path_buf())
        .map_err(ApiError::Internal)?;
    let db = state.db()?;
    let (skill_dir, _owner) = resolve_skill_dir(&headers, &db, mgr.paths(), &name)
        .map_err(|_| ApiError::NotFound)?;
    if !skill_dir.is_dir() {
        return Err(ApiError::NotFound);
    }
    let mut entries: Vec<SkillFileEntry> = Vec::new();
    walk_skill_dir(&skill_dir, &skill_dir, &mut entries)?;
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Json(SkillFilesResponse {
        name,
        skill_dir: skill_dir.display().to_string(),
        entries,
    }))
}

fn walk_skill_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<SkillFileEntry>,
) -> Result<(), ApiError> {
    let read = std::fs::read_dir(dir).map_err(|e| ApiError::Internal(e.into()))?;
    for entry in read {
        let entry = entry.map_err(|e| ApiError::Internal(e.into()))?;
        let path = entry.path();
        let file_name = entry.file_name();
        let fname_str = file_name.to_string_lossy();
        // Skip hidden/junk
        if fname_str.starts_with('.') {
            continue;
        }
        let md = entry.metadata().map_err(|e| ApiError::Internal(e.into()))?;
        if md.is_dir() {
            walk_skill_dir(root, &path, out)?;
        } else if md.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("strip_prefix: {e}")))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(SkillFileEntry {
                path: rel,
                size: md.len(),
                is_text: is_text_path(&path),
            });
        }
    }
    Ok(())
}

fn is_text_path(p: &std::path::Path) -> bool {
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "md" | "markdown"
            | "txt"
            | "json"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "sh"
            | "bash"
            | "zsh"
            | "fish"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "mjs"
            | "cjs"
            | "rs"
            | "go"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "css"
            | "scss"
            | "html"
            | "xml"
            | "xsd"
            | "xsl"
            | "xslt"
            | "dtd"
            | "csv"
            | "tsv"
            | "log"
            | "vue"
            | "svelte"
            | "rb"
            | "php"
            | "lua"
            | "swift"
            | "kt"
            | "kts"
            | "rst"
            | "tex"
            | "sql"
            | "dockerfile"
            | "makefile"
            | "env"
            | ""
    )
}

async fn api_skill_file(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<SkillFileQuery>,
) -> Result<Json<SkillFileResponse>, ApiError> {
    use crate::core::manager::SkillManager;
    let mgr = SkillManager::with_base(state.db_path.parent().unwrap().to_path_buf())
        .map_err(ApiError::Internal)?;
    let db = state.db()?;
    let (skill_dir, _owner) = resolve_skill_dir(&headers, &db, mgr.paths(), &name)
        .map_err(|_| ApiError::NotFound)?;
    let target = skill_dir.join(&q.path);
    // SECURITY: canonicalise both, verify target still under skill_dir.
    // Prevents `?path=../../etc/passwd` style traversal.
    let root_real = skill_dir
        .canonicalize()
        .map_err(|e| ApiError::Internal(e.into()))?;
    let target_real = match target.canonicalize() {
        Ok(p) => p,
        Err(_) => return Err(ApiError::NotFound),
    };
    if !target_real.starts_with(&root_real) {
        return Err(ApiError::NotFound);
    }
    let md = target_real.metadata().map_err(|_| ApiError::NotFound)?;
    if md.is_dir() {
        return Err(ApiError::NotFound);
    }
    let size = md.len();
    let is_text = is_text_path(&target_real);
    const MAX_BYTES: usize = 80_000;
    let (content, truncated) = if is_text {
        match std::fs::read_to_string(&target_real) {
            Ok(s) => {
                if s.len() > MAX_BYTES {
                    (s.chars().take(MAX_BYTES).collect::<String>(), true)
                } else {
                    (s, false)
                }
            }
            // text by extension but not valid UTF-8 → treat as binary
            Err(_) => {
                return Ok(Json(SkillFileResponse {
                    path: q.path,
                    size,
                    content: String::new(),
                    truncated: false,
                    is_text: false,
                }));
            }
        }
    } else {
        (String::new(), false)
    };
    Ok(Json(SkillFileResponse {
        path: q.path,
        size,
        content,
        truncated,
        is_text,
    }))
}

/// Pull a single field from the Claude Code hook payload, defaulting to
/// empty string when missing.
fn payload_str(payload: &serde_json::Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// POST /recommend — runai's remote skill router.
///
/// Body: the standard Claude Code UserPromptSubmit hook JSON (fields used:
/// `prompt`, `session_id`, `cwd`, `transcript_path`).
///
/// Optional `X-Runai-User: {user}@{host}` header — when present, the
/// teammate's identity is prefixed into the `session_id` so multiple
/// teammates' sessions don't collide in the router's per-session memory.
/// The install script writes this header automatically; manual callers can
/// omit it.
///
/// Returns the hook output string (markdown to be injected into the
/// teammate's Claude Code prompt) as plain text. Errors fall through to
/// 200 + empty body — the install script's `--max-time 30 || true`
/// pattern means a server hiccup never blocks the teammate's prompt.
async fn handle_recommend(headers: HeaderMap, Json(payload): Json<serde_json::Value>) -> Response {
    let user_prefix = headers
        .get("X-Runai-User")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    // Server-rendered hook output points the agent at THIS server. URL
    // derived from the request's Host header; falls back to the
    // dashboard default when missing. User header gets pasted into every
    // curl call so the server can session-prefix per teammate.
    let server_url = guess_server_url(&headers);
    let user_header_arg = if user_prefix.is_empty() {
        String::new()
    } else {
        format!(" -H 'X-Runai-User: {user_prefix}'")
    };

    // Extract auth before we hop to blocking. v15 multi-user: Bearer token
    // identifies which user_id to scope the recommendation against; when
    // absent, fall back to legacy single-user behavior (no filter, user_id
    // stamp on router_events is NULL — compat with existing clients).
    let bearer_user_id = {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        authmod::parse_bearer_header(bearer).map(|tok| authmod::key_hash(&tok))
    };

    // recommend() is blocking (reqwest::blocking + rusqlite). Hop onto a
    // blocking thread so the async runtime stays responsive.
    let join = tokio::task::spawn_blocking(move || -> Result<String> {
        let mgr = SkillManager::new()?;

        // Resolve user_id from bearer hash. None when no header / unknown key.
        let user_id_opt: Option<String> = match bearer_user_id.as_deref() {
            Some(h) => mgr
                .db()
                .find_user_by_api_key_hash(h)
                .ok()
                .flatten()
                .filter(|u| !u.disabled)
                .map(|u| u.user_id),
            None => None,
        };

        let prompt = payload_str(&payload, "prompt");
        if prompt.is_empty() {
            return Ok(String::new());
        }
        let cwd = payload_str(&payload, "cwd");
        let transcript = payload_str(&payload, "transcript_path");
        let claude_sid = payload_str(&payload, "session_id");

        // session_id is `{user_prefix}:{claude_sid}` when both present;
        // either alone when only one; empty when neither (single-user
        // local-test path).
        let sid_string: String = match (user_prefix.is_empty(), claude_sid.is_empty()) {
            (false, false) => format!("{user_prefix}:{claude_sid}"),
            (false, true) => user_prefix.clone(),
            (true, false) => claude_sid.clone(),
            (true, true) => String::new(),
        };

        let tpath_pb = if transcript.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(&transcript))
        };
        let sid_opt = if sid_string.is_empty() {
            None
        } else {
            Some(sid_string.as_str())
        };
        let cwd_opt = if cwd.is_empty() {
            None
        } else {
            Some(cwd.as_str())
        };

        let decision = recommend::recommend_for_user(
            &mgr,
            &prompt,
            tpath_pb.as_deref(),
            sid_opt,
            cwd_opt,
            user_id_opt.as_deref(),
        )?;

        let history = match sid_opt {
            Some(s) if !s.is_empty() => mgr
                .db()
                .router_session_recommended_skills(s)
                .unwrap_or_default(),
            _ => Vec::new(),
        };

        let mut cfg = recommend::RecommendConfig::load(mgr.paths()).unwrap_or_default();
        // Mirror the per-user override applied inside recommend_for_user
        // so the final hook trailer reflects the same source of truth.
        if let Some(uid) = user_id_opt.as_deref() {
            if let Ok(Some(user)) = mgr.db().find_user_by_id(uid) {
                let p = crate::core::prefs::UserPrefs::from_json_str(&user.prefs_json);
                cfg.skip_reminder_enabled = p.skip_reminder_enabled;
                if !p.skip_reminder_template.is_empty() {
                    cfg.skip_reminder_template = p.skip_reminder_template;
                }
            }
        }
        let skip_reminder = if cfg.skip_reminder_enabled {
            cfg.skip_reminder_template.as_str()
        } else {
            ""
        };
        Ok(recommend::format_for_hook_full(
            &decision,
            sid_opt.unwrap_or(""),
            &history,
            &server_url,
            &user_header_arg,
            skip_reminder,
        ))
    })
    .await;

    let body = match join {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            eprintln!("/recommend: recommend() failed: {e:#}");
            String::new()
        }
        Err(e) => {
            eprintln!("/recommend: spawn_blocking join failed: {e}");
            String::new()
        }
    };
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// GET /install — return the client install bash script with `{SERVER_URL}`
/// substituted by the URL the request came in on. Teammate runs:
///   curl -fsSL http://<server>:<port>/install | bash
/// Query string for /skills/get/{name}: optional `session_id` used to
/// session-prefix the adoption row.
#[derive(Deserialize)]
struct SkillGetQuery {
    #[serde(default)]
    session_id: String,
}

/// POST /skills/get/{name} — return SKILL.md body + record adoption.
///
/// Replaces the local-only `runai recommend get <name>` command for users
/// who don't have the binary. Side-effects (idempotent):
///   - record_usage: bumps the skill's usage_count
///   - record_session_adoption: writes (session_id, skill_name) row
///
/// session id = `{X-Runai-User}:{session_id query}` when both present.
async fn handle_skill_get(
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(q): Query<SkillGetQuery>,
) -> Response {
    let user_prefix = headers
        .get("X-Runai-User")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    let claude_sid = q.session_id;

    let server_url_for_get = guess_server_url(&headers);
    let user_header_arg = if user_prefix.is_empty() {
        String::new()
    } else {
        format!(" -H 'X-Runai-User: {user_prefix}'")
    };

    let headers_owned = headers.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<String> {
        let mgr = SkillManager::new()?;
        let (skill_dir, _owner_uid) =
            resolve_skill_dir(&headers_owned, mgr.db(), mgr.paths(), &name)?;
        let skill_md = skill_dir.join("SKILL.md");
        let content = std::fs::read_to_string(&skill_md)
            .with_context(|| format!("read {}", skill_md.display()))?;

        let _ = mgr.record_usage(&name);
        let sid_string = match (user_prefix.is_empty(), claude_sid.is_empty()) {
            (false, false) => format!("{user_prefix}:{claude_sid}"),
            (false, true) => user_prefix.clone(),
            (true, false) => claude_sid.clone(),
            (true, true) => String::new(),
        };
        if !sid_string.is_empty() {
            let _ = mgr.db().record_session_adoption(&sid_string, &name);
        }

        // List sibling files inside the skill directory so the remote
        // agent knows what `references/X.md` / `scripts/Y.py` exist and
        // how to curl them — server-mode equivalent of having the skill
        // dir on disk where the agent could `Read` siblings.
        let mut sibling_entries: Vec<String> = Vec::new();
        let _ = walk_skill_dir_plain(&skill_dir, &skill_dir, &mut sibling_entries);
        sibling_entries.sort();
        sibling_entries.retain(|p| p != "SKILL.md");

        let appendix = if sibling_entries.is_empty() {
            String::new()
        } else {
            let mut buf = String::from("\n\n---\n附加文件（按需取，curl 替代 Read）：\n");
            for rel in &sibling_entries {
                buf.push_str(&format!(
                    "  curl -s '{server_url_for_get}/skills/file/{name}/{rel}'{user_header_arg}\n"
                ));
            }
            buf
        };

        Ok(format!("{content}{appendix}"))
    })
    .await;

    match join {
        Ok(Ok(content)) => (
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            content,
        )
            .into_response(),
        Ok(Err(e)) => {
            eprintln!("/skills/get: {e:#}");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("skill not found: {e}\n"),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("/skills/get: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::from("internal error\n"),
            )
                .into_response()
        }
    }
}

/// Recursive directory walk that yields all non-hidden file paths
/// relative to `root`, as forward-slash strings. Errors are swallowed
/// per-entry so a single unreadable file doesn't kill the whole listing.
/// Plain anyhow::Result so it can be called from inside the
/// spawn_blocking closure (the ApiError-flavoured `walk_skill_dir` above
/// is the dashboard variant).
fn walk_skill_dir_plain(
    root: &std::path::Path,
    dir: &std::path::Path,
    out: &mut Vec<String>,
) -> Result<()> {
    let read = std::fs::read_dir(dir)?;
    for entry in read.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let fname_str = file_name.to_string_lossy();
        if fname_str.starts_with('.') {
            continue;
        }
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            let _ = walk_skill_dir_plain(root, &path, out);
        } else if md.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

/// GET /skills/file/{name}/{*path} — return the raw bytes of a file
/// inside a managed skill's directory. The "curl-as-Read" primitive:
/// remote teammates without the binary or the skill on disk can fetch
/// references/X.md, scripts/Y.py, templates/Z.html the same way Claude
/// Code's Read tool would on a machine that has the file locally.
///
/// SECURITY: canonicalise both the skill_dir root and the joined target,
/// and refuse anything that escapes the skill_dir. Prevents
/// `..%2f..%2fetc%2fpasswd`-style traversal.
/// GET /skills/bundle/{name} — gzipped tarball of the resolved skill
/// directory. Owner-aware via `resolve_skill_dir`: private rows shadow
/// public ones for the authenticated caller. The tar is built in-process
/// (we don't spawn a `tar` subprocess — pure-rust `tar` + `flate2`) so
/// the endpoint works the same on every OS the binary ships on.
///
/// SECURITY: walks the resolved `skill_dir` only; never follows symlinks
/// out of the tree. The tar layout uses the skill name as the top-level
/// directory so a client `tar -xzf` lands at `<cache>/<name>/...`.
async fn handle_skill_bundle(
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let name_for_header = name.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mgr = SkillManager::new()?;
        let (skill_dir, _owner_uid) =
            resolve_skill_dir(&headers, mgr.db(), mgr.paths(), &name)?;
        if !skill_dir.is_dir() {
            bail!("not a directory: {}", skill_dir.display());
        }
        let root_real = skill_dir
            .canonicalize()
            .with_context(|| format!("canonicalize {}", skill_dir.display()))?;

        let buf: Vec<u8> = Vec::new();
        let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        // append_dir_all uses the second arg as the path-on-disk to read,
        // the first arg as the in-archive top-level prefix. We prefix with
        // the skill name so `tar xzf` produces `<name>/SKILL.md` not bare files.
        tar.follow_symlinks(false);
        tar.append_dir_all(&name, &root_real)
            .with_context(|| format!("tar walk {}", root_real.display()))?;
        let enc = tar.into_inner()?;
        let gz_bytes = enc.finish()?;
        Ok(gz_bytes)
    })
    .await;

    match join {
        Ok(Ok(bytes)) => {
            let disposition = format!("attachment; filename=\"{name_for_header}.tar.gz\"");
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/gzip")
                .header(header::CONTENT_DISPOSITION, disposition)
                .body(axum::body::Body::from(bytes))
                .unwrap()
        }
        Ok(Err(e)) => {
            eprintln!("/skills/bundle: {e:#}");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("bundle not found: {e}\n"),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("/skills/bundle: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::from("internal error\n"),
            )
                .into_response()
        }
    }
}

async fn handle_skill_file(
    headers: HeaderMap,
    Path((name, sub_path)): Path<(String, String)>,
) -> Response {
    let join = tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, &'static str)> {
        let mgr = SkillManager::new()?;
        let (skill_dir, _owner_uid) =
            resolve_skill_dir(&headers, mgr.db(), mgr.paths(), &name)?;
        let target = skill_dir.join(&sub_path);
        let root_real = skill_dir
            .canonicalize()
            .with_context(|| format!("canonicalize {}", skill_dir.display()))?;
        let target_real = target
            .canonicalize()
            .with_context(|| format!("canonicalize {}", target.display()))?;
        if !target_real.starts_with(&root_real) {
            bail!("path escapes skill dir: {sub_path}");
        }
        let md = target_real.metadata()?;
        if !md.is_file() {
            bail!("not a file: {sub_path}");
        }
        let bytes = std::fs::read(&target_real)?;
        let ct = if is_text_path(&target_real) {
            "text/plain; charset=utf-8"
        } else {
            "application/octet-stream"
        };
        Ok((bytes, ct))
    })
    .await;

    match join {
        Ok(Ok((bytes, ct))) => ([(header::CONTENT_TYPE, ct)], bytes).into_response(),
        Ok(Err(e)) => {
            eprintln!("/skills/file: {e:#}");
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("not found: {e}\n"),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("/skills/file: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::from("internal error\n"),
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
struct FeedbackBody {
    skill: String,
    note: String,
}

/// POST /feedback — replaces `runai recommend feedback`.
/// Body: `{"skill":"...","note":"..."}`.
async fn handle_feedback(headers: HeaderMap, Json(req): Json<FeedbackBody>) -> Response {
    let user_prefix = headers
        .get("X-Runai-User")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();

    let join = tokio::task::spawn_blocking(move || -> Result<String> {
        let mgr = SkillManager::new()?;
        let report = recommend::reevaluate_skill(&mgr, &req.skill, &req.note)?;
        Ok(format!(
            "feedback applied by {user_prefix}: {} llm_score {} → {} (summary {} chars)\n",
            req.skill, report.old_score, report.new_score, report.new_summary_len
        ))
    })
    .await;

    match join {
        Ok(Ok(s)) => ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], s).into_response(),
        Ok(Err(e)) => {
            eprintln!("/feedback: {e:#}");
            (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("feedback error: {e}\n"),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("/feedback: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::from("internal error\n"),
            )
                .into_response()
        }
    }
}

async fn handle_install_script(headers: HeaderMap) -> Response {
    let server_url = guess_server_url(&headers);
    let body = CLIENT_INSTALL_SH.replace("{SERVER_URL}", &server_url);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        body,
    )
        .into_response()
}

/// GET /uninstall — return the client uninstall bash script. Reverses
/// /install: removes the hook entry from Claude Code settings.json and
/// deletes ~/.runai-hook.sh.
async fn handle_uninstall_script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        CLIENT_UNINSTALL_SH.to_string(),
    )
        .into_response()
}

/// GET /install.ps1 — Windows / PowerShell install. Teammate runs:
///   irm http://<server>:<port>/install.ps1 | iex
async fn handle_install_ps1(headers: HeaderMap) -> Response {
    let server_url = guess_server_url(&headers);
    let body = CLIENT_INSTALL_PS1.replace("{SERVER_URL}", &server_url);
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// GET /uninstall.ps1 — Windows / PowerShell uninstall.
async fn handle_uninstall_ps1() -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        CLIENT_UNINSTALL_PS1.to_string(),
    )
        .into_response()
}

/// Reconstruct the server URL clients should use. Strategy:
///   - Use the request's `Host` header normally.
///   - But if Host is a loopback (`127.0.0.1` / `localhost` / `[::1]`),
///     substitute the machine's real LAN IPv4 — the rendered URL has to
///     be reachable by teammates and Claude Code agents, not just by
///     processes on the same box. A loopback URL leaks out only because
///     curl came in over loopback (local healthcheck / agent on same
///     host); we want the LAN form regardless.
fn guess_server_url(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1:17888");

    let host_part = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let port_part = host.rsplit_once(':').map(|(_, p)| p).unwrap_or("17888");
    let is_loopback = matches!(host_part, "127.0.0.1" | "localhost" | "::1" | "[::1]");
    if is_loopback {
        if let Some(ip) = local_ipv4() {
            return format!("{scheme}://{ip}:{port_part}");
        }
    }
    format!("{scheme}://{host}")
}

async fn api_event_by_id(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<Json<EventJson>, ApiError> {
    let db = state.db()?;
    // Same tenant rule as the list endpoint: non-admin only sees their own
    // events; admin sees anything; compat mode (no users yet) is open.
    let view = resolve_view_user(&headers, &db, None)?;
    match db.router_event_by_id(id)? {
        Some(ev) => {
            if let Some(scope) = view.as_deref() {
                if ev.user_id.as_deref() != Some(scope) {
                    // Hide cross-tenant access — return 404 (not 403) so
                    // attackers can't enumerate event ids by status code.
                    return Err(ApiError::NotFound);
                }
            }
            Ok(Json(ev.into()))
        }
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
    Unauthorized,
    Forbidden,
    BadRequest(String),
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
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({"error": "unauthorized"})),
            )
                .into_response(),
            ApiError::Forbidden => (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "admin required"})),
            )
                .into_response(),
            ApiError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": msg})),
            )
                .into_response(),
        }
    }
}

// =====================================================================
// Settings API — exposes `RecommendConfig` to the dashboard Settings tab.
// `api_key` bytes never travel back to the browser; the wire shape only
// carries `has_api_key: bool` per provider so the UI can show a "key set"
// indicator without leaking the secret.
// =====================================================================

#[derive(Serialize)]
struct ProviderView {
    id: String,
    label: String,
    kind: String,
    base_url: String,
    model: String,
    has_api_key: bool,
}

#[derive(Serialize)]
struct SettingsView {
    enabled: bool,
    top_k: usize,
    session_mode: String,
    session_history_limit: usize,
    summary_lang: String,
    active_provider_id: String,
    read_claude_md: bool,
    skip_reminder_enabled: bool,
    skip_reminder_template: String,
    providers: Vec<ProviderView>,
}

fn provider_kind_str(k: recommend::Provider) -> &'static str {
    match k {
        recommend::Provider::OpenaiCompat => "openai-compat",
        recommend::Provider::Anthropic => "anthropic",
        recommend::Provider::ClaudeCli => "claude-cli",
    }
}

fn provider_kind_from_str(s: &str) -> Option<recommend::Provider> {
    match s {
        "openai-compat" => Some(recommend::Provider::OpenaiCompat),
        "anthropic" => Some(recommend::Provider::Anthropic),
        "claude-cli" => Some(recommend::Provider::ClaudeCli),
        _ => None,
    }
}

fn session_mode_str(m: recommend::SessionMode) -> &'static str {
    match m {
        recommend::SessionMode::Oneshot => "oneshot",
        recommend::SessionMode::Conversation => "conversation",
    }
}

fn session_mode_from_str(s: &str) -> Option<recommend::SessionMode> {
    match s {
        "oneshot" => Some(recommend::SessionMode::Oneshot),
        "conversation" => Some(recommend::SessionMode::Conversation),
        _ => None,
    }
}

fn render_settings(cfg: &recommend::RecommendConfig) -> SettingsView {
    SettingsView {
        enabled: cfg.enabled,
        top_k: cfg.top_k,
        session_mode: session_mode_str(cfg.session_mode).to_string(),
        session_history_limit: cfg.session_history_limit,
        summary_lang: cfg.summary_lang.clone(),
        active_provider_id: cfg.active_provider_id.clone(),
        read_claude_md: cfg.read_claude_md,
        skip_reminder_enabled: cfg.skip_reminder_enabled,
        skip_reminder_template: cfg.skip_reminder_template.clone(),
        providers: cfg
            .saved_providers
            .iter()
            .map(|p| ProviderView {
                id: p.id.clone(),
                label: p.label.clone(),
                kind: provider_kind_str(p.kind).to_string(),
                base_url: p.base_url.clone(),
                model: p.model.clone(),
                has_api_key: !p.api_key.is_empty(),
            })
            .collect(),
    }
}

async fn api_get_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        // Settings tab shows recommend / providers config — admin only.
        // Compat: if the users table is empty (fresh install before any
        // registration) keep the legacy "no auth" behavior so first-run
        // setup wizard from the dashboard still works.
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::default_path();
        let cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

#[derive(Deserialize)]
struct SettingsPatch {
    enabled: Option<bool>,
    top_k: Option<usize>,
    session_mode: Option<String>,
    session_history_limit: Option<usize>,
    summary_lang: Option<String>,
    read_claude_md: Option<bool>,
    skip_reminder_enabled: Option<bool>,
    skip_reminder_template: Option<String>,
}

async fn api_post_settings(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::default_path();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        if let Some(v) = patch.enabled {
            cfg.enabled = v;
        }
        if let Some(v) = patch.top_k {
            cfg.top_k = v;
        }
        if let Some(v) = patch.session_mode {
            if let Some(m) = session_mode_from_str(&v) {
                cfg.session_mode = m;
            }
        }
        if let Some(v) = patch.session_history_limit {
            cfg.session_history_limit = v;
        }
        if let Some(v) = patch.summary_lang {
            cfg.summary_lang = v;
            // Choosing a summary language from the dashboard is an explicit
            // user action — release the enrich gate (same as `recommend setup`).
            cfg.summary_lang_confirmed = true;
        }
        if let Some(v) = patch.read_claude_md {
            cfg.read_claude_md = v;
        }
        if let Some(v) = patch.skip_reminder_enabled {
            cfg.skip_reminder_enabled = v;
        }
        if let Some(v) = patch.skip_reminder_template {
            cfg.skip_reminder_template = v;
        }
        cfg.save(&paths).map_err(ApiError::Internal)?;
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

#[derive(Deserialize)]
struct ProviderPatch {
    id: String,
    label: String,
    kind: String,
    base_url: String,
    model: String,
    /// Empty string = keep existing api_key (when editing an existing entry)
    /// or store empty (when adding new). UI sends `""` to preserve secrets
    /// that never round-tripped to the browser.
    #[serde(default)]
    api_key: String,
}

async fn api_upsert_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(patch): Json<ProviderPatch>,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        if patch.id.trim().is_empty() {
            return Err(ApiError::BadRequest("provider id is required".into()));
        }
        let kind = match provider_kind_from_str(&patch.kind) {
            Some(k) => k,
            None => return Err(ApiError::BadRequest(format!(
                "unknown provider kind: {}",
                patch.kind
            ))),
        };
        let paths = AppPaths::default_path();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        // Preserve existing api_key when payload sends empty (UI never gets
        // the bytes, so empty here = "don't change").
        let existing_key = cfg
            .saved_providers
            .iter()
            .find(|p| p.id == patch.id)
            .map(|p| p.api_key.clone());
        let api_key = if patch.api_key.is_empty() {
            existing_key.unwrap_or_default()
        } else {
            patch.api_key
        };
        let entry = recommend::ProviderEntry {
            id: patch.id,
            label: patch.label,
            kind,
            base_url: patch.base_url,
            model: patch.model,
            api_key,
        };
        cfg.upsert_provider(entry);
        cfg.save(&paths).map_err(ApiError::Internal)?;
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

async fn api_delete_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::default_path();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        if !cfg.remove_provider(&id) {
            return Err(ApiError::BadRequest(format!("provider not found: {id}")));
        }
        cfg.save(&paths).map_err(ApiError::Internal)?;
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

async fn api_activate_provider(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<SettingsView>, ApiError> {
    tokio::task::spawn_blocking(move || -> Result<SettingsView, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        if !db.list_users().map_err(ApiError::Internal)?.is_empty() {
            require_admin(&headers, &db)?;
        }
        let paths = AppPaths::default_path();
        let mut cfg = recommend::RecommendConfig::load(&paths).unwrap_or_default();
        if !cfg.activate_provider(&id) {
            return Err(ApiError::BadRequest(format!("provider not found: {id}")));
        }
        cfg.save(&paths).map_err(ApiError::Internal)?;
        Ok(render_settings(&cfg))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
    .map(Json)
}

// ===================================================================
//  v15 multi-user handlers
//
//  Auth resolution order: Authorization: Bearer first (client hook),
//  then runai_session cookie (browser dashboard). Both end up at the
//  same User row via api_key_hash lookup, so password is only used at
//  /auth/login to mint the cookie.
// ===================================================================

use crate::core::auth as authmod;
use crate::core::db::User;
use crate::core::prefs::UserPrefs;

/// Resolve the current user from request headers. Returns None when no
/// credential is presented, or when the credential doesn't match any
/// non-disabled user. Used by every authenticated route.
fn current_user(headers: &HeaderMap, db: &Database) -> Result<Option<User>> {
    // 1. Bearer header (hook path)
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Some(token) = authmod::parse_bearer_header(auth) {
        let h = authmod::key_hash(&token);
        if let Some(u) = db.find_user_by_api_key_hash(&h)? {
            if u.disabled {
                return Ok(None);
            }
            return Ok(Some(u));
        }
    }

    // 2. Cookie session (dashboard path) — cookie value is the raw api_key
    let cookie_hdr = headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
    if let Some(tok) = authmod::parse_session_cookie(cookie_hdr) {
        let token = authmod::BearerToken(tok);
        let h = authmod::key_hash(&token);
        if let Some(u) = db.find_user_by_api_key_hash(&h)? {
            if u.disabled {
                return Ok(None);
            }
            return Ok(Some(u));
        }
    }
    Ok(None)
}

fn require_user(headers: &HeaderMap, db: &Database) -> Result<User, ApiError> {
    match current_user(headers, db).map_err(ApiError::Internal)? {
        Some(u) => Ok(u),
        None => Err(ApiError::Unauthorized),
    }
}

/// Owner scope to use when looking up / writing skill resources for the
/// current request. `Ok(None)` means anonymous / compat (public pool);
/// `Ok(Some(uid))` means private to the authenticated user. Never errors
/// — auth failure degrades to anonymous on purpose so the legacy unauth
/// client paths keep working.
fn current_owner_id(headers: &HeaderMap, db: &Database) -> Option<String> {
    current_user(headers, db).ok().flatten().map(|u| u.user_id)
}

/// Resolve `(skill_dir, owner_user_id)` for `/skills/get` and `/skills/file`.
///
/// Lookup order:
/// 1. If the caller is authenticated and has a private skill named `name`,
///    return its directory + owner_user_id.
/// 2. Otherwise fall back to a public skill with this name (DB row or, for
///    the compat window, the on-disk directory under `paths.skills_dir()`).
///
/// Errors only when neither path resolves. Callers should map that to 404.
fn resolve_skill_dir(
    headers: &HeaderMap,
    db: &Database,
    paths: &crate::core::paths::AppPaths,
    name: &str,
) -> Result<(std::path::PathBuf, Option<String>)> {
    let owner = current_owner_id(headers, db);
    if let Some(row) = db
        .find_resource_by_name_for_user(crate::core::resource::ResourceKind::Skill, name, owner.as_deref())?
    {
        return Ok((row.directory.clone(), row.owner_user_id.clone()));
    }
    // Compat: a public skill might exist on disk without a DB row (e.g.
    // a freshly-cloned tree that hasn't been `runai scan`-ed yet).
    let public_dir = paths.skills_dir().join(name);
    if public_dir.exists() {
        return Ok((public_dir, None));
    }
    anyhow::bail!("skill not found: {name}")
}

/// Returns the current user only if they are admin. Used to gate
/// provider/settings/users-management endpoints.
fn require_admin(headers: &HeaderMap, db: &Database) -> Result<User, ApiError> {
    let u = require_user(headers, db)?;
    if !u.is_admin {
        return Err(ApiError::Forbidden);
    }
    Ok(u)
}

/// Resolve "which user_id should we filter activity by". Returns
/// `Some(uid)` to scope SQL to that user, `None` for global view.
///
/// Industry-standard tenant isolation (Linear / Stripe / GitHub dashboards
/// all follow the same pattern):
///   - **No auth → 401.** Activity / events / summary are private telemetry;
///     anonymous reads are rejected.
///   - **Non-admin → forced own scope.** Even if `?user=` is passed,
///     anything other than their own uid is 403. Default scope is their
///     own uid, no opt-out.
///   - **Admin → global by default, can target a specific user.** Default
///     (no `?user=` param) returns `None` so admin's Activity tab shows
///     the global view straight away. `?user=<uid>` scopes to that user
///     for cross-user inspection. `?user=me` is a convenience alias for
///     "filter to my own events".
///
/// First-run bootstrap: the very first /users/register call auto-promotes
/// the first registered account to admin, so the dashboard is usable
/// immediately after a fresh deployment.
fn resolve_view_user(
    headers: &HeaderMap,
    db: &Database,
    requested: Option<&str>,
) -> Result<Option<String>, ApiError> {
    // Compat carve-out: when the users table is literally empty (no one
    // has registered yet), allow anonymous read so the very first visit
    // to the dashboard isn't a 401 wall. The moment any user is created,
    // auth becomes mandatory.
    if db.list_users().map_err(ApiError::Internal)?.is_empty() {
        return Ok(None);
    }
    let me = require_user(headers, db)?;
    let req = requested.map(str::trim).filter(|s| !s.is_empty());
    if me.is_admin {
        match req {
            None | Some("all") => Ok(None),
            Some("me") => Ok(Some(me.user_id)),
            Some(other) => Ok(Some(other.to_string())),
        }
    } else {
        match req {
            None => Ok(Some(me.user_id)),
            Some("me") => Ok(Some(me.user_id)),
            Some(uid) if uid == me.user_id => Ok(Some(me.user_id)),
            _ => Err(ApiError::Forbidden),
        }
    }
}

#[derive(Deserialize)]
struct RegisterReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct RegisterResp {
    user_id: String,
    username: String,
    api_key: String,
    is_admin: bool,
}

/// POST /users/register
/// body: {"username": "...", "password": "..."}
/// Side effects: creates user row, pre-fills top 30 public skills into
/// user_skill_library, returns user_id + api_key. api_key is shown ONCE —
/// client persists it to ~/.runai-identity.
/// First registered user is auto-promoted to admin.
async fn api_register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterReq>,
) -> Result<(StatusCode, Json<RegisterResp>), ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<RegisterResp, ApiError> {
        authmod::validate_username(&req.username)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        authmod::validate_password(&req.password)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        let db = state.db().map_err(ApiError::Internal)?;

        if db
            .find_user_by_username(&req.username)
            .map_err(ApiError::Internal)?
            .is_some()
        {
            return Err(ApiError::BadRequest("username already taken".into()));
        }

        let is_first_user = db.list_users().map_err(ApiError::Internal)?.is_empty();
        let user_id = authmod::new_user_id();
        let api_key = authmod::new_api_key();
        let password_hash = authmod::hash_password(&req.password).map_err(ApiError::Internal)?;
        let api_key_hash = authmod::key_hash(&authmod::BearerToken(api_key.clone()));

        db.create_user(
            &user_id,
            &req.username,
            &password_hash,
            &api_key_hash,
            is_first_user,
        )
        .map_err(ApiError::Internal)?;

        // Pre-fill library with top 30 popular public skills so the user's
        // first /recommend isn't empty.
        let prefill = db.top_public_skills(30).map_err(ApiError::Internal)?;
        for name in &prefill {
            let _ = db.library_add(&user_id, name);
        }

        Ok(RegisterResp {
            user_id,
            username: req.username,
            api_key,
            is_admin: is_first_user,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok((StatusCode::CREATED, Json(resp)))
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
struct LoginResp {
    user_id: String,
    username: String,
    is_admin: bool,
    api_key: String,
}

/// POST /auth/login
/// body: {"username": "...", "password": "..."}
/// Returns the api_key in JSON and sets the runai_session cookie. Either
/// channel can be used afterwards.
async fn api_login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginReq>,
) -> Result<Response, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LoginResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = match db
            .find_user_by_username(&req.username)
            .map_err(ApiError::Internal)?
        {
            Some(u) => u,
            None => return Err(ApiError::Unauthorized),
        };
        if user.disabled {
            return Err(ApiError::Unauthorized);
        }
        if !authmod::verify_password(&req.password, &user.password_hash)
            .map_err(ApiError::Internal)?
        {
            return Err(ApiError::Unauthorized);
        }

        // On login we rotate-issue: re-emit the api_key from the row.
        // We cannot recover the original secret from the hash, so login
        // must mint a new api_key and update the hash. This invalidates
        // any previously-installed client that hasn't logged in since —
        // intentional: the password is the source of truth.
        let new_key = authmod::new_api_key();
        let new_hash = authmod::key_hash(&authmod::BearerToken(new_key.clone()));
        db.rotate_api_key(&user.user_id, &new_hash)
            .map_err(ApiError::Internal)?;

        Ok(LoginResp {
            user_id: user.user_id,
            username: user.username,
            is_admin: user.is_admin,
            api_key: new_key,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;

    let cookie = authmod::build_session_cookie(&resp.api_key, false, 60 * 60 * 24 * 30);
    let body = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::SET_COOKIE, cookie),
        ],
        body,
    )
        .into_response())
}

/// POST /auth/logout — clears the session cookie. Does NOT rotate the
/// api_key; the client-side hook continues to work with its stored Bearer.
async fn api_logout() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::SET_COOKIE, authmod::build_logout_cookie()),
        ],
        r#"{"ok":true}"#.to_string(),
    )
        .into_response()
}

#[derive(Serialize)]
struct MeResp {
    user_id: String,
    username: String,
    is_admin: bool,
    library_size: usize,
}

/// GET /api/me — current user info. 401 when unauthenticated.
async fn api_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<MeResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MeResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let size = db.library_count(&u.user_id).map_err(ApiError::Internal)?;
        Ok(MeResp {
            user_id: u.user_id,
            username: u.username,
            is_admin: u.is_admin,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

/// GET /api/prefs — current user prefs (parsed from prefs_json).
async fn api_get_prefs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<UserPrefs>, ApiError> {
    let prefs = tokio::task::spawn_blocking(move || -> Result<UserPrefs, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        Ok(UserPrefs::from_json_str(&u.prefs_json))
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(prefs))
}

/// POST /api/prefs — replace current user prefs (full object).
async fn api_post_prefs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(new_prefs): Json<UserPrefs>,
) -> Result<Json<UserPrefs>, ApiError> {
    let prefs = tokio::task::spawn_blocking(move || -> Result<UserPrefs, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let json = new_prefs.to_json_str();
        db.update_user_prefs(&u.user_id, &json)
            .map_err(ApiError::Internal)?;
        Ok(new_prefs)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(prefs))
}

#[derive(Serialize)]
struct LibraryEntry {
    name: String,
}

#[derive(Serialize)]
struct LibraryListResp {
    user_id: String,
    items: Vec<LibraryEntry>,
    total: usize,
}

/// GET /api/skills/library — list of skill names in the current user's library.
async fn api_library_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<LibraryListResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryListResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let names = db.library_list(&u.user_id).map_err(ApiError::Internal)?;
        Ok(LibraryListResp {
            total: names.len(),
            items: names.into_iter().map(|name| LibraryEntry { name }).collect(),
            user_id: u.user_id,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct LibraryMutateReq {
    /// "add" | "remove"
    action: String,
    names: Vec<String>,
}

#[derive(Serialize)]
struct LibraryMutateResp {
    action: String,
    affected: usize,
    library_size: usize,
}

/// POST /api/skills/library — batch add or remove names.
async fn api_library_mutate(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<LibraryMutateReq>,
) -> Result<Json<LibraryMutateResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryMutateResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let action = req.action.to_lowercase();
        let mut affected = 0usize;
        match action.as_str() {
            "add" => {
                for name in &req.names {
                    if !db
                        .library_contains(&u.user_id, name)
                        .map_err(ApiError::Internal)?
                    {
                        db.library_add(&u.user_id, name)
                            .map_err(ApiError::Internal)?;
                        affected += 1;
                    }
                }
            }
            "remove" => {
                for name in &req.names {
                    if db
                        .library_contains(&u.user_id, name)
                        .map_err(ApiError::Internal)?
                    {
                        db.library_remove(&u.user_id, name)
                            .map_err(ApiError::Internal)?;
                        affected += 1;
                    }
                }
            }
            _ => return Err(ApiError::BadRequest(format!("unknown action: {action}"))),
        }
        let size = db
            .library_count(&u.user_id)
            .map_err(ApiError::Internal)?;
        Ok(LibraryMutateResp {
            action,
            affected,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

/// POST /api/skills/library/clear — remove every entry for current user.
async fn api_library_clear(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<LibraryMutateResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryMutateResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let n = db.library_clear(&u.user_id).map_err(ApiError::Internal)?;
        Ok(LibraryMutateResp {
            action: "clear".into(),
            affected: n,
            library_size: 0,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct LibraryFillQuery {
    #[serde(default)]
    top: Option<usize>,
}

/// POST /api/skills/library/fill?top=N — add top N public skills (by global
/// usage_count) that aren't already in the user's library. Idempotent.
async fn api_library_fill(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<LibraryFillQuery>,
) -> Result<Json<LibraryMutateResp>, ApiError> {
    let top = q.top.unwrap_or(50).clamp(1, 500);
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryMutateResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        let names = db.top_public_skills(top).map_err(ApiError::Internal)?;
        let mut affected = 0;
        for name in names {
            if !db
                .library_contains(&u.user_id, &name)
                .map_err(ApiError::Internal)?
            {
                db.library_add(&u.user_id, &name)
                    .map_err(ApiError::Internal)?;
                affected += 1;
            }
        }
        let size = db
            .library_count(&u.user_id)
            .map_err(ApiError::Internal)?;
        Ok(LibraryMutateResp {
            action: "fill".into(),
            affected,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

/// POST /api/skills/library/import-from-usage — add every skill that
/// appears in this user's router_events.chosen_skills_json (i.e. has been
/// recommended TO them at least once). For bootstrapping the library from
/// the pre-multi-user usage history.
async fn api_library_import_from_usage(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<LibraryMutateResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<LibraryMutateResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;

        // Fast-path SQL: pull JSON arrays for this user and merge in Rust.
        let mut stmt = db
            .conn_ref()
            .prepare(
                "SELECT chosen_skills_json FROM router_events
                 WHERE user_id = ?1 AND chosen_skills_json IS NOT NULL
                 AND chosen_skills_json != '[]'",
            )
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let rows = stmt
            .query_map([&u.user_id], |r| r.get::<_, String>(0))
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let mut seen = std::collections::BTreeSet::new();
        for row in rows {
            let json = row.map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(&json) {
                for name in arr {
                    seen.insert(name);
                }
            }
        }

        let mut affected = 0;
        for name in &seen {
            if !db
                .library_contains(&u.user_id, name)
                .map_err(ApiError::Internal)?
            {
                db.library_add(&u.user_id, name)
                    .map_err(ApiError::Internal)?;
                affected += 1;
            }
        }
        let size = db
            .library_count(&u.user_id)
            .map_err(ApiError::Internal)?;
        Ok(LibraryMutateResp {
            action: "import-from-usage".into(),
            affected,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

// ===================================================================
//  v15 admin — user management (list / promote / disable / delete)
//  All routes require an admin Bearer / cookie. First-user-auto-admin
//  bootstrap means the very first /users/register call grants admin.
// ===================================================================

#[derive(Serialize)]
struct AdminUserRow {
    user_id: String,
    username: String,
    is_admin: bool,
    disabled: bool,
    created_at: i64,
    library_size: usize,
    event_count: i64,
}

#[derive(Serialize)]
struct AdminUsersListResp {
    total: usize,
    items: Vec<AdminUserRow>,
}

async fn api_admin_users_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AdminUsersListResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminUsersListResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_admin(&headers, &db)?;
        let users = db.list_users().map_err(ApiError::Internal)?;
        let mut items = Vec::with_capacity(users.len());
        for u in &users {
            let library_size = db.library_count(&u.user_id).unwrap_or(0);
            let event_count: i64 = db
                .conn_ref()
                .query_row(
                    "SELECT COUNT(*) FROM router_events WHERE user_id = ?1",
                    [&u.user_id],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            items.push(AdminUserRow {
                user_id: u.user_id.clone(),
                username: u.username.clone(),
                is_admin: u.is_admin,
                disabled: u.disabled,
                created_at: u.created_at,
                library_size,
                event_count,
            });
        }
        Ok(AdminUsersListResp {
            total: items.len(),
            items,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct AdminUserPatch {
    #[serde(default)]
    is_admin: Option<bool>,
    #[serde(default)]
    disabled: Option<bool>,
}

async fn api_admin_users_update(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Json(patch): Json<AdminUserPatch>,
) -> Result<Json<AdminUserRow>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminUserRow, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let admin = require_admin(&headers, &db)?;

        // Sanity: target must exist.
        let target = db
            .find_user_by_id(&user_id)
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;

        // Self-demote / self-disable guard — admin cannot strip their own
        // privileges via this endpoint (avoids accidentally locking the
        // dashboard out of admin completely).
        if target.user_id == admin.user_id {
            if patch.is_admin == Some(false) {
                return Err(ApiError::BadRequest(
                    "cannot demote yourself".into(),
                ));
            }
            if patch.disabled == Some(true) {
                return Err(ApiError::BadRequest(
                    "cannot disable yourself".into(),
                ));
            }
        }

        if let Some(v) = patch.is_admin {
            db.set_user_admin(&user_id, v).map_err(ApiError::Internal)?;
        }
        if let Some(v) = patch.disabled {
            db.set_user_disabled(&user_id, v)
                .map_err(ApiError::Internal)?;
        }

        let updated = db
            .find_user_by_id(&user_id)
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;
        let library_size = db.library_count(&updated.user_id).unwrap_or(0);
        let event_count: i64 = db
            .conn_ref()
            .query_row(
                "SELECT COUNT(*) FROM router_events WHERE user_id = ?1",
                [&updated.user_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        Ok(AdminUserRow {
            user_id: updated.user_id,
            username: updated.username,
            is_admin: updated.is_admin,
            disabled: updated.disabled,
            created_at: updated.created_at,
            library_size,
            event_count,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Serialize)]
struct AdminDeleteResp {
    user_id: String,
    deleted_library: usize,
}

async fn api_admin_users_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<Json<AdminDeleteResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminDeleteResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let admin = require_admin(&headers, &db)?;
        if admin.user_id == user_id {
            return Err(ApiError::BadRequest("cannot delete yourself".into()));
        }
        // Sanity: target must exist.
        if db
            .find_user_by_id(&user_id)
            .map_err(ApiError::Internal)?
            .is_none()
        {
            return Err(ApiError::NotFound);
        }
        let lib_size = db.library_count(&user_id).unwrap_or(0);
        let _ = db.library_clear(&user_id);
        db.conn_ref()
            .execute("DELETE FROM users WHERE user_id = ?1", [&user_id])
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        Ok(AdminDeleteResp {
            user_id,
            deleted_library: lib_size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

// ===================================================================
//  v15 marketplace + GitHub install (any authenticated user)
//
//  Successful install writes the skill to the public pool
//  (~/.runai/skills/<name>) and auto-subscribes it to the installing
//  user's library so the new skill appears in their "我的库" scope
//  immediately.
// ===================================================================

use crate::core::cli_target::CliTarget;
use crate::core::market::{self as mkt};

/// Refresh every market source's cached skill index. Concurrent fetch
/// per source; failures are silently skipped (one bad source doesn't
/// poison the rest of the index).
async fn refresh_all_sources(sources: &[mkt::SourceEntry], data_dir: &std::path::Path) {
    let mut handles = Vec::new();
    for source in sources.iter().cloned() {
        let data_dir = data_dir.to_path_buf();
        handles.push(tokio::spawn(async move {
            match mkt::Market::fetch(&source).await {
                Ok(extract) => {
                    let _ = mkt::save_cache(&data_dir, &source, &extract.skills);
                    Some(extract.skills.len())
                }
                Err(_) => None,
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

#[derive(Serialize)]
struct MarketRefreshResp {
    refreshed_sources: usize,
    total_skills: usize,
}

/// POST /api/market/refresh — re-fetch every market source's skill index.
/// Any logged-in user can trigger; admin-only would be more conservative
/// but we let users pull fresh data themselves to avoid waiting on admin.
async fn api_market_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<MarketRefreshResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MarketRefreshResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_user(&headers, &db)?;
        let paths = AppPaths::default_path();
        let data_dir = paths.data_dir().to_path_buf();
        let sources = mkt::load_sources(&data_dir);
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        rt.block_on(refresh_all_sources(&sources, &data_dir));
        // Re-tally what we got back from the freshly-written caches.
        let total: usize = sources
            .iter()
            .filter_map(|s| mkt::load_cache(&data_dir, s).map(|v| v.len()))
            .sum();
        Ok(MarketRefreshResp {
            refreshed_sources: sources.len(),
            total_skills: total,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

/// Best-effort background enrich for freshly installed skills. Mirrors
/// `cli::spawn_targeted_enrich` — `recommend enrich --name N1 --name N2 ...`.
fn spawn_enrich(names: &[String]) {
    if names.is_empty() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("recommend").arg("enrich");
    for n in names {
        cmd.arg("--name").arg(n);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = cmd.spawn();
}

#[derive(Serialize)]
struct MarketSkillView {
    name: String,
    description: String,
    source_repo: String,
    /// "owner/repo" for sources defined as GitHub repos. None for non-GitHub.
    source_label: String,
    /// e.g. "skills/bolder" relative to source_repo root.
    repo_path: String,
    /// "main" or whatever branch the source was configured with.
    branch: String,
    installed: bool,
    in_my_library: bool,
    /// Popularity signals from the skills.sh leaderboard. Zero when unknown.
    installs: u64,
    trending_installs: u64,
    hot_score: u64,
    weekly_installs: Vec<u64>,
    is_official: bool,
}

#[derive(Serialize)]
struct MarketSourceStatus {
    label: String,
    owner: String,
    repo: String,
    cached_count: usize,
}

#[derive(Serialize)]
struct MarketListResp {
    /// Items in the current page (post-filter, post-sort, post-paging).
    items: Vec<MarketSkillView>,
    /// Total count of items matching `q` + `sort` BEFORE the offset/limit
    /// window. Drives the pager.
    total: usize,
    /// Window offset that produced `items`. Echoed for client convenience.
    offset: usize,
    /// Window size that produced `items`. Echoed for client convenience.
    limit: usize,
    /// True when zero sources have cache on disk. Frontend should fire
    /// `/api/market/refresh` in the background to warm things up.
    needs_refresh: bool,
    /// Per-source breakdown so the user can see e.g. "Vercel: 12 cached,
    /// Anthropic: 23 cached" instead of a single opaque total.
    sources: Vec<MarketSourceStatus>,
}

#[derive(Deserialize)]
struct MarketListQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// `all` (default) | `trending` | `hot` — sort the leaderboard server-side
    /// so the frontend doesn't need to ship every signal to do client-sorts.
    #[serde(default)]
    sort: Option<String>,
    /// Zero-based offset for pagination. Combine with `limit` to walk past
    /// the front of the leaderboard.
    #[serde(default)]
    offset: Option<usize>,
}

async fn api_market_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<MarketListQuery>,
) -> Result<Json<MarketListResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MarketListResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let paths = AppPaths::default_path();
        let data_dir = paths.data_dir().to_path_buf();
        let sources = mkt::load_sources(&data_dir);
        // Never block this endpoint on GitHub. Returns whatever's cached;
        // when nothing is cached the response carries `needs_refresh: true`
        // and the frontend auto-fires /api/market/refresh in the background.
        let cached_sources = sources
            .iter()
            .filter(|s| mkt::load_cache(&data_dir, s).is_some())
            .count();
        let needs_refresh = cached_sources == 0;
        let installed_names: std::collections::HashSet<String> = db
            .list_resources(Some(crate::core::resource::ResourceKind::Skill), None)
            .map_err(ApiError::Internal)?
            .into_iter()
            .map(|r| r.name)
            .collect();
        let in_library: std::collections::HashSet<String> = db
            .library_list(&user.user_id)
            .map_err(ApiError::Internal)?
            .into_iter()
            .collect();

        let q = query.q.unwrap_or_default().trim().to_lowercase();
        let limit = query.limit.unwrap_or(50).clamp(1, 500);
        let offset = query.offset.unwrap_or(0);
        let sort = query.sort.as_deref().unwrap_or("all");

        // Flatten every cached source into one searchable list. With the
        // skills.sh aggregator this is effectively the whole catalog;
        // user-added GitHub sources merge in.
        let mut all_skills: Vec<(String, mkt::MarketSkill)> = Vec::new();
        for source in &sources {
            let Some(skills) = mkt::load_cache(&data_dir, source) else {
                continue;
            };
            for s in skills {
                all_skills.push((source.label.clone(), s));
            }
        }

        // Filter on the typed query.
        let filtered: Vec<&(String, mkt::MarketSkill)> = all_skills
            .iter()
            .filter(|(_, s)| {
                if q.is_empty() {
                    return true;
                }
                s.name.to_lowercase().contains(&q)
                    || s.source_repo.to_lowercase().contains(&q)
            })
            .collect();

        // Sort matches the user-selected tab.
        let mut sorted: Vec<&(String, mkt::MarketSkill)> = filtered.clone();
        match sort {
            "trending" => sorted.sort_by(|a, b| {
                b.1.trending_installs
                    .cmp(&a.1.trending_installs)
                    .then(a.1.name.cmp(&b.1.name))
            }),
            "hot" => sorted.sort_by(|a, b| {
                b.1.hot_score
                    .cmp(&a.1.hot_score)
                    .then(a.1.name.cmp(&b.1.name))
            }),
            _ => sorted.sort_by(|a, b| {
                b.1.installs
                    .cmp(&a.1.installs)
                    .then(a.1.name.cmp(&b.1.name))
            }),
        }

        let total = sorted.len();
        let items: Vec<MarketSkillView> = sorted
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(label, s)| MarketSkillView {
                source_label: label.clone(),
                in_my_library: in_library.contains(&s.name),
                installed: installed_names.contains(&s.name),
                description: String::new(),
                name: s.name.clone(),
                source_repo: s.source_repo.clone(),
                repo_path: s.repo_path.clone(),
                branch: s.branch.clone(),
                installs: s.installs,
                trending_installs: s.trending_installs,
                hot_score: s.hot_score,
                weekly_installs: s.weekly_installs.clone(),
                is_official: s.is_official,
            })
            .collect();

        let source_status: Vec<MarketSourceStatus> = sources
            .iter()
            .map(|s| MarketSourceStatus {
                label: s.label.clone(),
                owner: s.owner.clone(),
                repo: s.repo.clone(),
                cached_count: mkt::load_cache(&data_dir, s).map_or(0, |v| v.len()),
            })
            .collect();
        Ok(MarketListResp {
            items,
            total,
            offset,
            limit,
            needs_refresh,
            sources: source_status,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct MarketPreviewQuery {
    /// e.g. "owner/repo"
    source_repo: String,
    /// e.g. "main"
    branch: String,
    /// e.g. "skills/bolder" — the skill dir inside the repo. Empty for
    /// skills.sh aggregator entries (the SKILL.md path inside the GitHub
    /// repo isn't known until we walk the tree); when empty, the server
    /// falls back to trying common layouts using `skill_name`.
    repo_path: String,
    /// Skill name. Required when `repo_path` is empty so the preview
    /// path candidates can be built (`<name>/SKILL.md`,
    /// `skills/<name>/SKILL.md`, etc.).
    #[serde(default)]
    skill_name: String,
}

#[derive(Serialize, Deserialize)]
struct MarketPreviewResp {
    name: String,
    /// First ~8 KB of SKILL.md. Empty when SKILL.md is missing or fetch failed.
    skill_md: String,
    /// Convenience hint when SKILL.md couldn't be fetched.
    error: Option<String>,
}

/// GET /api/market/preview?source_repo=owner/repo&branch=main&repo_path=skills/foo
/// Fetches the skill's SKILL.md from raw.githubusercontent.com and returns
/// up to 8 KB so the dashboard can preview before installing. Done server-side
/// so the browser doesn't hit GitHub directly (browsers may be on a network
/// that can't, and a server-side fetch can be cached / rate-limited later).
async fn api_market_preview(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<MarketPreviewQuery>,
) -> Result<Json<MarketPreviewResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MarketPreviewResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_user(&headers, &db)?;
        let parts: Vec<&str> = q.source_repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(ApiError::BadRequest(
                "source_repo must be 'owner/repo'".into(),
            ));
        }
        let owner = parts[0];
        let repo = parts[1];
        let branch = if q.branch.is_empty() { "main" } else { &q.branch };
        // Trim trailing slash; SKILL.md is appended.
        let path = q.repo_path.trim_end_matches('/');

        // Disk cache: SKILL.md previews live 1h on disk so repeat
        // clicks on the same row stay below 5ms instead of triggering
        // another mirror race. Cache hits (success or empty body) both
        // honor TTL; we keep failures only 5 min so a fluke network
        // blip doesn't poison the entry for an hour.
        let paths_cache = AppPaths::default_path();
        let cache_dir = paths_cache.data_dir().join("market-cache").join("preview-md");
        let _ = std::fs::create_dir_all(&cache_dir);
        let cache_key = format!(
            "{}__{}__{}__{}__{}.json",
            owner,
            repo,
            branch,
            path.replace('/', "_")
                .chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>(),
            q.skill_name.replace('/', "_")
                .chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>(),
        );
        let cache_path = cache_dir.join(cache_key);
        if let Ok(meta) = std::fs::metadata(&cache_path)
            && let Ok(modified) = meta.modified()
            && let Ok(age) = modified.elapsed()
            && let Ok(content) = std::fs::read_to_string(&cache_path)
            && let Ok(cached) = serde_json::from_str::<MarketPreviewResp>(&content)
        {
            let ttl = if cached.error.is_some() { 300 } else { 3600 };
            if age.as_secs() < ttl {
                return Ok(cached);
            }
        }
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_string();
        // Build candidate paths inside the repo. Each path expands
        // across multiple mirrors and we race them in parallel so the
        // user always sees the SKILL.md from the fastest reachable
        // mirror. Mirror order (informational; race winner decides):
        //   - raw.githubusercontent.com (direct)
        //   - ghfast.top                (China-friendly proxy, ~1s)
        //   - cdn.jsdelivr.net          (fastly CDN, ~2s)
        //   - cdn.jsdmirror.com         (jsdelivr China mirror, fallback)
        let raw_paths: Vec<String> = if !path.is_empty() {
            vec![format!("{path}/SKILL.md")]
        } else if !q.skill_name.is_empty() {
            let n = &q.skill_name;
            vec![
                format!("{n}/SKILL.md"),
                format!("skills/{n}/SKILL.md"),
                format!("agent-skills/{n}/SKILL.md"),
                "SKILL.md".to_string(),
            ]
        } else {
            vec!["SKILL.md".to_string()]
        };
        let mut candidates: Vec<String> = Vec::new();
        for p in &raw_paths {
            candidates.push(format!("https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{p}"));
            candidates.push(format!("https://ghfast.top/https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{p}"));
            candidates.push(format!("https://cdn.jsdelivr.net/gh/{owner}/{repo}@{branch}/{p}"));
            candidates.push(format!("https://cdn.jsdmirror.com/gh/{owner}/{repo}@{branch}/{p}"));
        }
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let result: anyhow::Result<String> = rt.block_on(async {
            let client = reqwest::Client::builder()
                .user_agent("runai/0.11 (+https://github.com/Crosery/runai)")
                .timeout(std::time::Duration::from_secs(6))
                .build()?;

            // Race every candidate URL concurrently — first one that
            // returns 200 with a body wins, the rest get aborted by
            // JoinSet drop. Saves the "raw.github slow → fall back"
            // chain latency. Records 404 / err per URL for diagnostics.
            let mut set = tokio::task::JoinSet::new();
            for url in candidates.iter().cloned() {
                let client = client.clone();
                set.spawn(async move {
                    let resp = client.get(&url).send().await;
                    match resp {
                        Ok(r) if r.status().is_success() => match r.text().await {
                            Ok(body) if !body.is_empty() => Ok((url, body)),
                            Ok(_) => Err((url, "empty body".to_string())),
                            Err(e) => Err((url, e.to_string())),
                        },
                        Ok(r) => Err((url, format!("HTTP {}", r.status()))),
                        Err(e) => Err((url, e.to_string())),
                    }
                });
            }
            let mut errs: Vec<String> = Vec::new();
            while let Some(res) = set.join_next().await {
                match res {
                    Ok(Ok((_url, body))) => {
                        // Abort siblings so they don't burn bandwidth
                        // after we already have a winner.
                        set.abort_all();
                        return Ok(body);
                    }
                    Ok(Err((url, e))) => errs.push(format!("{url} → {e}")),
                    Err(e) => errs.push(format!("join: {e}")),
                }
            }
            anyhow::bail!("SKILL.md unreachable on every mirror: {}", errs.join("; "));
        });
        let (skill_md, error) = match result {
            Ok(t) => (t.chars().take(8000).collect::<String>(), None),
            Err(e) => (String::new(), Some(e.to_string())),
        };
        let resp_out = MarketPreviewResp {
            name,
            skill_md,
            error,
        };
        let _ = std::fs::write(
            &cache_path,
            serde_json::to_string(&resp_out).unwrap_or_default(),
        );
        Ok(resp_out)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct MarketPreviewFilesQuery {
    /// e.g. "owner/repo"
    source_repo: String,
    /// Branch on GitHub. Defaults to "main".
    #[serde(default)]
    branch: String,
    /// e.g. "find-skills" — used to build layout candidates when repo_path is empty.
    skill_name: String,
    /// Optional pre-known path; when supplied, only that layout is tried.
    #[serde(default)]
    repo_path: String,
}

#[derive(Serialize, Deserialize)]
struct MarketPreviewFile {
    /// Path relative to the skill root, forward-slashes.
    path: String,
    size: u64,
    is_dir: bool,
}

#[derive(Serialize, Deserialize)]
struct MarketPreviewFilesResp {
    /// Path inside the GitHub repo that produced this listing (the layout
    /// that hit). Empty when nothing matched.
    matched_path: String,
    entries: Vec<MarketPreviewFile>,
    error: Option<String>,
}

/// GET /api/market/preview-files?source_repo=&branch=&skill_name=
/// Lists the contents of a skill's GitHub directory via the unauthenticated
/// GitHub Contents API. Used by the Market detail modal to show what
/// sibling files (scripts/, references/, etc.) the skill ships with.
/// Tries the same `<n>/SKILL.md` / `skills/<n>/SKILL.md` / `agent-skills/<n>/SKILL.md`
/// layout chain as `api_market_preview`, stopping at the first 200.
async fn api_market_preview_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<MarketPreviewFilesQuery>,
) -> Result<Json<MarketPreviewFilesResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MarketPreviewFilesResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_user(&headers, &db)?;
        let parts: Vec<&str> = q.source_repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(ApiError::BadRequest("source_repo must be 'owner/repo'".into()));
        }
        let owner = parts[0];
        let repo = parts[1];
        let branch = if q.branch.is_empty() { "main" } else { &q.branch };
        let candidates: Vec<String> = if !q.repo_path.is_empty() {
            vec![q.repo_path.trim_matches('/').to_string()]
        } else if !q.skill_name.is_empty() {
            let n = &q.skill_name;
            // Last entry is "" → root-skill repos where SKILL.md
            // sits at the repository root (e.g. anysearch-ai/anysearch-skill).
            vec![
                n.clone(),
                format!("skills/{n}"),
                format!("agent-skills/{n}"),
                String::new(),
            ]
        } else {
            vec![String::new()]
        };

        // Disk cache lookup — preview-files results live 1h on disk so
        // repeat clicks on the same skill don't burn the GitHub API
        // budget (60 req/h unauth, 5000/h with GITHUB_TOKEN).
        let paths = AppPaths::default_path();
        let cache_dir = paths.data_dir().join("market-cache").join("preview-files");
        let _ = std::fs::create_dir_all(&cache_dir);
        let cache_key = format!(
            "{}__{}__{}__{}.json",
            owner,
            repo,
            branch,
            q.repo_path.replace('/', "_")
                .chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        );
        let cache_path = cache_dir.join(cache_key);
        if let Ok(meta) = std::fs::metadata(&cache_path)
            && let Ok(modified) = meta.modified()
            && let Ok(age) = modified.elapsed()
            && let Ok(content) = std::fs::read_to_string(&cache_path)
            && let Ok(cached) = serde_json::from_str::<MarketPreviewFilesResp>(&content)
        {
            // Success cached 1h; failures cached 5min so a one-time
            // rate-limit / 404 doesn't keep retrying every click.
            let ttl = if cached.error.is_some() { 300 } else { 3600 };
            if age.as_secs() < ttl {
                return Ok(cached);
            }
        }

        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let result: anyhow::Result<(String, Vec<MarketPreviewFile>)> = rt.block_on(async {
            let client = reqwest::Client::builder()
                .user_agent("runai/0.11 (+https://github.com/Crosery/runai)")
                .timeout(std::time::Duration::from_secs(12))
                .build()?;

            // Primary: jsdelivr fastly endpoint (data.jsdelivr.com — the
            // .net mirror has flaky DNS on some networks). One repo-wide
            // tree fetch, no auth, no rate limit. Walk in-memory to find
            // the matching skill directory.
            let jsd_url = format!(
                "https://data.jsdelivr.com/v1/packages/gh/{owner}/{repo}@{branch}?structure=tree"
            );
            let jsd_err: String = match client
                .get(&jsd_url)
                .header("accept", "application/json")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let body: serde_json::Value = r.json().await?;
                    let root_files = body
                        .get("files")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    fn find_dir<'a>(
                        nodes: &'a [serde_json::Value],
                        want: &[&str],
                    ) -> Option<&'a [serde_json::Value]> {
                        if want.is_empty() {
                            return None;
                        }
                        for n in nodes {
                            if n.get("type").and_then(|v| v.as_str()) != Some("directory") {
                                continue;
                            }
                            let name = n.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            if name == want[0] {
                                if want.len() == 1 {
                                    return n.get("files").and_then(|v| v.as_array()).map(|a| a.as_slice());
                                }
                                if let Some(child) = n.get("files").and_then(|v| v.as_array())
                                    && let Some(hit) = find_dir(child, &want[1..])
                                {
                                    return Some(hit);
                                }
                            }
                        }
                        None
                    }
                    for path in &candidates {
                        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                        // Empty segs = repo root listing. Only return
                        // root when there's a SKILL.md sitting at the
                        // top — otherwise we'd surface a generic monorepo
                        // root for a non-root-skill query.
                        let dir_files: &[serde_json::Value] = if segs.is_empty() {
                            let root_has_skill_md = root_files.iter().any(|n| {
                                n.get("type").and_then(|v| v.as_str()) == Some("file")
                                    && n.get("name").and_then(|v| v.as_str()) == Some("SKILL.md")
                            });
                            if !root_has_skill_md { continue; }
                            &root_files
                        } else {
                            match find_dir(&root_files, &segs) {
                                Some(v) => v,
                                None => continue,
                            }
                        };
                        let mut entries: Vec<MarketPreviewFile> = dir_files
                            .iter()
                            .filter_map(|n| {
                                let name = n.get("name").and_then(|v| v.as_str())?.to_string();
                                let is_dir = n.get("type").and_then(|v| v.as_str()) == Some("directory");
                                let size = n.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                                // For root listings, hide the housekeeping noise.
                                if segs.is_empty()
                                    && !crate::core::market::is_root_skill_payload(&name)
                                {
                                    return None;
                                }
                                Some(MarketPreviewFile { path: name, size, is_dir })
                            })
                            .collect();
                        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.path.cmp(&b.path)));
                        return Ok((path.clone(), entries));
                    }
                    format!("jsdelivr tree had no matching dir for {candidates:?}")
                }
                Ok(r) => format!("jsdelivr HTTP {}", r.status()),
                Err(e) => format!("jsdelivr request: {e}"),
            };

            // Fallback: GitHub Contents API. Unauthenticated 60/h limit;
            // bumps to 5000/h when GITHUB_TOKEN env is set on the server.
            let token = std::env::var("GITHUB_TOKEN").ok();
            let mut last_err: Option<String> = Some(format!("jsdelivr → {jsd_err}; github →"));
            for path in &candidates {
                let url = if path.is_empty() {
                    format!("https://api.github.com/repos/{owner}/{repo}/contents?ref={branch}")
                } else {
                    format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}?ref={branch}")
                };
                let mut req = client.get(&url).header("accept", "application/vnd.github+json");
                if let Some(t) = &token {
                    req = req.header("authorization", format!("Bearer {t}"));
                }
                let resp = req.send().await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let arr: serde_json::Value = r.json().await?;
                        let mut entries: Vec<MarketPreviewFile> = Vec::new();
                        if let Some(items) = arr.as_array() {
                            for item in items {
                                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("file");
                                let size = item.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                                if name.is_empty() { continue; }
                                entries.push(MarketPreviewFile {
                                    path: name.to_string(),
                                    size,
                                    is_dir: kind == "dir",
                                });
                            }
                        }
                        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.path.cmp(&b.path)));
                        return Ok((path.clone(), entries));
                    }
                    Ok(r) if r.status().as_u16() == 403 => {
                        anyhow::bail!("{} GitHub 403 (rate-limited)", last_err.unwrap_or_default());
                    }
                    Ok(r) if r.status().as_u16() == 404 => {
                        last_err = Some(format!("{} 404 at {path:?}", last_err.unwrap_or_default()));
                    }
                    Ok(r) => {
                        last_err = Some(format!("{} HTTP {} at {path:?}", last_err.unwrap_or_default(), r.status()));
                    }
                    Err(e) => {
                        last_err = Some(format!("{} {e}", last_err.unwrap_or_default()));
                    }
                }
            }
            anyhow::bail!("file listing failed: {}", last_err.unwrap_or_default());
        });
        let (matched_path, entries, error) = match result {
            Ok((p, e)) => (p, e, None),
            Err(e) => (String::new(), Vec::new(), Some(e.to_string())),
        };
        let resp_out = MarketPreviewFilesResp { matched_path, entries, error };
        // Cache successful results (and short-lived 404s) to keep the
        // 60-req/h GitHub API budget from getting blown.
        let _ = std::fs::write(
            &cache_path,
            serde_json::to_string(&resp_out).unwrap_or_default(),
        );
        Ok(resp_out)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct MarketInstallReq {
    name: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Serialize)]
struct InstallResp {
    installed: Vec<String>,
    library_size: usize,
}

async fn api_market_install(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<MarketInstallReq>,
) -> Result<Json<InstallResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<InstallResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let mgr = SkillManager::new().map_err(ApiError::Internal)?;
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let sources = mkt::load_sources(&data_dir);
        let skill = mkt::find_skill_in_sources(&data_dir, &sources, &req.name, req.source.as_deref())
            .ok_or_else(|| ApiError::BadRequest(format!("skill '{}' not found in market", req.name)))?;
        let skill_name = skill.name.clone();
        // Phase D: market install through the dashboard lands in the
        // user's private pool (`<data>/users/<uid>/skills/<name>/`), not
        // the shared public pool. Symlink registration is skipped — a
        // remote dashboard user runs no local Claude Code that would
        // consume those symlinks; they pull skills via /skills/get + /skills/file.
        mgr.paths()
            .ensure_user_dirs(&user.user_id)
            .map_err(ApiError::Internal)?;

        // skills.sh aggregator: MarketSkill carries the real GitHub repo
        // in `source_repo` but no `repo_path` (skill location inside the
        // repo is unknown until we fetch the tree). Re-route through
        // install_github_repo_filtered_for so the install path fetches
        // the tree, locates the skill dir, and downloads the whole
        // bundle into the user's private pool.
        if skill.source_label == "skills.sh" {
            let (owner_part, repo_part) = match skill.source_repo.split_once('/') {
                Some((o, r)) => (o.to_string(), r.to_string()),
                None => return Err(ApiError::Internal(anyhow::anyhow!(
                    "malformed source_repo {:?} for skills.sh entry",
                    skill.source_repo
                ))),
            };
            let filter = vec![skill_name.clone()];
            let _ = mgr
                .install_github_repo_filtered_for(
                    &owner_part,
                    &repo_part,
                    &skill.branch,
                    CliTarget::Claude,
                    Some(&filter),
                    Some(&user.user_id),
                )
                .map_err(ApiError::Internal)?;
            let _ = db.library_add(&user.user_id, &skill_name);
            spawn_enrich(&[skill_name.clone()]);
            let size = db.library_count(&user.user_id).unwrap_or(0);
            return Ok(InstallResp {
                installed: vec![skill_name],
                library_size: size,
            });
        }

        let install_root = mgr
            .paths()
            .user_skills_dir(&user.user_id)
            .map_err(ApiError::Internal)?;
        let rt = tokio::runtime::Runtime::new().map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        rt.block_on(mkt::Market::install_single(&skill, &install_root))
            .map_err(ApiError::Internal)?;
        let _ = mgr.register_local_skill_for(&skill_name, Some(&user.user_id));
        // Auto-subscribe to installing user's library.
        let _ = db.library_add(&user.user_id, &skill_name);
        spawn_enrich(&[skill_name.clone()]);
        let size = db.library_count(&user.user_id).unwrap_or(0);
        Ok(InstallResp {
            installed: vec![skill_name],
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
struct GithubInstallReq {
    /// Either "owner/repo" or "owner/repo@branch" or a full GitHub URL.
    /// We accept any of those shapes for paste convenience.
    source: String,
    #[serde(default)]
    branch: Option<String>,
    /// Optional whitelist of skill names to install. None / empty = all.
    /// Used by the dashboard "parse → user picks subset → install" flow.
    #[serde(default)]
    skills: Option<Vec<String>>,
}

#[derive(Serialize)]
struct ParseSkillView {
    name: String,
    repo_path: String,
    /// Already in resources table (someone else installed it before).
    already_installed: bool,
    /// Already in this user's library.
    in_my_library: bool,
}

#[derive(Serialize)]
struct ParseGithubResp {
    owner: String,
    repo: String,
    branch: String,
    plugin_detected: bool,
    skills: Vec<ParseSkillView>,
}

/// POST /api/parse/github  body: {source}
/// Discovers what skills exist inside a GitHub repo WITHOUT installing
/// anything. Returns a list the dashboard renders as checkboxes so the
/// user can pick which ones to import.
async fn api_parse_github(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<GithubInstallReq>,
) -> Result<Json<ParseGithubResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<ParseGithubResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let raw = req
            .source
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/');
        let (repo_part, parsed_branch) = if raw.contains('@') {
            let parts: Vec<&str> = raw.splitn(2, '@').collect();
            (parts[0], parts[1].to_string())
        } else {
            (raw, "main".to_string())
        };
        let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(ApiError::BadRequest(
                "expected 'owner/repo' or 'owner/repo@branch' or a full GitHub URL".into(),
            ));
        }
        let branch = req.branch.unwrap_or(parsed_branch);
        let owner = parts[0].to_string();
        let repo = parts[1].to_string();

        let source = mkt::SourceEntry::from_input(&format!("{}/{}@{}", owner, repo, branch))
            .map_err(ApiError::Internal)?;
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let extract = rt
            .block_on(mkt::Market::fetch(&source))
            .map_err(|e| ApiError::BadRequest(format!("无法解析仓库: {e}")))?;

        let installed_names: std::collections::HashSet<String> = db
            .list_resources(Some(crate::core::resource::ResourceKind::Skill), None)
            .map_err(ApiError::Internal)?
            .into_iter()
            .map(|r| r.name)
            .collect();
        let in_library: std::collections::HashSet<String> = db
            .library_list(&user.user_id)
            .map_err(ApiError::Internal)?
            .into_iter()
            .collect();

        let skills = extract
            .skills
            .into_iter()
            .map(|s| ParseSkillView {
                already_installed: installed_names.contains(&s.name),
                in_my_library: in_library.contains(&s.name),
                name: s.name,
                repo_path: s.repo_path,
            })
            .collect();
        Ok(ParseGithubResp {
            owner,
            repo,
            branch,
            plugin_detected: extract.plugin_detected,
            skills,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

async fn api_install_github(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<GithubInstallReq>,
) -> Result<Json<InstallResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<InstallResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let raw = req
            .source
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/');
        let (repo_part, parsed_branch) = if raw.contains('@') {
            let parts: Vec<&str> = raw.splitn(2, '@').collect();
            (parts[0], parts[1].to_string())
        } else {
            (raw, "main".to_string())
        };
        let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(ApiError::BadRequest(
                "expected 'owner/repo' or 'owner/repo@branch'".into(),
            ));
        }
        let branch = req.branch.unwrap_or(parsed_branch);
        let mgr = SkillManager::new().map_err(ApiError::Internal)?;
        let filter: Option<Vec<String>> = req.skills.filter(|v| !v.is_empty());
        // Phase D: dashboard-driven github install goes to the
        // authenticated user's private pool.
        let (_group, names) = mgr
            .install_github_repo_filtered_for(
                parts[0],
                parts[1],
                &branch,
                CliTarget::Claude,
                filter.as_deref(),
                Some(&user.user_id),
            )
            .map_err(ApiError::Internal)?;
        for name in &names {
            let _ = db.library_add(&user.user_id, name);
        }
        spawn_enrich(&names);
        let size = db.library_count(&user.user_id).unwrap_or(0);
        Ok(InstallResp {
            installed: names,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

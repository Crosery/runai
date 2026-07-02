//! Router telemetry endpoints: summary / events / timeline / event-by-id.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::db::RouterEvent;

use super::error::ApiError;
use super::state::{AppState, resolve_view_user};

#[derive(Deserialize)]
pub(super) struct EventsQuery {
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
pub(super) struct PerModel {
    model: String,
    calls: i64,
    total_tokens: i64,
}

#[derive(Serialize)]
pub(super) struct SummaryResponse {
    total: i64,
    hits: i64,
    errors: i64,
    hit_rate: f64,
    avg_latency_ms: Option<f64>,
    avg_prompt_tokens: f64,
    total_tokens: i64,
    per_model: Vec<PerModel>,
}

pub(super) async fn api_summary(
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
pub(super) struct EventsResponse {
    total: i64,
    events: Vec<EventJson>,
}

#[derive(Serialize)]
pub(super) struct EventJson {
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
    authenticated: bool,
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
            authenticated: e.user_id.is_some(),
            error_msg: e.error_msg,
            llm_raw_response: e.llm_raw_response,
            hook_output: e.hook_output,
            llm_input: e.llm_input,
            injected,
        }
    }
}

pub(super) async fn api_events(
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
    let events = db.router_events_paged_filtered(
        since,
        limit,
        offset,
        model_ref,
        hit_only,
        view.as_deref(),
    )?;
    let total = db.router_events_count_filtered(since, model_ref, hit_only, view.as_deref())?;
    Ok(Json(EventsResponse {
        total,
        events: events.into_iter().map(EventJson::from).collect(),
    }))
}

#[derive(Deserialize)]
pub(super) struct TimelineQuery {
    /// Window length in hours. 24 -> 24 hourly buckets; 6 -> 6 hourly buckets.
    hours: Option<i64>,
    /// Optional bucket width override in seconds. Default = hours * 3600 / 24
    /// (so 24h -> hourly, 6h -> 15min, etc), capped to keep the chart legible.
    bucket_secs: Option<i64>,
    /// Same semantics as `EventsQuery::user`.
    user: Option<String>,
}

#[derive(Serialize)]
pub(super) struct TimelinePoint {
    ts_start: i64,
    total: i64,
    hits: i64,
    errors: i64,
    avg_latency_ms: f64,
}

#[derive(Serialize)]
pub(super) struct TimelineResponse {
    bucket_secs: i64,
    points: Vec<TimelinePoint>,
}

pub(super) async fn api_timeline(
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

pub(super) async fn api_event_by_id(
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

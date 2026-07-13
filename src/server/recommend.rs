//! Remote-hook endpoints: `/recommend` (the remote skill router) and
//! `/feedback`, plus the `guess_server_url` URL-reconstruction helper.

use anyhow::Result;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::auth as authmod;
use crate::core::manager::SkillManager;
use crate::core::recommend;
use crate::core::recommend::local_ipv4;

use super::state::{AppState, require_user};

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
/// Body: hook JSON (fields used: `prompt`, `session_id`, `runai_session_id`,
/// `client_kind`, `cwd`, `transcript_path`). A request with a non-empty
/// `query` uses the bounded presentation index and returns JSON without an
/// LLM call; the original `prompt` protocol remains unchanged.
///
/// Optional `X-Runai-User: {user}@{host}` header scopes the native session key
/// before deriving the opaque `rnai_sess_*` id, so multiple teammates' sessions
/// do not collide in router memory. The install script writes this header
/// automatically; manual callers can omit it.
///
/// Returns the hook output string (markdown to be injected into the
/// teammate's Claude Code prompt) as plain text. Errors fall through to
/// 200 + empty body — the install script's `--max-time 30 || true`
/// pattern means a server hiccup never blocks the teammate's prompt.
pub(super) async fn handle_recommend(
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> Response {
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

    // Extract auth before we hop to blocking. Bearer identifies which
    // user_id scopes the recommendation. Absence is the legacy anonymous
    // compat lane; a malformed / stale Bearer must fail closed instead of
    // silently becoming anonymous, or per-user prompt-injection prefs are
    // bypassed after a browser login rotates the api_key.
    let auth_header_present = headers.contains_key(header::AUTHORIZATION);
    let bearer_hash = {
        let bearer = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        authmod::parse_bearer_header(bearer).map(|tok| authmod::key_hash(&tok))
    };
    if auth_header_present && bearer_hash.is_none() {
        return ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "").into_response();
    }

    let query = payload_str(&payload, "query");
    if !query.is_empty() {
        let query_for_index = query.clone();
        let payload_for_index = payload.clone();
        let bearer_hash_for_index = bearer_hash.clone();
        let user_prefix_for_index = user_prefix.clone();
        let join = tokio::task::spawn_blocking(
            move || -> Result<Option<(Vec<QuickCandidate>, Option<String>)>> {
                let mgr = SkillManager::new()?;
                let user_id_opt = match bearer_hash_for_index.as_deref() {
                    Some(hash) => match mgr.db().find_user_by_api_key_hash(hash).ok().flatten() {
                        Some(user) if !user.disabled => Some(user.user_id),
                        _ => return Ok(None),
                    },
                    None => None,
                };
                let host_kind = payload_host_kind(&payload_for_index);
                let native_sid = payload_native_session_id(&payload_for_index);
                let runai_session_id = scoped_runai_session_id(
                    user_id_opt.as_deref(),
                    &user_prefix_for_index,
                    &host_kind,
                    &native_sid,
                );
                let resources = query_resources_for_user(&mgr, user_id_opt.as_deref())?;
                let entries = resources
                    .iter()
                    .map(|resource| (resource.name.as_str(), resource.description.as_str()))
                    .collect::<Vec<_>>();
                Ok(Some((
                    quick_candidates_from_entries(&query_for_index, &entries),
                    runai_session_id,
                )))
            },
        )
        .await;
        let result = match join {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                eprintln!("/recommend query index failed: {error:#}");
                Some((Vec::new(), None))
            }
            Err(error) => {
                eprintln!("/recommend query index join failed: {error}");
                Some((Vec::new(), None))
            }
        };
        let Some((candidates, runai_session_id)) = result else {
            return ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], "").into_response();
        };
        return (
            StatusCode::OK,
            Json(serde_json::json!({
                "protocol": "runai-recommend-query-v1",
                "query": query,
                "router": "deterministic-local-index",
                "runai_session_id": runai_session_id,
                "candidates": candidates,
            })),
        )
            .into_response();
    }

    // recommend() is blocking (reqwest::blocking + rusqlite). Hop onto a
    // blocking thread so the async runtime stays responsive.
    let join = tokio::task::spawn_blocking(move || -> Result<String> {
        let mgr = SkillManager::new()?;

        // Resolve user_id from bearer hash. No header keeps the legacy
        // anonymous path. Unknown / disabled Bearer means the client is
        // stale or invalid, so return empty hook output without routing.
        let user_id_opt: Option<String> = match bearer_hash.as_deref() {
            Some(h) => match mgr.db().find_user_by_api_key_hash(h).ok().flatten() {
                Some(u) if !u.disabled => Some(u.user_id),
                _ => return Ok(String::new()),
            },
            None => None,
        };

        let prompt = payload_str(&payload, "prompt");
        if prompt.is_empty() {
            return Ok(String::new());
        }
        let cwd = payload_str(&payload, "cwd");
        let transcript = payload_str(&payload, "transcript_path");
        let native_sid = payload_native_session_id(&payload);
        let host_kind = payload_host_kind(&payload);
        let sid_string = scoped_runai_session_id(
            user_id_opt.as_deref(),
            &user_prefix,
            &host_kind,
            &native_sid,
        );

        let tpath_pb = if transcript.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(&transcript))
        };
        let sid_opt = sid_string.as_deref();
        let cwd_opt = if cwd.is_empty() {
            None
        } else {
            Some(cwd.as_str())
        };

        let decision = recommend::recommend_for_user_with_client(
            &mgr,
            &prompt,
            tpath_pb.as_deref(),
            sid_opt,
            cwd_opt,
            user_id_opt.as_deref(),
            Some(&host_kind),
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
        if let Some(uid) = user_id_opt.as_deref()
            && let Ok(Some(user)) = mgr.db().find_user_by_id(uid)
        {
            let p = crate::core::prefs::UserPrefs::from_json_str(&user.prefs_json);
            cfg.skip_reminder_enabled = p.skip_reminder_enabled;
            if !p.skip_reminder_template.is_empty() {
                cfg.skip_reminder_template = p.skip_reminder_template;
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

fn payload_native_session_id(payload: &serde_json::Value) -> String {
    let runai_session_id = payload_str(payload, "runai_session_id");
    if runai_session_id.is_empty() {
        payload_str(payload, "session_id")
    } else {
        runai_session_id
    }
}

fn payload_host_kind(payload: &serde_json::Value) -> String {
    let client_kind = payload_str(payload, "client_kind");
    if client_kind.is_empty() {
        payload_str(payload, "host_kind")
    } else {
        client_kind
    }
}

fn scoped_runai_session_id(
    user_id: Option<&str>,
    user_prefix: &str,
    host_kind: &str,
    native_session_id: &str,
) -> Option<String> {
    let scope = match (user_id, user_prefix.is_empty(), host_kind.is_empty()) {
        (Some(uid), _, false) => format!("user:{uid}:host:{host_kind}"),
        (Some(uid), _, true) => format!("user:{uid}"),
        (None, false, false) => format!("header:{user_prefix}:host:{host_kind}"),
        (None, false, true) => format!("header:{user_prefix}"),
        (None, true, false) => format!("anon:host:{host_kind}"),
        (None, true, true) => "anon".to_string(),
    };
    crate::core::recommend::runai_session_id_from_native(Some(&scope), native_session_id)
}

fn query_resources_for_user(
    mgr: &SkillManager,
    user_id: Option<&str>,
) -> Result<Vec<crate::core::resource::Resource>> {
    use crate::core::prefs::UserPrefs;
    use crate::core::resource::ResourceKind;
    use std::collections::{BTreeSet, HashSet};

    let db = mgr.db();
    let mut resources = match user_id {
        Some(uid) => {
            let prefs = db
                .find_user_by_id(uid)?
                .map(|user| UserPrefs::from_json_str(&user.prefs_json))
                .unwrap_or_default();
            if !prefs.recommend_enabled {
                return Ok(Vec::new());
            }
            let visible = db.list_resources_for_user(Some(ResourceKind::Skill), Some(uid))?;
            if prefs.allow_public_recommend {
                visible
            } else {
                let library = db.library_list(uid)?.into_iter().collect::<BTreeSet<_>>();
                visible
                    .into_iter()
                    .filter(|resource| {
                        resource.owner_user_id.as_deref() == Some(uid)
                            || library.contains(&resource.name)
                    })
                    .collect()
            }
        }
        None => db.list_resources_for_user(Some(ResourceKind::Skill), None)?,
    };

    resources.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| {
                right
                    .owner_user_id
                    .is_some()
                    .cmp(&left.owner_user_id.is_some())
            })
            .then_with(|| right.installed_at.cmp(&left.installed_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut seen = HashSet::new();
    resources.retain(|resource| seen.insert(resource.name.clone()));
    Ok(resources)
}

#[derive(Serialize)]
struct QuickCandidate {
    name: String,
    description: String,
}

/// Bounded deterministic compatibility lane. General routing remains on the
/// existing `prompt` protocol so this path never turns every Pi prompt into an
/// LLM-blocking request.
fn quick_candidates_from_entries(query: &str, entries: &[(&str, &str)]) -> Vec<QuickCandidate> {
    let query = query.to_lowercase();
    let presentation_query = [
        "ppt",
        "powerpoint",
        "presentation",
        "slide",
        "deck",
        "演示",
        "幻灯",
    ]
    .iter()
    .any(|term| query.contains(term));
    if !presentation_query {
        return Vec::new();
    }

    let mut scored = entries
        .iter()
        .filter_map(|(name, description)| {
            let name_lower = name.to_lowercase();
            let haystack = format!("{name_lower} {}", description.to_lowercase());
            let score = [
                "ppt",
                "powerpoint",
                "presentation",
                "slide",
                "deck",
                "演示",
                "幻灯",
            ]
            .iter()
            .enumerate()
            .filter_map(|(index, term)| {
                if name_lower.contains(term) {
                    Some(200 - index as i32)
                } else {
                    haystack.contains(term).then_some(100 - index as i32)
                }
            })
            .max()
            .unwrap_or(0);
            (score > 0).then(|| (score, name, description))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
    scored
        .into_iter()
        .take(3)
        .map(|(_, name, description)| QuickCandidate {
            name: name.to_string(),
            description: description.to_string(),
        })
        .collect()
}

/// Reconstruct the server URL clients should use. Strategy:
///   - Use the request's `Host` header normally.
///   - But if Host is a loopback (`127.0.0.1` / `localhost` / `[::1]`),
///     substitute the machine's real LAN IPv4 — the rendered URL has to
///     be reachable by teammates and Claude Code agents, not just by
///     processes on the same box. A loopback URL leaks out only because
///     curl came in over loopback (local healthcheck / agent on same
///     host); we want the LAN form regardless.
pub(super) fn guess_server_url(headers: &HeaderMap) -> String {
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
    if is_loopback && let Some(ip) = local_ipv4() {
        return format!("{scheme}://{ip}:{port_part}");
    }
    format!("{scheme}://{host}")
}

/// Setup-page variant of `guess_server_url`. Returns the request origin
/// verbatim (scheme + Host header) — does NOT translate loopback to LAN
/// IP, because the user reading the Setup tab IS the curl client. They
/// got to the page via `127.0.0.1:17888`, so showing them
/// `curl http://192.168.0.93:17888/install | bash` would fail when the
/// server is bound to `127.0.0.1` (LAN IP isn't listening). Use Host
/// header as-is: whatever URL got them here will work for the snippets
/// they're about to copy-paste back into their own terminal.
///
/// `install.rs` keeps using `guess_server_url` because the install
/// script is the OUTPUT — meant for OTHER machines that need a routable
/// URL, not loopback. Setup is the INPUT view; loopback is fine because
/// the reader is already on the loopback.
pub(super) fn request_origin(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("http");
    let host = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1:17888");
    format!("{scheme}://{host}")
}

/// `verdict` accepts either a bare `+1`/`-1` or the strings `"good"`/`"bad"`
/// (case-insensitive). The dashboard's thumbs-up/down buttons send the
/// numeric form; the string form exists for script callers. Anything else
/// (`0`, `"meh"`, floats, booleans, ...) is not a valid verdict.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum VerdictInput {
    Num(i64),
    Str(String),
}

impl VerdictInput {
    fn normalize(&self) -> Option<i64> {
        match self {
            VerdictInput::Num(1) => Some(1),
            VerdictInput::Num(-1) => Some(-1),
            VerdictInput::Str(s) if s.eq_ignore_ascii_case("good") => Some(1),
            VerdictInput::Str(s) if s.eq_ignore_ascii_case("bad") => Some(-1),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
pub(super) struct FeedbackBody {
    skill: String,
    /// `#[serde(default)]` so a verdict-only body (no `note` key at all)
    /// still deserializes — the pre-verdict wire contract always required
    /// this field, so every legacy caller already sends it and is
    /// unaffected.
    #[serde(default)]
    note: String,
    /// Structured skill-feedback-radar verdict (new). `None` = legacy
    /// note-only request, which runs the exact pre-existing code path.
    #[serde(default)]
    verdict: Option<VerdictInput>,
    /// Which `router_events.id` this verdict is about, if any.
    #[serde(default)]
    event_id: Option<i64>,
    /// Session this verdict was given in, if any (opaque `rnai_sess_*`,
    /// not validated here — `skill_feedback` is an append-only log, not a
    /// trust boundary).
    #[serde(default)]
    session_id: Option<String>,
}

/// Wire ack for every successful `/feedback` call (verdict-only, note-only
/// legacy body, or verdict+note). `message` always contains
/// `"feedback applied by {username}: {skill}"` so pre-existing callers that
/// substring-match that phrase (dashboard + `runai-client`) keep working
/// unchanged even though the shape is now JSON, not the old bare
/// `text/plain` sentence.
#[derive(Serialize)]
struct FeedbackAck {
    ok: bool,
    skill: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    verdict: Option<i64>,
    /// `"queued"` — this call just claimed the re-enrich slot and spawned
    /// it. `"already-running"` — another vote on the same skill already has
    /// a re-enrich in flight (see `enrich_state::try_claim`); this vote was
    /// still recorded, just without spawning a duplicate LLM call.
    reenrich: &'static str,
    message: String,
}

/// Fire-and-forget re-enrich after ANY successfully-recorded feedback
/// (verdict-only, verdict+note, or the legacy note-only body) — closes the
/// loop for agent-driven feedback (`runai-client feedback`) the same way a
/// dashboard vote does. Runs `reevaluate_skill` on a detached OS thread, NOT
/// awaited and NOT tied to the request's response, so a slow or failing LLM
/// call never holds the HTTP connection open.
///
/// Callers MUST have already won `enrich_state::try_claim(skill)` before
/// calling this — the claim is what makes the "already-running" vs
/// "queued" response accurate and stops two concurrent votes on the same
/// skill from double-spending LLM calls.
///
/// On failure this only logs via `tracing::warn!` (spawn_enrich's "永不吞
/// 日志" rule) — the in-flight mark is left in place to expire via
/// `enrich_state`'s TTL rather than being cleared, so a transient failure
/// doesn't silently downgrade the dashboard tag back to 已富集/未富集 while
/// a human still believes a re-enrich was queued.
fn spawn_reevaluate(base_dir: std::path::PathBuf, skill: String, note: String) {
    std::thread::spawn(move || {
        let mgr = match SkillManager::with_base(base_dir) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    "feedback reevaluate: SkillManager::with_base failed for {skill}: {e:#}"
                );
                return;
            }
        };
        if let Err(e) = recommend::reevaluate_skill(&mgr, &skill, &note) {
            tracing::warn!("feedback reevaluate failed for {skill}: {e:#}");
        }
    });
}

/// POST /feedback — replaces `runai recommend feedback`.
/// Body: `{"skill":"...","note":"..."}` (legacy) or `{"skill":"...",
/// "verdict":1|-1|"good"|"bad","note":"...","event_id":123,"session_id":"..."}`
/// (skill-feedback-radar).
///
/// Requires auth (Bearer or session cookie) — issue #26: this endpoint
/// eventually triggers a real LLM call (`reevaluate_skill`) and rewrites a
/// skill's AI summary/score, so it must be gated the same way as every
/// other write endpoint (`require_user`). An anonymous caller gets `401`
/// with a completely EMPTY body (not `ApiError::Unauthorized`'s JSON
/// shape) to match the anti-enumeration style used by `empty_404` / the
/// `/recommend` fail-closed-Bearer lane — no distinguishing "auth
/// required" text for a probe to key off of.
///
/// The "applied by" attribution is now always the AUTHENTICATED username.
/// The legacy `X-Runai-User` header is no longer read here at all — it was
/// fully client-controlled and trivially forgeable, so trusting it for the
/// audit-trail field would let any anonymous caller impersonate anyone in
/// the response text.
///
/// **Re-enrich is always asynchronous.** Every request that records
/// feedback (verdict-only, note-only, or both) claims the skill's
/// `enrich_state` in-flight slot (`enrich_state::try_claim`) and spawns
/// `reevaluate_skill` on a detached thread via `spawn_reevaluate` — the
/// HTTP response returns immediately, before the LLM call starts. The
/// response's `reenrich` field is `"queued"` (this call won the claim) or
/// `"already-running"` (another vote's re-enrich is still in flight for
/// this skill, so this vote was recorded but did not spawn a duplicate LLM
/// call). A synchronous existence check still runs first so a missing
/// skill fails fast (404 empty body for the verdict path, 400 text/plain
/// for the legacy note-only path — see the invariant in this folder's
/// `AGENTS.md`) instead of silently queuing a re-enrich that can never
/// resolve.
///
/// Idempotency (PLANNING §1.3, optional): when the request carries an
/// `X-Runai-Event-Id` header, the re-enrich TRIGGER is gated on the
/// `usage_events` table (kind=`feedback`). First → re-enrich is queued;
/// Duplicate (same id + same payload hash) → 200 no-op (no second
/// re-enrich); Conflict (same id, different hash) → 409. When the header
/// is absent, the legacy non-idempotent path runs unchanged so
/// pre-protocol callers (and the existing feedback_auth_e2e suite) keep
/// working. The `runai-client` companion always sends the header, so the
/// protocol lane is the live one for new deploys.
pub(super) async fn handle_feedback(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    use crate::core::db::UsageOutcome;
    let user = {
        let db = match state.db() {
            Ok(db) => db,
            Err(e) => {
                eprintln!("/feedback: db open failed: {e:#}");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "internal error\n",
                )
                    .into_response();
            }
        };
        match require_user(&headers, &db) {
            Ok(u) => u,
            Err(_) => return (StatusCode::UNAUTHORIZED, "").into_response(),
        }
    };

    let req: FeedbackBody = match serde_json::from_slice::<FeedbackBody>(&body) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                format!("feedback error: invalid body: {e}\n"),
            )
                .into_response();
        }
    };

    // Structured verdict validation happens before we ever hop to the
    // blocking task — it's a pure check on the already-parsed body.
    // `None` = no `verdict` key at all = the exact pre-existing code path.
    let verdict_num: Option<i64> = match req.verdict.as_ref() {
        None => None,
        Some(v) => match v.normalize() {
            Some(n) => Some(n),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "feedback error: verdict must be 1, -1, \"good\", or \"bad\"\n",
                )
                    .into_response();
            }
        },
    };

    // Optional idempotency: only when X-Runai-Event-Id is present. This
    // guards the (expensive, LLM-calling) reevaluate path only — a bare
    // verdict vote is a cheap append-only insert and is not deduped by
    // this header (skill_feedback is event-sourced by design).
    let event_id_opt = headers
        .get("X-Runai-Event-Id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let payload_hash = super::skills::canonical_payload_hash_pub(&body);
    let event_id_for_task = event_id_opt.clone();
    let state_for_task = state.clone();
    let user_for_task = user;

    let join = tokio::task::spawn_blocking(move || -> Result<Response> {
        use crate::core::resource::ResourceKind;
        let base_dir = state_for_task.db_path.parent().unwrap().to_path_buf();
        let mgr = SkillManager::with_base(base_dir.clone())?;
        let db = mgr.db();
        let has_note = !req.note.trim().is_empty();

        // Structured verdict path (skill-feedback-radar): record an
        // event-sourced skill_feedback row. Requires the skill to resolve
        // in the caller's own owner scope (public pool ∪ their own
        // privates; admin sees everything) so a vote can't be recorded
        // against a name that doesn't exist for this caller. A miss is 404
        // empty body (anti-enumeration style), unlike the legacy path below.
        if let Some(verdict) = verdict_num {
            let owner_scope: Option<String> = if user_for_task.is_admin {
                Some("*".to_string())
            } else {
                Some(user_for_task.user_id.clone())
            };
            let resource = match db.find_resource_by_name_for_user(
                ResourceKind::Skill,
                &req.skill,
                owner_scope.as_deref(),
            )? {
                Some(r) => r,
                None => return Ok((StatusCode::NOT_FOUND, "").into_response()),
            };
            let note_opt = if has_note {
                Some(req.note.as_str())
            } else {
                None
            };
            db.record_skill_feedback(
                chrono::Utc::now().timestamp(),
                &req.skill,
                resource.owner_user_id.as_deref(),
                Some(user_for_task.user_id.as_str()),
                req.session_id.as_deref(),
                req.event_id,
                verdict,
                note_opt,
            )?;
        } else if !has_note {
            // Neither a verdict nor a note: nothing to record, nothing to
            // feed a re-enrich. Preserves `reevaluate_skill`'s historical
            // "--note is empty" wire contract (400 + text/plain) for the
            // legacy `{skill, note}` body with an empty/missing note.
            anyhow::bail!("--note is empty; pass concrete feedback text");
        }

        // Idempotency header only guards the (expensive) re-enrich TRIGGER,
        // same scope as before — a bare verdict vote's DB insert above is
        // never deduped by this header (skill_feedback is event-sourced).
        if let Some(event_id) = event_id_for_task.as_deref() {
            let outcome = db.record_usage_event(
                event_id,
                "feedback",
                &req.skill,
                &payload_hash,
                "",
                Some(user_for_task.user_id.as_str()),
            )?;
            match outcome {
                UsageOutcome::Conflict => {
                    anyhow::bail!("__conflict__");
                }
                UsageOutcome::Duplicate => {
                    return Ok((
                        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                        format!(
                            "feedback already applied by {}: {} (idempotent replay)\n",
                            user_for_task.username, req.skill
                        ),
                    )
                        .into_response());
                }
                UsageOutcome::First => { /* proceed to reenrich */ }
            }
        }

        // Legacy note-only path (no `verdict` key at all): existence is
        // checked here, synchronously, so a missing skill still surfaces
        // the historical 400 + "feedback error: skill not found: ..."
        // wire contract (pinned by
        // tests/router_skill_lifecycle_p1c0.rs::feedback_returns_400_for_unconfigured_router_or_missing_skill)
        // instead of silently queuing a re-enrich for a name that will
        // never resolve. Scope mirrors `reevaluate_skill`'s own internal
        // resolution (admin-wide "*", via `enrich_candidates`).
        if verdict_num.is_none() {
            db.find_resource_by_name_for_user(ResourceKind::Skill, &req.skill, Some("*"))?
                .ok_or_else(|| anyhow::anyhow!("skill not found: {}", req.skill))?;
        }

        // Every successful feedback (verdict-only, note-only, or both)
        // queues an async re-enrich now — closing the loop for
        // agent-driven feedback via `runai-client feedback`, not just the
        // dashboard's note box. Verdict-only feedback with no note
        // synthesizes a short signal so `reevaluate_skill` still has
        // something concrete to fold into the prompt.
        let reenrich_note = if has_note {
            req.note.clone()
        } else {
            // Reached only when verdict_num is Some — the bail above rules
            // out (verdict_num == None && !has_note), so !has_note here
            // implies a verdict was given.
            match verdict_num {
                Some(v) if v > 0 => "User gave a positive rating with no additional comment.",
                _ => "User gave a negative rating with no additional comment.",
            }
            .to_string()
        };
        let reenrich = if super::enrich_state::try_claim(&req.skill) {
            spawn_reevaluate(base_dir, req.skill.clone(), reenrich_note);
            "queued"
        } else {
            "already-running"
        };

        let message = format!(
            "feedback applied by {}: {} ({})\n",
            user_for_task.username,
            req.skill,
            if reenrich == "queued" {
                "queued for re-enrich"
            } else {
                "re-enrich already running"
            }
        );
        Ok(Json(FeedbackAck {
            ok: true,
            skill: req.skill.clone(),
            verdict: verdict_num,
            reenrich,
            message,
        })
        .into_response())
    })
    .await;

    match join {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            let msg = format!("{e:#}");
            if msg.contains("__conflict__") {
                return (
                    StatusCode::CONFLICT,
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "conflict: event_id already used with a different payload\n",
                )
                    .into_response();
            }
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

#[cfg(test)]
mod tests {
    use super::quick_candidates_from_entries;

    #[test]
    fn query_compatibility_lane_is_presentation_only_and_ranked() {
        let entries = [
            ("article-writing", "Write long-form articles"),
            ("pptx", "Presentation creation and analysis"),
            ("slide-making-skill", "Implement PowerPoint slides"),
            ("browser-qa", "Browser checks"),
        ];

        let candidates =
            quick_candidates_from_entries("制作一份中文自我介绍 PPT 演示文稿", &entries);
        let names = candidates
            .into_iter()
            .map(|candidate| candidate.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["pptx", "slide-making-skill"]);
        assert!(quick_candidates_from_entries("修复一个 Rust 编译错误", &entries).is_empty());
    }
}

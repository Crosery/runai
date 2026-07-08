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
/// `client_kind`, `cwd`, `transcript_path`).
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
        let mut native_sid = payload_str(&payload, "runai_session_id");
        if native_sid.is_empty() {
            native_sid = payload_str(&payload, "session_id");
        }
        let mut host_kind = payload_str(&payload, "client_kind");
        if host_kind.is_empty() {
            host_kind = payload_str(&payload, "host_kind");
        }

        // The host-native session key is scoped before hashing so two
        // teammates or host integrations cannot collide in router memory.
        let sid_scope = match (
            user_id_opt.as_deref(),
            user_prefix.is_empty(),
            host_kind.is_empty(),
        ) {
            (Some(uid), _, false) => format!("user:{uid}:host:{host_kind}"),
            (Some(uid), _, true) => format!("user:{uid}"),
            (None, false, false) => format!("header:{user_prefix}:host:{host_kind}"),
            (None, false, true) => format!("header:{user_prefix}"),
            (None, true, false) => format!("anon:host:{host_kind}"),
            (None, true, true) => "anon".to_string(),
        };
        let sid_string =
            crate::core::recommend::runai_session_id_from_native(Some(&sid_scope), &native_sid);

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
/// (case-insensitive) — the dashboard's thumbs-up/down buttons send the
/// string form, `runai-client` sends the numeric form. Anything else
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

#[derive(Serialize)]
struct FeedbackVerdictAck {
    ok: bool,
    skill: String,
    verdict: i64,
    /// Always `false` — the fast path never runs `reevaluate_skill`. Kept
    /// explicit in the wire shape so a client doesn't have to infer it.
    reevaluated: bool,
}

/// POST /feedback — replaces `runai recommend feedback`.
/// Body: `{"skill":"...","note":"..."}` (legacy) or `{"skill":"...",
/// "verdict":1|-1|"good"|"bad","note":"...","event_id":123,"session_id":"..."}`
/// (skill-feedback-radar).
///
/// Requires auth (Bearer or session cookie) — issue #26: this endpoint
/// triggers a real LLM call (`reevaluate_skill`) and rewrites a skill's AI
/// summary/score, so it must be gated the same way as every other write
/// endpoint (`require_user`). An anonymous caller gets `401` with a
/// completely EMPTY body (not `ApiError::Unauthorized`'s JSON shape) to
/// match the anti-enumeration style used by `empty_404` / the `/recommend`
/// fail-closed-Bearer lane — no distinguishing "auth required" text for a
/// probe to key off of.
///
/// The "applied by" attribution is now always the AUTHENTICATED username.
/// The legacy `X-Runai-User` header is no longer read here at all — it was
/// fully client-controlled and trivially forgeable, so trusting it for the
/// audit-trail field would let any anonymous caller impersonate anyone in
/// the response text.
///
/// Idempotency (PLANNING §1.3, optional): when the request carries an
/// `X-Runai-Event-Id` header, the side effect is gated on the
/// `usage_events` table (kind=`feedback`). First → reevaluate runs;
/// Duplicate (same id + same payload hash) → 200 no-op; Conflict (same
/// id, different hash) → 409. When the header is absent, the legacy
/// non-idempotent path runs unchanged so pre-protocol callers (and the
/// existing feedback_auth_e2e suite) keep working. The `runai-client`
/// companion always sends the header, so the protocol lane is the live
/// one for new deploys.
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
        let mgr = SkillManager::with_base(state_for_task.db_path.parent().unwrap().to_path_buf())?;
        let db = mgr.db();

        // Structured verdict path (skill-feedback-radar): record an
        // event-sourced skill_feedback row. Requires the skill to resolve
        // in the caller's own owner scope (public pool ∪ their own
        // privates; admin sees everything) so a vote can't be recorded
        // against a name that doesn't exist for this caller.
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
            let note_opt = if req.note.trim().is_empty() {
                None
            } else {
                Some(req.note.as_str())
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

            if note_opt.is_none() {
                // Fast path: no note means no LLM re-enrich. Ack quickly —
                // this is what keeps a thumbs-up/down button snappy.
                return Ok(Json(FeedbackVerdictAck {
                    ok: true,
                    skill: req.skill.clone(),
                    verdict,
                    reevaluated: false,
                })
                .into_response());
            }
            // Note is non-empty: fall through to the reevaluate flow below,
            // same as the legacy (no-verdict) path.
        }

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
                UsageOutcome::First => { /* proceed to reevaluate */ }
            }
        }
        let report = recommend::reevaluate_skill(&mgr, &req.skill, &req.note)?;
        Ok((
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!(
                "feedback applied by {}: {} llm_score {} → {} (summary {} chars)\n",
                user_for_task.username,
                req.skill,
                report.old_score,
                report.new_score,
                report.new_summary_len
            ),
        )
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

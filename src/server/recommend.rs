//! Remote-hook endpoints: `/recommend` (the remote skill router) and
//! `/feedback`, plus the `guess_server_url` URL-reconstruction helper.

use anyhow::Result;
use axum::{
    Json,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::core::auth as authmod;
use crate::core::manager::SkillManager;
use crate::core::recommend;
use crate::core::recommend::local_ipv4;

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
    if is_loopback {
        if let Some(ip) = local_ipv4() {
            return format!("{scheme}://{ip}:{port_part}");
        }
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

#[derive(Deserialize)]
pub(super) struct FeedbackBody {
    skill: String,
    note: String,
}

/// POST /feedback — replaces `runai recommend feedback`.
/// Body: `{"skill":"...","note":"..."}`.
pub(super) async fn handle_feedback(headers: HeaderMap, Json(req): Json<FeedbackBody>) -> Response {
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

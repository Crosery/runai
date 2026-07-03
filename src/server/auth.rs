//! v15 multi-user auth handlers: register / login / logout / me.
//!
//! Auth resolution order: Authorization: Bearer first (client hook),
//! then runai_session cookie (browser dashboard). Both end up at the
//! same User row via api_key_hash lookup, so password is only used at
//! /auth/login to mint the cookie.

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
use crate::core::server_mode::ServerMode;

use super::error::ApiError;
use super::state::{AppState, require_user, server_mode};

#[derive(Deserialize)]
pub(super) struct RegisterReq {
    username: String,
    password: String,
}

#[derive(Serialize)]
pub(super) struct RegisterResp {
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
pub(super) async fn api_register(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RegisterReq>,
) -> Result<(StatusCode, Json<RegisterResp>), ApiError> {
    // Owner mode = single-user self-serve. The /users/register endpoint is
    // off — only the operator on the box, who already implicitly has admin,
    // can use this dashboard. Cuts off remote account creation entirely.
    // See PLANNING.md §1.1.
    if state.mode == ServerMode::Owner {
        return Err(ApiError::Forbidden);
    }
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
pub(super) struct LoginReq {
    username: String,
    password: String,
    /// Issue #35: only credential-persisting clients (the install script,
    /// which writes ~/.runai-identity) ask for a rotation. The dashboard
    /// omits it — a browser login must not revoke installed hook clients.
    #[serde(default)]
    rotate_api_key: bool,
}

#[derive(Serialize)]
pub(super) struct LoginResp {
    user_id: String,
    username: String,
    is_admin: bool,
    /// Present only on `rotate_api_key: true` logins. A dashboard login
    /// authenticates via the session cookie alone and never sees a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
}

/// Canonical failure body for /auth/login. Used for BOTH "user does not
/// exist" and "password mismatch" (and "account disabled") so an attacker
/// cannot enumerate account existence by diffing error strings. PLANNING
/// §2.3 item 5 — abuser sees the same byte sequence on every miss.
///
/// Status: always 401. Body: `{"error":"invalid_credentials"}`. Both are
/// deliberately generic. Server-side logs can still record the real reason
/// for the admin reading the dashboard; the wire format is the gate.
const LOGIN_FAILURE_BODY: &str = r#"{"error":"invalid_credentials"}"#;

fn login_failure() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "application/json".to_string())],
        LOGIN_FAILURE_BODY.to_string(),
    )
        .into_response()
}

/// POST /auth/login
/// body: {"username": "...", "password": "...", "rotate_api_key": bool?}
///
/// Two lanes (issue #35):
/// - default (dashboard): verifies the password, mints an independent
///   `rnai_sess_...` session token (hash stored in `users.session_key_hash`),
///   sets it as the runai_session cookie, and leaves the api_key untouched —
///   a browser login must not revoke installed hook clients. The response
///   body carries NO api_key.
/// - `rotate_api_key: true` (install script, which persists the key to
///   ~/.runai-identity): mints a fresh api_key, replaces `api_key_hash`
///   (revoking all previous copies), and returns it in JSON. The session
///   slot is untouched so an active browser session survives.
///
/// Failure shape: always 401 + `{"error":"invalid_credentials"}` — same
/// body for "no such user" / "wrong password" / "account disabled" so an
/// attacker cannot enumerate accounts via response diffing (PLANNING
/// §2.3 item 5). The 500 path (DB unavailable) still routes through
/// `ApiError::Internal` because it is operationally distinct.
pub(super) async fn api_login(
    State(state): State<Arc<AppState>>,
    Json(req): Json<LoginReq>,
) -> Response {
    // Outer Result is Internal (5xx) vs login-flow (success-or-401).
    // The login-flow branch is `Result<(LoginResp, cookie_token), ()>` —
    // the `Err(())` payload is intentionally type-erased because all three
    // failure reasons collapse to the same wire response. cookie_token is
    // None on the rotate lane (the script ignores cookies; setting one
    // would serve no client).
    type LoginOk = (LoginResp, Option<String>);
    let join =
        tokio::task::spawn_blocking(move || -> Result<Result<LoginOk, ()>, anyhow::Error> {
            let db = state.db()?;
            let user = match db.find_user_by_username(&req.username)? {
                Some(u) => u,
                None => return Ok(Err(())),
            };
            if user.disabled {
                return Ok(Err(()));
            }
            if !authmod::verify_password(&req.password, &user.password_hash)? {
                return Ok(Err(()));
            }

            if req.rotate_api_key {
                // Script lane: we cannot recover the original secret from the
                // hash, so handing out a key means minting a new one and
                // updating the hash. This invalidates any previously-installed
                // client that hasn't re-run the install script since — the
                // password is the source of truth.
                let new_key = authmod::new_api_key();
                let new_hash = authmod::key_hash(&authmod::BearerToken(new_key.clone()));
                db.rotate_api_key(&user.user_id, &new_hash)?;
                Ok(Ok((
                    LoginResp {
                        user_id: user.user_id,
                        username: user.username,
                        is_admin: user.is_admin,
                        api_key: Some(new_key),
                    },
                    None,
                )))
            } else {
                // Dashboard lane: independent session token, api_key untouched.
                let session_token = authmod::new_session_token();
                let session_hash = authmod::key_hash(&authmod::BearerToken(session_token.clone()));
                db.set_session_key_hash(&user.user_id, Some(&session_hash))?;
                Ok(Ok((
                    LoginResp {
                        user_id: user.user_id,
                        username: user.username,
                        is_admin: user.is_admin,
                        api_key: None,
                    },
                    Some(session_token),
                )))
            }
        })
        .await;

    let inner = match join {
        Ok(inner) => inner,
        Err(e) => {
            return ApiError::Internal(anyhow::anyhow!(e)).into_response();
        }
    };
    let (resp, cookie_token) = match inner {
        Ok(Ok(r)) => r,
        Ok(Err(())) => return login_failure(),
        Err(e) => return ApiError::Internal(e).into_response(),
    };

    let body = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
    let Some(token) = cookie_token else {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json".to_string())],
            body,
        )
            .into_response();
    };
    let cookie = authmod::build_session_cookie(&token, false, 60 * 60 * 24 * 30);
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::SET_COOKIE, cookie),
        ],
        body,
    )
        .into_response()
}

/// POST /auth/logout — clears the session cookie. Does NOT rotate the
/// api_key; the client-side hook continues to work with its stored Bearer.
pub(super) async fn api_logout() -> Response {
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

/// Log out EVERYWHERE (E1): plain `api_logout` only forgets the current
/// browser's cookie — a copy of the session token captured before logout
/// (devtools / a proxy), and the hook's `~/.runai-identity` key, keep
/// authenticating. This is the real revoke: it rotates the api_key AND
/// clears the session slot (issue #35), invalidating every existing
/// credential incl. the caller's own cookie, then clears the cookie. The
/// user must log in again; previously-installed hook clients must re-run
/// the install script. Requires an authenticated caller.
pub(super) async fn api_logout_everywhere(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let join = tokio::task::spawn_blocking(move || -> Result<(), ApiError> {
        let db = state.db()?;
        let user = require_user(&headers, &db)?;
        let new_key = authmod::new_api_key();
        let new_hash = authmod::key_hash(&authmod::BearerToken(new_key));
        db.rotate_api_key(&user.user_id, &new_hash)
            .map_err(ApiError::Internal)?;
        db.set_session_key_hash(&user.user_id, None)
            .map_err(ApiError::Internal)?;
        Ok(())
    })
    .await;
    match join {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return e.into_response(),
        Err(e) => return ApiError::Internal(anyhow::anyhow!(e)).into_response(),
    }
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
pub(super) struct MeResp {
    user_id: String,
    username: String,
    is_admin: bool,
    library_size: usize,
    /// Runtime server mode (`"owner"` or `"team"`). Frontend reads this on
    /// boot to set the `mode-owner` body class and skip the login UI when
    /// the server runs in single-user self-serve mode. PLANNING §1.1.
    mode: String,
}

/// GET /api/me — current user info. 401 when unauthenticated.
///
/// Owner-mode short circuit: `require_user` returns the synthetic owner
/// identity via `state::current_user` regardless of credential, so this
/// endpoint succeeds with `is_admin: true` + `library_size: 0` on every
/// owner-mode request. The synthetic `user_id = "owner"` is a reserved
/// sentinel — `library_count` on it returns 0 because the row doesn't
/// exist in `users` table.
pub(super) async fn api_me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<MeResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MeResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let u = require_user(&headers, &db)?;
        // Synthetic owner has no DB row → library_count would fail; default to 0.
        let size = if u.user_id == "owner" {
            0
        } else {
            db.library_count(&u.user_id).map_err(ApiError::Internal)?
        };
        Ok(MeResp {
            user_id: u.user_id,
            username: u.username,
            is_admin: u.is_admin,
            library_size: size,
            mode: server_mode().as_str().to_string(),
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

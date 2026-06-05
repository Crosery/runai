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
use super::state::{AppState, require_user};

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
}

#[derive(Serialize)]
pub(super) struct LoginResp {
    user_id: String,
    username: String,
    is_admin: bool,
    api_key: String,
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
/// body: {"username": "...", "password": "..."}
/// Returns the api_key in JSON and sets the runai_session cookie. Either
/// channel can be used afterwards.
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
    // The login-flow branch is `Result<LoginResp, ()>` — the `Err(())`
    // payload is intentionally type-erased because all three failure
    // reasons collapse to the same wire response.
    let join =
        tokio::task::spawn_blocking(move || -> Result<Result<LoginResp, ()>, anyhow::Error> {
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

            // On login we rotate-issue: re-emit the api_key from the row.
            // We cannot recover the original secret from the hash, so login
            // must mint a new api_key and update the hash. This invalidates
            // any previously-installed client that hasn't logged in since —
            // intentional: the password is the source of truth.
            let new_key = authmod::new_api_key();
            let new_hash = authmod::key_hash(&authmod::BearerToken(new_key.clone()));
            db.rotate_api_key(&user.user_id, &new_hash)?;

            Ok(Ok(LoginResp {
                user_id: user.user_id,
                username: user.username,
                is_admin: user.is_admin,
                api_key: new_key,
            }))
        })
        .await;

    let inner = match join {
        Ok(inner) => inner,
        Err(e) => {
            return ApiError::Internal(anyhow::anyhow!(e)).into_response();
        }
    };
    let resp = match inner {
        Ok(Ok(r)) => r,
        Ok(Err(())) => return login_failure(),
        Err(e) => return ApiError::Internal(e).into_response(),
    };

    let cookie = authmod::build_session_cookie(&resp.api_key, false, 60 * 60 * 24 * 30);
    let body = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".into());
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

#[derive(Serialize)]
pub(super) struct MeResp {
    user_id: String,
    username: String,
    is_admin: bool,
    library_size: usize,
}

/// GET /api/me — current user info. 401 when unauthenticated.
pub(super) async fn api_me(
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

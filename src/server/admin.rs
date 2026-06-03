//! v15 admin — user management (list / promote / disable / delete).
//!
//! All routes require an admin Bearer / cookie. First-user-auto-admin
//! bootstrap means the very first /users/register call grants admin.

use anyhow::Result;
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::error::ApiError;
use super::state::{AppState, require_admin};

#[derive(Serialize)]
pub(super) struct AdminUserRow {
    user_id: String,
    username: String,
    is_admin: bool,
    disabled: bool,
    created_at: i64,
    library_size: usize,
    event_count: i64,
}

#[derive(Serialize)]
pub(super) struct AdminUsersListResp {
    total: usize,
    items: Vec<AdminUserRow>,
}

pub(super) async fn api_admin_users_list(
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
pub(super) struct AdminUserPatch {
    #[serde(default)]
    is_admin: Option<bool>,
    #[serde(default)]
    disabled: Option<bool>,
}

pub(super) async fn api_admin_users_update(
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
pub(super) struct AdminDeleteResp {
    user_id: String,
    deleted_library: usize,
}

pub(super) async fn api_admin_users_delete(
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

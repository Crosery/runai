//! v15 admin — user management (list / promote / disable / delete).
//! v0.11 model B — admin userlib browse (PLANNING §1.6): admin sees a
//! "用户库" sub-tab next to "已安装", with one row per non-admin user
//! and a detail panel that splits each user's library into private
//! (owner_user_id = uid) vs imported public (user_skill_library row).
//!
//! All routes require an admin Bearer / cookie. First-user-auto-admin
//! bootstrap means the very first /users/register call grants admin.

use anyhow::Result;
use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::paths::AppPaths;

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
                return Err(ApiError::BadRequest("cannot demote yourself".into()));
            }
            if patch.disabled == Some(true) {
                return Err(ApiError::BadRequest("cannot disable yourself".into()));
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

// ─── publish-request workflow (PLANNING §1.4 C9d) ───────────────────────

#[derive(Serialize)]
pub(super) struct AdminPublishRow {
    resource_id: String,
    name: String,
    description: String,
    owner_user_id: String,
    /// Username denormalized from users.username for the dashboard so the
    /// admin doesn't have to second-look-up an opaque user_id.
    uploader_username: String,
    /// Unix seconds when the row was uploaded (resources.installed_at).
    uploaded_at: i64,
    /// AI-generated summary content (whatever `recommend enrich` wrote).
    /// Empty string if enrich never landed — the publish-request gate
    /// would have rejected the request, so empty summaries shouldn't
    /// appear in pending listings, but defensively render the empty case.
    ai_summary: String,
}

#[derive(Serialize)]
pub(super) struct AdminPublishListResp {
    total: usize,
    items: Vec<AdminPublishRow>,
}

/// GET /api/admin/publish-requests — list every private skill in the
/// `pending` publish_status. Joins users to surface the uploader's
/// username + ai_summary so the admin has enough context to approve /
/// reject without a follow-up round trip. Sorted by upload time desc.
pub(super) async fn api_admin_publish_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AdminPublishListResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminPublishListResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_admin(&headers, &db)?;
        let mut stmt = db
            .conn_ref()
            .prepare(
                "SELECT r.id, r.name, COALESCE(r.description, ''), r.owner_user_id, \
                        COALESCE(u.username, ''), r.installed_at, \
                        COALESCE(s.summary, '') \
                 FROM resources r \
                 LEFT JOIN users u ON u.user_id = r.owner_user_id \
                 LEFT JOIN resource_ai_summary s ON s.name = r.name \
                 WHERE r.publish_status = 'pending' AND r.kind = 'skill' \
                 ORDER BY r.installed_at DESC",
            )
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let items: Vec<AdminPublishRow> = stmt
            .query_map([], |row| {
                Ok(AdminPublishRow {
                    resource_id: row.get::<_, String>(0)?,
                    name: row.get::<_, String>(1)?,
                    description: row.get::<_, String>(2)?,
                    owner_user_id: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    uploader_username: row.get::<_, String>(4)?,
                    uploaded_at: row.get::<_, i64>(5)?,
                    ai_summary: row.get::<_, String>(6)?,
                })
            })
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        Ok(AdminPublishListResp {
            total: items.len(),
            items,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Serialize)]
pub(super) struct AdminPublishActionResp {
    resource_id: String,
    publish_status: String,
}

/// Recursive directory copy. Used by approve to clone the private skill
/// tree into the community pool without depending on a `cp` subprocess.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// POST /api/admin/publish-requests/{resource_id}/approve — admin signs
/// off on a pending publish-request:
///   1. Copy `<data>/users/<uid>/skills/<name>/` → `<data>/community/<uid>/<name>/`
///   2. Upsert a `community_skills` row with a monotonic version.
///   3. Flip publish_status pending → approved (clearing publish_reason).
///
/// 404 when the resource_id doesn't exist; 400 when the resource isn't
/// in `pending` state (drafts can't skip the user submission step, and
/// re-approving is a no-op the caller would benefit from knowing about).
pub(super) async fn api_admin_publish_approve(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(resource_id): Path<String>,
) -> Result<Json<AdminPublishActionResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminPublishActionResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_admin(&headers, &db)?;
        let res = db
            .get_resource(&resource_id)
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;
        if res.publish_status != "pending" {
            return Err(ApiError::BadRequest(format!(
                "resource is in publish_status='{}', only 'pending' rows can be approved",
                res.publish_status
            )));
        }
        let owner = res.owner_user_id.clone().ok_or_else(|| {
            ApiError::BadRequest("public-pool row has no owner to publish".into())
        })?;

        let data_root = state
            .db_path
            .parent()
            .expect("db_path has parent")
            .to_path_buf();
        let paths = AppPaths::with_base(data_root);
        let dst = paths
            .community_skill_dir(&owner, &res.name)
            .map_err(ApiError::Internal)?;
        if dst.exists() {
            std::fs::remove_dir_all(&dst).map_err(|e| ApiError::Internal(e.into()))?;
        }
        copy_dir_recursive(&res.directory, &dst).map_err(|e| ApiError::Internal(e.into()))?;

        let version = format!("v{}", chrono::Utc::now().timestamp());
        db.upsert_community_skill(&owner, &res.name, &version)
            .map_err(ApiError::Internal)?;

        db.set_publish_status_with_reason(&resource_id, "approved", None)
            .map_err(ApiError::Internal)?;

        Ok(AdminPublishActionResp {
            resource_id,
            publish_status: "approved".to_string(),
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
pub(super) struct AdminPublishRejectReq {
    reason: String,
}

/// POST /api/admin/publish-requests/{resource_id}/reject — admin declines
/// the pending request and records a free-text reason the user can read
/// from list-mine. Flips pending → rejected; non-pending rows return 400.
pub(super) async fn api_admin_publish_reject(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(resource_id): Path<String>,
    Json(req): Json<AdminPublishRejectReq>,
) -> Result<Json<AdminPublishActionResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminPublishActionResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_admin(&headers, &db)?;
        let res = db
            .get_resource(&resource_id)
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;
        if res.publish_status != "pending" {
            return Err(ApiError::BadRequest(format!(
                "resource is in publish_status='{}', only 'pending' rows can be rejected",
                res.publish_status
            )));
        }
        let reason = req.reason.trim();
        if reason.is_empty() {
            return Err(ApiError::BadRequest(
                "reject reason cannot be empty — give the user something to act on".into(),
            ));
        }
        db.set_publish_status_with_reason(&resource_id, "rejected", Some(reason))
            .map_err(ApiError::Internal)?;
        Ok(AdminPublishActionResp {
            resource_id,
            publish_status: "rejected".to_string(),
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

// ─── admin batch trash (public pool maintenance) ────────────────────────

#[derive(Deserialize)]
pub(super) struct AdminSkillsTrashReq {
    /// Skill names to move to trash. Names are matched against the public
    /// pool only (`SkillManager::find_resource_id` is public-pool-only by
    /// design), so this CANNOT accidentally trash a private skill owned by
    /// another user — admin's per-user delete path lives in the userlib
    /// detail panel instead (PLANNING §1.6 Model B C6c/C6d).
    names: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct AdminSkillsTrashResp {
    trashed: usize,
    failed: Vec<String>,
}

/// POST /api/admin/skills/trash — admin bulk-trash N public-pool skills
/// in one call. Delegates to `SkillManager::batch_delete` so the per-name
/// failure granularity (and the trash-not-hard-delete semantic) match the
/// CLI / TUI / MCP flavors of the same operation. Returns `200` even when
/// every name fails — the caller needs `failed` to render per-row outcomes
/// on the dashboard.
pub(super) async fn api_admin_skills_trash(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<AdminSkillsTrashReq>,
) -> Result<Json<AdminSkillsTrashResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminSkillsTrashResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_admin(&headers, &db)?;
        // Empty batch is a no-op rather than a 400 — callers will hit this
        // when the user clicks "trash" with zero rows selected after a
        // race against a concurrent refresh.
        if req.names.is_empty() {
            return Ok(AdminSkillsTrashResp {
                trashed: 0,
                failed: Vec::new(),
            });
        }
        let manager = crate::core::manager::SkillManager::new().map_err(ApiError::Internal)?;
        let (trashed, failed) = manager
            .batch_delete(&req.names)
            .map_err(ApiError::Internal)?;
        Ok(AdminSkillsTrashResp { trashed, failed })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

// ─── userlib (Model B): list + detail ───────────────────────────────────

#[derive(Serialize)]
pub(super) struct AdminUserlibRow {
    user_id: String,
    username: String,
    disabled: bool,
    created_at: i64,
    private_count: usize,
    imported_count: usize,
    /// Latest `router_events.ts` for this user, or `0` if they have never
    /// fired a router request. `0` is the sentinel so the frontend can
    /// render "—" without a separate `nullable` branch.
    last_active_ts: i64,
}

#[derive(Serialize)]
pub(super) struct AdminUserlibListResp {
    total: usize,
    items: Vec<AdminUserlibRow>,
}

/// GET /api/admin/userlib — list every NON-admin user with library headline
/// stats. Sorted by `last_active_ts DESC, username ASC` so the admin sees
/// recently-active users at the top. Admin users themselves are filtered
/// out because their "library" is the entire public pool by convention
/// (PLANNING §1.6 model B) — there's nothing meaningful to inspect.
pub(super) async fn api_admin_userlib_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<AdminUserlibListResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminUserlibListResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_admin(&headers, &db)?;
        let mut stmt = db
            .conn_ref()
            .prepare(
                "SELECT u.user_id, u.username, u.disabled, u.created_at,
                    COALESCE((SELECT COUNT(*) FROM resources \
                              WHERE owner_user_id = u.user_id AND kind = 'skill'), 0) AS private_count,
                    COALESCE((SELECT COUNT(*) FROM user_skill_library \
                              WHERE user_id = u.user_id), 0) AS imported_count,
                    COALESCE((SELECT MAX(ts) FROM router_events \
                              WHERE user_id = u.user_id), 0) AS last_active_ts
                 FROM users u
                 WHERE u.is_admin = 0
                 ORDER BY last_active_ts DESC, u.username ASC",
            )
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let items: Vec<AdminUserlibRow> = stmt
            .query_map([], |r| {
                Ok(AdminUserlibRow {
                    user_id: r.get::<_, String>(0)?,
                    username: r.get::<_, String>(1)?,
                    disabled: r.get::<_, i64>(2)? != 0,
                    created_at: r.get::<_, i64>(3)?,
                    private_count: r.get::<_, i64>(4)? as usize,
                    imported_count: r.get::<_, i64>(5)? as usize,
                    last_active_ts: r.get::<_, i64>(6)?,
                })
            })
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        Ok(AdminUserlibListResp {
            total: items.len(),
            items,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Serialize)]
pub(super) struct AdminUserlibSkillEntry {
    name: String,
    description: String,
    usage_count: i64,
    last_used_at: Option<i64>,
}

#[derive(Serialize)]
pub(super) struct AdminUserlibDetailResp {
    user_id: String,
    username: String,
    disabled: bool,
    /// User's private skills (`resources.owner_user_id = uid`).
    private: Vec<AdminUserlibSkillEntry>,
    /// Public skills the user has subscribed via `user_skill_library`.
    imported: Vec<AdminUserlibSkillEntry>,
    recent_events_count: i64,
    last_active_ts: i64,
}

/// GET /api/admin/userlib/{user_id} — that user's library split into
/// private + imported public, plus aggregate router-event stats. Used to
/// power the admin "user detail" panel (PLANNING §1.6 Model B layer 2).
pub(super) async fn api_admin_userlib_detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<Json<AdminUserlibDetailResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<AdminUserlibDetailResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_admin(&headers, &db)?;

        let user = db
            .find_user_by_id(&user_id)
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;

        // Private skills owned by this user.
        let mut p_stmt = db
            .conn_ref()
            .prepare(
                "SELECT name, COALESCE(description, ''), usage_count, last_used_at \
                 FROM resources \
                 WHERE owner_user_id = ?1 AND kind = 'skill' \
                 ORDER BY usage_count DESC, name ASC",
            )
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let private: Vec<AdminUserlibSkillEntry> = p_stmt
            .query_map(params![&user_id], |r| {
                Ok(AdminUserlibSkillEntry {
                    name: r.get::<_, String>(0)?,
                    description: r.get::<_, String>(1)?,
                    usage_count: r.get::<_, i64>(2)?,
                    last_used_at: r.get::<_, Option<i64>>(3)?,
                })
            })
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

        // Imported public skills (rows in user_skill_library that still
        // resolve to a live public-pool resources row).
        let mut i_stmt = db
            .conn_ref()
            .prepare(
                "SELECT r.name, COALESCE(r.description, ''), r.usage_count, r.last_used_at \
                 FROM resources r \
                 INNER JOIN user_skill_library lib ON lib.skill_name = r.name \
                 WHERE lib.user_id = ?1 AND r.owner_user_id IS NULL AND r.kind = 'skill' \
                 ORDER BY r.usage_count DESC, r.name ASC",
            )
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let imported: Vec<AdminUserlibSkillEntry> = i_stmt
            .query_map(params![&user_id], |r| {
                Ok(AdminUserlibSkillEntry {
                    name: r.get::<_, String>(0)?,
                    description: r.get::<_, String>(1)?,
                    usage_count: r.get::<_, i64>(2)?,
                    last_used_at: r.get::<_, Option<i64>>(3)?,
                })
            })
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

        let recent_events_count: i64 = db
            .conn_ref()
            .query_row(
                "SELECT COUNT(*) FROM router_events WHERE user_id = ?1",
                params![&user_id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let last_active_ts: i64 = db
            .conn_ref()
            .query_row(
                "SELECT COALESCE(MAX(ts), 0) FROM router_events WHERE user_id = ?1",
                params![&user_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        Ok(AdminUserlibDetailResp {
            user_id: user.user_id,
            username: user.username,
            disabled: user.disabled,
            private,
            imported,
            recent_events_count,
            last_active_ts,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
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

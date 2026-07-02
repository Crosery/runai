//! PLANNING §1.4 (rewrite) — private skill upload endpoint.
//!
//! POST /api/users/me/skills/upload (multipart):
//!   - `name`: skill name (string field)
//!   - `bundle`: gzipped tar of the skill directory (file field)
//!
//! Side effects:
//!   - Writes / replaces `<data>/users/<uid>/skills/<name>/`.
//!   - Inserts a `resources` row with `owner_user_id = uid` and
//!     `publish_status = 'draft'`.
//!
//! The publish workflow (draft → pending → approved / rejected) lives in
//! C9c (publish-request) and C9d (admin approve/reject). This endpoint is
//! the entry point: every user-uploaded skill starts here as `draft`.
//!
//! Auth: team mode only (mirrors community.rs), Bearer or session cookie
//! must resolve to a non-disabled user. Owner mode has no remote client
//! surface (PLANNING §1.1) and returns 403 the same way community does.

use anyhow::Result;
use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::HeaderMap,
};
use serde::Serialize;
use std::sync::Arc;

use crate::core::manager::SkillManager;
use crate::core::paths::AppPaths;
use crate::core::resource::ResourceKind;
use crate::core::server_mode::ServerMode;

use super::community::{extract_targz, is_safe_skill_name, move_skill_payload};
use super::error::ApiError;
use super::market::spawn_enrich;
use super::state::{AppState, require_user};

#[derive(Serialize)]
pub(super) struct PrivateUploadResp {
    name: String,
    uploader_uid: String,
    bytes: usize,
    publish_status: String,
}

/// POST /api/users/me/skills/upload — accept a skill bundle and land it
/// in the caller's private pool with publish_status='draft'.
pub(super) async fn api_user_skills_upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<PrivateUploadResp>, ApiError> {
    if state.mode == ServerMode::Owner {
        return Err(ApiError::Forbidden);
    }

    let user = {
        let db = state.db().map_err(ApiError::Internal)?;
        require_user(&headers, &db)?
    };

    let mut skill_name: Option<String> = None;
    let mut bundle_bytes: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("multipart: {e}")))?
    {
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "name" => {
                let v = field
                    .text()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("read name: {e}")))?;
                skill_name = Some(v.trim().to_string());
            }
            "bundle" => {
                let bytes = field
                    .bytes()
                    .await
                    .map_err(|e| ApiError::BadRequest(format!("read bundle: {e}")))?;
                bundle_bytes = Some(bytes.to_vec());
            }
            _ => {
                let _ = field.bytes().await;
            }
        }
    }

    let name = skill_name.ok_or_else(|| ApiError::BadRequest("missing 'name' field".into()))?;
    let bytes =
        bundle_bytes.ok_or_else(|| ApiError::BadRequest("missing 'bundle' field".into()))?;
    if !is_safe_skill_name(&name) {
        return Err(ApiError::BadRequest(format!("invalid skill name: {name}")));
    }
    if bytes.is_empty() {
        return Err(ApiError::BadRequest("empty bundle".into()));
    }

    let uid = user.user_id.clone();
    let bytes_len = bytes.len();
    let resp = tokio::task::spawn_blocking(move || -> Result<PrivateUploadResp, ApiError> {
        let data_root = state
            .db_path
            .parent()
            .expect("db_path has parent")
            .to_path_buf();
        let paths = AppPaths::with_base(data_root.clone());

        // Land the unpacked tree under <data>/users/<uid>/skills/<name>/
        // via a staging tempdir on the same filesystem so a malformed
        // archive never partially overwrites a previously-good payload.
        let user_skills_dir = paths.user_skills_dir(&uid).map_err(ApiError::Internal)?;
        std::fs::create_dir_all(&user_skills_dir).map_err(|e| ApiError::Internal(e.into()))?;
        let staging = tempfile::tempdir_in(&user_skills_dir)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("staging tempdir: {e}")))?;
        if let Err(e) = extract_targz(&bytes, staging.path()) {
            return Err(ApiError::BadRequest(format!("invalid tar.gz bundle: {e}")));
        }
        let final_dir = user_skills_dir.join(&name);
        move_skill_payload(staging.path(), &final_dir)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;

        // Register as a private skill row via SkillManager — same path
        // installer uses for github-source private skills (owner_user_id
        // stamped, publish_status defaults to 'draft' from schema).
        let mgr = SkillManager::with_base(data_root).map_err(ApiError::Internal)?;
        mgr.register_local_skill_for(&name, Some(&uid))
            .map_err(ApiError::Internal)?;

        // Best-effort enrich (BM25 summary + LLM score). Same fire-and-
        // forget spawn used by the github install flow — failure here
        // does NOT roll the upload back; the user can re-fire enrich
        // later via `runai recommend enrich --name <name>` if needed.
        // PLANNING §1.4 publish workflow gates publish-request on
        // enrich completion, so a draft sitting at "no summary" is
        // visible to the user but cannot be submitted for review until
        // enrich actually lands.
        spawn_enrich(&[name.clone()]);

        Ok(PrivateUploadResp {
            name,
            uploader_uid: uid,
            bytes: bytes_len,
            publish_status: "draft".to_string(),
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;

    Ok(Json(resp))
}

#[derive(Serialize)]
pub(super) struct MySkillRow {
    name: String,
    description: String,
    uploaded_at: i64,
    /// `"done"` (ai_summary present) or `"pending"` (waiting / failed).
    /// MVP per PLANNING §1.4 C9e — distinguishes "still cooking" from
    /// "ready to submit for publish".
    enrich_status: String,
    publish_status: String,
    /// Set only when publish_status == "rejected". Free-text reason the
    /// admin gave so the user knows what to fix before re-submitting.
    publish_reason: Option<String>,
}

#[derive(Serialize)]
pub(super) struct MySkillsResp {
    total: usize,
    items: Vec<MySkillRow>,
}

/// GET /api/users/me/skills — list every skill the caller owns
/// (resources.owner_user_id = caller). Sorted by upload time desc so
/// recent uploads appear first. Each row carries the publish workflow
/// state (draft / pending / approved / rejected + reason) and the
/// enrich status so the CLI / dashboard can render guidance like
/// "still being summarized; can't publish yet".
pub(super) async fn api_user_skills_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<MySkillsResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MySkillsResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let mut stmt = db
            .conn_ref()
            .prepare(
                "SELECT r.name, COALESCE(r.description, ''), r.installed_at, \
                        r.publish_status, r.publish_reason, COALESCE(s.summary, '') \
                 FROM resources r \
                 LEFT JOIN resource_ai_summary s \
                    ON s.owner_user_id = COALESCE(r.owner_user_id, '') AND s.name = r.name \
                 WHERE r.owner_user_id = ?1 AND r.kind = 'skill' \
                 ORDER BY r.installed_at DESC",
            )
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let items: Vec<MySkillRow> = stmt
            .query_map(rusqlite::params![&user.user_id], |row| {
                let summary: String = row.get(5)?;
                let enrich_status = if summary.trim().is_empty() {
                    "pending".to_string()
                } else {
                    "done".to_string()
                };
                Ok(MySkillRow {
                    name: row.get::<_, String>(0)?,
                    description: row.get::<_, String>(1)?,
                    uploaded_at: row.get::<_, i64>(2)?,
                    enrich_status,
                    publish_status: row.get::<_, String>(3)?,
                    publish_reason: row.get::<_, Option<String>>(4)?,
                })
            })
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        Ok(MySkillsResp {
            total: items.len(),
            items,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Serialize)]
pub(super) struct PublishRequestResp {
    name: String,
    publish_status: String,
}

/// POST /api/users/me/skills/{name}/publish-request — user-driven flow:
/// flip a draft private skill to `pending` so the admin can review +
/// approve / reject. Pre-conditions enforced:
///   - team mode only (owner mode 403 — single-user self-serve, no community).
///   - caller must own the skill (private row with matching owner_user_id).
///     Resolves via `find_resource_by_name_for_user(uid)`; if no such row
///     exists OR the row is public-pool (owner None) we 404 to avoid
///     leaking the existence of someone else's private skill name.
///   - enrich must be done (`resource_ai_summary` non-empty). If not, 400
///     with `富集 (enrich) 还未完成` so the user knows to wait / re-fire.
///
/// Idempotent: re-submitting a pending or approved skill silently no-ops
/// (returns 200 with the existing status). Re-submitting a `rejected`
/// row flips it back to `pending` so the user can iterate after fixing
/// whatever the admin flagged.
pub(super) async fn api_publish_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<PublishRequestResp>, ApiError> {
    if state.mode == ServerMode::Owner {
        return Err(ApiError::Forbidden);
    }
    let resp = tokio::task::spawn_blocking(move || -> Result<PublishRequestResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;

        let resource = db
            .find_resource_by_name_for_user(ResourceKind::Skill, &name, Some(&user.user_id))
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;

        // Hard-gate: the row must be the caller's PRIVATE skill, not a
        // shadowed public row. find_resource_by_name_for_user can return
        // public rows when the caller has no private of that name; reject
        // those by checking owner_user_id matches.
        if resource.owner_user_id.as_deref() != Some(&user.user_id) {
            return Err(ApiError::NotFound);
        }

        // Idempotent short-circuits.
        if resource.publish_status == "pending" || resource.publish_status == "approved" {
            return Ok(PublishRequestResp {
                name,
                publish_status: resource.publish_status,
            });
        }

        // Enrich gate: resource_ai_summary must have a non-empty summary
        // before we let the user submit. Until enrich lands, the LLM
        // doesn't know what the skill is about and the admin has nothing
        // to review beyond the user-provided description.
        let summary = db
            .skill_ai_index_for_resource(&resource)
            .map_err(ApiError::Internal)?
            .map(|row| row.summary)
            .unwrap_or_default();
        if summary.trim().is_empty() {
            return Err(ApiError::BadRequest(
                "富集 (enrich) 还未完成,暂不能申请发布。等几分钟再试 — \
                 或运行 `runai recommend enrich --name <name>` 手动触发。"
                    .into(),
            ));
        }

        db.set_publish_status(&resource.id, "pending")
            .map_err(ApiError::Internal)?;

        Ok(PublishRequestResp {
            name,
            publish_status: "pending".to_string(),
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

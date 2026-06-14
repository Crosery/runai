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
    extract::{Multipart, State},
    http::HeaderMap,
};
use serde::Serialize;
use std::sync::Arc;

use crate::core::manager::SkillManager;
use crate::core::paths::AppPaths;
use crate::core::server_mode::ServerMode;

use super::community::{extract_targz, is_safe_skill_name, move_skill_payload};
use super::error::ApiError;
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

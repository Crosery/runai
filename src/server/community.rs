//! Community-market handlers (PLANNING.md §1.4).
//!
//! Team-mode social skill pool: any authenticated user can browse the
//! catalog, fetch metadata, download the raw bundle, install a copy into
//! their own private pool, or delete their own uploads (admins can delete
//! anything). **Direct upload (`api_community_upload`) is admin-only**
//! (issue #29) — regular users go through the private-pool draft →
//! enrich → publish-request → admin-approve workflow in `private_upload.rs`
//! + `admin.rs` instead; see the doc comment on `api_community_upload`.
//!
//! Physical layout (see also [`crate::core::paths::community_dir`]):
//! ```text
//! <data>/community/
//!   <uploader_uid>/
//!     <skill_name>/
//!       SKILL.md
//!       scripts/...
//!       references/...
//! ```
//!
//! The PK contract — `(uploader_uid, name)` — lets the same `name` exist
//! across multiple uploaders without collision. Re-uploading the same name
//! by the same uploader bumps `version` (a timestamp-derived monotonic
//! string) and rewrites the on-disk payload in place; `installs_total` is
//! preserved across version bumps.
//!
//! **All endpoints are owner mode 404 + team mode allow** — the social
//! pool only exists in team mode. Owner mode is single-user self-serve and
//! has no concept of "other users to share with".

use anyhow::{Context, Result};
use axum::{
    Json,
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::db::CommunitySort;
use crate::core::manager::SkillManager;
use crate::core::paths::AppPaths;
use crate::core::server_mode::ServerMode;

use super::error::ApiError;
use super::state::{AppState, require_admin, require_user};

// ─── helpers ─────────────────────────────────────────────────────────────

/// Guard: every community endpoint is team-mode-only.
fn require_team_mode(state: &AppState) -> Result<(), ApiError> {
    if state.mode == ServerMode::Owner {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

/// Skill names accepted by the community market. Must match the same
/// shape we accept for local skill names so an installer doesn't choke
/// on a name it pulled out of the social pool:
///   - non-empty, ≤ 64 chars
///   - ascii alphanumeric + `_` + `-` + `.`
///   - no leading dot (would hide the dir on unix listings)
pub(super) fn is_safe_skill_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    if s.starts_with('.') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

/// Re-tar + gzip a directory tree into a `Vec<u8>` (same primitive as
/// `/skills/bundle/{name}`). Pure-Rust, no `tar` subprocess. The in-archive
/// top-level prefix is the skill name so `tar xzf` produces
/// `<name>/SKILL.md` not bare files.
fn tar_gz_dir(dir: &std::path::Path, archive_prefix: &str) -> Result<Vec<u8>> {
    let buf: Vec<u8> = Vec::new();
    let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.follow_symlinks(false);
    tar.append_dir_all(archive_prefix, dir)
        .with_context(|| format!("tar walk {}", dir.display()))?;
    let enc = tar.into_inner()?;
    let gz_bytes = enc.finish()?;
    Ok(gz_bytes)
}

/// Extract a gzipped tar into `dest`. Errors propagate. The caller is
/// responsible for verifying the extracted content (e.g. presence of
/// `SKILL.md`).
pub(super) fn extract_targz(bytes: &[u8], dest: &std::path::Path) -> Result<()> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive
        .unpack(dest)
        .with_context(|| format!("unpack tar into {}", dest.display()))?;
    Ok(())
}

/// Walk `dest` looking for the directory that contains `SKILL.md`.
/// Returns the path of the first such directory found, depth-first.
/// `None` when no `SKILL.md` is present anywhere in the tree.
fn find_skill_root(dest: &std::path::Path) -> Option<std::path::PathBuf> {
    if dest.join("SKILL.md").is_file() {
        return Some(dest.to_path_buf());
    }
    let read = std::fs::read_dir(dest).ok()?;
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir()
            && let Some(found) = find_skill_root(&path)
        {
            return Some(found);
        }
    }
    None
}

/// Normalise a skill payload at `staging` into the canonical
/// `<final_dir>/SKILL.md + scripts/...` shape. Most uploads will already
/// have either `SKILL.md` at the top OR a single `<name>/SKILL.md`
/// subdirectory (the common case — the user `tar -czf bundle.tar.gz <name>/`).
/// If the SKILL.md lives one level down, we move that subdir's contents up
/// so the on-disk layout matches what every other code path expects.
pub(super) fn move_skill_payload(
    staging: &std::path::Path,
    final_dir: &std::path::Path,
) -> Result<()> {
    let skill_root = find_skill_root(staging)
        .ok_or_else(|| anyhow::anyhow!("uploaded archive does not contain SKILL.md"))?;
    if final_dir.exists() {
        std::fs::remove_dir_all(final_dir)
            .with_context(|| format!("clear existing {}", final_dir.display()))?;
    }
    if let Some(parent) = final_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&skill_root, final_dir)
        .with_context(|| format!("rename {} → {}", skill_root.display(), final_dir.display()))?;
    Ok(())
}

// ─── upload ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct UploadResp {
    uploader_uid: String,
    name: String,
    version: String,
    bytes: usize,
}

/// POST /api/community/upload — **admin-only** direct-to-community-pool
/// upload, bypassing the private-pool draft → enrich → publish-request →
/// approve workflow (PLANNING §1.4 rewrite).
///
/// This is NOT the user-facing upload entrypoint. Regular users go through
/// `POST /api/users/me/skills/upload` (private pool, `publish_status='draft'`)
/// followed by `POST /api/users/me/skills/{name}/publish-request` and an
/// admin `approve`. This endpoint stays mounted only for an admin who wants
/// to seed the community pool directly (e.g. bootstrapping, or restoring a
/// skill from a backup) without round-tripping through their own private
/// pool. A non-admin caller gets 403 — see issue #29.
///
/// multipart/form-data body. Required parts:
///   - `name`: skill name (string field)
///   - `bundle`: the gzipped tar of the skill directory (file field)
///
/// Side effects: writes / replaces `<data>/community/<uid>/<name>/`,
/// upserts a `community_skills` row with a monotonic `version` string
/// (Unix timestamp). Re-upload by the same uploader is a version bump.
pub(super) async fn api_community_upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<UploadResp>, ApiError> {
    require_team_mode(&state)?;

    // Resolve uploader before reading multipart (cheap fail-fast). Admin
    // only — see the doc comment above and issue #29.
    let user = {
        let db = state.db().map_err(ApiError::Internal)?;
        require_admin(&headers, &db)?
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
                // Drain unknown field so the multipart stream stays in sync.
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

    let resp = tokio::task::spawn_blocking(move || -> Result<UploadResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let paths = AppPaths::with_base(
            state
                .db_path
                .parent()
                .expect("db_path has parent")
                .to_path_buf(),
        );

        let uploader_dir = paths
            .community_uploader_dir(&user.user_id)
            .map_err(ApiError::Internal)?;
        std::fs::create_dir_all(&uploader_dir).map_err(|e| ApiError::Internal(e.into()))?;

        // Extract into a per-request staging dir so a malformed archive
        // never partially overwrites a previously-good payload.
        let staging = tempfile::tempdir_in(&uploader_dir)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("staging tempdir: {e}")))?;
        if let Err(e) = extract_targz(&bytes, staging.path()) {
            return Err(ApiError::BadRequest(format!("invalid tar.gz bundle: {e}")));
        }

        let final_dir = paths
            .community_skill_dir(&user.user_id, &name)
            .map_err(ApiError::Internal)?;
        move_skill_payload(staging.path(), &final_dir)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;

        // Monotonic version derived from the upload timestamp. Re-uploads
        // by the same uploader bump this; cross-uploader collisions are
        // impossible because the PK includes uploader_uid.
        let version = format!("v{}", chrono::Utc::now().timestamp());
        db.upsert_community_skill(&user.user_id, &name, &version)
            .map_err(ApiError::Internal)?;

        Ok(UploadResp {
            uploader_uid: user.user_id,
            name,
            version,
            bytes: bytes.len(),
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;

    Ok(Json(resp))
}

// ─── list / detail ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct ListQuery {
    #[serde(default)]
    sort: Option<String>,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Serialize)]
pub(super) struct CommunityListItem {
    uploader_uid: String,
    name: String,
    version: String,
    installs_total: i64,
    created_at: i64,
    updated_at: i64,
}

#[derive(Serialize)]
pub(super) struct CommunityListResp {
    total: usize,
    offset: usize,
    limit: usize,
    items: Vec<CommunityListItem>,
}

/// GET /api/community/list?sort={installs|created|name}&offset=&limit=
///
/// Open to any authenticated user (team mode only). Browse-only, no
/// side-effects.
pub(super) async fn api_community_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<ListQuery>,
) -> Result<Json<CommunityListResp>, ApiError> {
    require_team_mode(&state)?;

    let resp = tokio::task::spawn_blocking(move || -> Result<CommunityListResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let _ = require_user(&headers, &db)?;
        let sort = CommunitySort::parse(q.sort.as_deref());
        let offset = q.offset.unwrap_or(0);
        let limit = q.limit.unwrap_or(50).clamp(1, 500);
        let rows = db
            .list_community_skills(sort, offset, limit)
            .map_err(ApiError::Internal)?;
        let total = db.count_community_skills().map_err(ApiError::Internal)?;
        Ok(CommunityListResp {
            total,
            offset,
            limit,
            items: rows
                .into_iter()
                .map(|s| CommunityListItem {
                    uploader_uid: s.uploader_uid,
                    name: s.name,
                    version: s.version,
                    installs_total: s.installs_total,
                    created_at: s.created_at,
                    updated_at: s.updated_at,
                })
                .collect(),
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Serialize)]
pub(super) struct CommunityDetailResp {
    uploader_uid: String,
    name: String,
    version: String,
    installs_total: i64,
    created_at: i64,
    updated_at: i64,
    readme: String,
    /// True when SKILL.md was truncated to avoid huge payloads on the API.
    readme_truncated: bool,
    readme_size: usize,
}

/// GET /api/community/skill/{uid}/{name} — detail page, includes SKILL.md.
pub(super) async fn api_community_skill_detail(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((uid, name)): Path<(String, String)>,
) -> Result<Json<CommunityDetailResp>, ApiError> {
    require_team_mode(&state)?;

    let resp = tokio::task::spawn_blocking(move || -> Result<CommunityDetailResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let _ = require_user(&headers, &db)?;
        let paths = AppPaths::with_base(
            state
                .db_path
                .parent()
                .expect("db_path has parent")
                .to_path_buf(),
        );

        let row = db
            .get_community_skill(&uid, &name)
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;

        let skill_dir = paths
            .community_skill_dir(&uid, &name)
            .map_err(ApiError::Internal)?;
        let skill_md_path = skill_dir.join("SKILL.md");
        const MAX_BYTES: usize = 60_000;
        let (readme, truncated, total) = match std::fs::read_to_string(&skill_md_path) {
            Ok(body) => {
                let total = body.len();
                if total > MAX_BYTES {
                    let trunc: String = body.chars().take(MAX_BYTES).collect();
                    (trunc, true, total)
                } else {
                    (body, false, total)
                }
            }
            Err(_) => (String::new(), false, 0),
        };

        Ok(CommunityDetailResp {
            uploader_uid: row.uploader_uid,
            name: row.name,
            version: row.version,
            installs_total: row.installs_total,
            created_at: row.created_at,
            updated_at: row.updated_at,
            readme,
            readme_truncated: truncated,
            readme_size: total,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

// ─── download ────────────────────────────────────────────────────────────

enum DownloadErr {
    Unauthorized,
    NotFound,
    Internal(anyhow::Error),
}

/// GET /api/community/download/{uid}/{name} — raw gz tar of the skill
/// directory, same shape as `/skills/bundle/{name}`. Re-tarred on demand
/// so the on-disk representation stays a normal tree (easier to edit /
/// inspect on the server side).
pub(super) async fn api_community_download(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((uid, name)): Path<(String, String)>,
) -> Response {
    if let Err(e) = require_team_mode(&state) {
        return e.into_response();
    }

    let download_name = name.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, DownloadErr> {
        let db = state.db().map_err(DownloadErr::Internal)?;
        super::state::require_user(&headers, &db).map_err(|_| DownloadErr::Unauthorized)?;
        let paths = AppPaths::with_base(
            state
                .db_path
                .parent()
                .expect("db_path has parent")
                .to_path_buf(),
        );
        let row = db
            .get_community_skill(&uid, &name)
            .map_err(DownloadErr::Internal)?;
        if row.is_none() {
            return Err(DownloadErr::NotFound);
        }
        let skill_dir = paths
            .community_skill_dir(&uid, &name)
            .map_err(DownloadErr::Internal)?;
        if !skill_dir.is_dir() {
            return Err(DownloadErr::NotFound);
        }
        tar_gz_dir(&skill_dir, &name).map_err(DownloadErr::Internal)
    })
    .await;

    match join {
        Ok(Ok(bytes)) => {
            let disposition = format!("attachment; filename=\"{download_name}.tar.gz\"");
            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/gzip")
                .header(header::CONTENT_DISPOSITION, disposition)
                .body(axum::body::Body::from(bytes))
                .unwrap()
        }
        Ok(Err(DownloadErr::Unauthorized)) => ApiError::Unauthorized.into_response(),
        Ok(Err(DownloadErr::NotFound)) => ApiError::NotFound.into_response(),
        Ok(Err(DownloadErr::Internal(e))) => {
            eprintln!("/api/community/download: {e:#}");
            ApiError::Internal(e).into_response()
        }
        Err(e) => {
            eprintln!("/api/community/download: spawn_blocking join failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                String::from("internal error\n"),
            )
                .into_response()
        }
    }
}

// ─── install ─────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct CommunityInstallResp {
    /// The caller's user_id (where the skill landed).
    installed_for: String,
    /// The skill name as installed in the caller's private pool.
    name: String,
    /// The original uploader_uid + version (for audit / display).
    from_uploader: String,
    from_version: String,
    library_size: usize,
}

/// POST /api/community/install/{uid}/{name}
///
/// Copies `<data>/community/<uid>/<name>/` → `<data>/users/<caller>/skills/<name>/`,
/// registers a `Resource` row with `owner_user_id = caller`, auto-subscribes
/// the caller's library to the name, and bumps the community
/// `installs_total` counter. The caller and the uploader can be the same
/// user (installing their own upload) — that's allowed.
///
/// If the caller already has a private skill with the same name, the row
/// is replaced (this matches the upsert semantics callers expect from
/// "install a fresh copy").
pub(super) async fn api_community_install(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((uid, name)): Path<(String, String)>,
) -> Result<Json<CommunityInstallResp>, ApiError> {
    require_team_mode(&state)?;

    let resp = tokio::task::spawn_blocking(move || -> Result<CommunityInstallResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let caller = require_user(&headers, &db)?;
        let paths = AppPaths::with_base(
            state
                .db_path
                .parent()
                .expect("db_path has parent")
                .to_path_buf(),
        );

        let row = db
            .get_community_skill(&uid, &name)
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;
        let source_dir = paths
            .community_skill_dir(&uid, &name)
            .map_err(ApiError::Internal)?;
        if !source_dir.is_dir() {
            return Err(ApiError::NotFound);
        }

        paths
            .ensure_user_dirs(&caller.user_id)
            .map_err(ApiError::Internal)?;
        let caller_skills_root = paths
            .user_skills_dir(&caller.user_id)
            .map_err(ApiError::Internal)?;
        let dest = caller_skills_root.join(&name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("clear existing dest: {e}")))?;
        }
        copy_dir_recursive(&source_dir, &dest).map_err(ApiError::Internal)?;

        // Register the freshly-copied skill into the caller's private pool.
        // Re-using SkillManager keeps Source::Local + id generation +
        // description extraction consistent with every other install path.
        let mgr = SkillManager::with_base(
            state
                .db_path
                .parent()
                .expect("db_path has parent")
                .to_path_buf(),
        )
        .map_err(ApiError::Internal)?;
        // register_local_skill_for reads from <data>/users/<uid>/skills/<name>/
        // which is exactly where we just copied the payload — see the move
        // in copy_dir_recursive above.
        mgr.register_local_skill_for(&name, Some(&caller.user_id))
            .map_err(ApiError::Internal)?;

        // Auto-subscribe the caller's library so the new skill shows up on
        // their dashboard / `/recommend` candidate set immediately.
        let _ = db.library_add(&caller.user_id, &name);

        // Bump the community-wide install counter on the source row.
        let _ = db.increment_community_installs(&uid, &name);

        let library_size = db.library_count(&caller.user_id).unwrap_or(0);

        Ok(CommunityInstallResp {
            installed_for: caller.user_id,
            name,
            from_uploader: row.uploader_uid,
            from_version: row.version,
            library_size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

/// Bog-standard recursive copy. We can't `rename` across the
/// `community/` → `users/<uid>/skills/` boundary because both can be on
/// the same filesystem but we MUST keep the source intact (other users
/// might install it next). Symlinks are followed-and-copied as files;
/// the community payload should not contain symlinks in practice.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ─── delete ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub(super) struct CommunityDeleteResp {
    uploader_uid: String,
    name: String,
}

/// DELETE /api/community/skill/{uid}/{name}
///
/// Allowed when the caller is either the uploader OR a global admin.
/// Anyone else gets 403. Removes the on-disk payload and the DB row;
/// existing private installs in users' pools are left untouched (that's
/// the whole point — installing makes you owner of your own copy).
pub(super) async fn api_community_delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((uid, name)): Path<(String, String)>,
) -> Result<Json<CommunityDeleteResp>, ApiError> {
    require_team_mode(&state)?;

    let resp = tokio::task::spawn_blocking(move || -> Result<CommunityDeleteResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let caller = require_user(&headers, &db)?;
        if caller.user_id != uid && !caller.is_admin {
            // Not the uploader and not admin — explicit 403, not 404, so
            // the dashboard can render a meaningful error.
            return Err(ApiError::Forbidden);
        }

        let paths = AppPaths::with_base(
            state
                .db_path
                .parent()
                .expect("db_path has parent")
                .to_path_buf(),
        );

        let row = db
            .get_community_skill(&uid, &name)
            .map_err(ApiError::Internal)?
            .ok_or(ApiError::NotFound)?;

        // DB row first so future GETs 404 immediately even if the fs
        // delete races slow.
        db.delete_community_skill(&uid, &name)
            .map_err(ApiError::Internal)?;
        let skill_dir = paths
            .community_skill_dir(&uid, &name)
            .map_err(ApiError::Internal)?;
        if skill_dir.exists() {
            std::fs::remove_dir_all(&skill_dir)
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("remove payload: {e}")))?;
        }
        Ok(CommunityDeleteResp {
            uploader_uid: row.uploader_uid,
            name: row.name,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

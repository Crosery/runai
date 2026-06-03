//! GitHub repo parse + install handlers (the "paste a repo, pick a subset,
//! import" dashboard flow). Split out of `market.rs` to keep both files under
//! the size ceiling; shares `InstallResp` + `spawn_enrich` with its sibling.

use anyhow::Result;
use axum::{Json, extract::State, http::HeaderMap};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::cli_target::CliTarget;
use crate::core::manager::SkillManager;
use crate::core::market::{self as mkt};

use super::error::ApiError;
use super::market::{InstallResp, spawn_enrich};
use super::state::{AppState, require_user};

#[derive(Deserialize)]
pub(super) struct GithubInstallReq {
    /// Either "owner/repo" or "owner/repo@branch" or a full GitHub URL.
    /// We accept any of those shapes for paste convenience.
    source: String,
    #[serde(default)]
    branch: Option<String>,
    /// Optional whitelist of skill names to install. None / empty = all.
    /// Used by the dashboard "parse → user picks subset → install" flow.
    #[serde(default)]
    skills: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(super) struct ParseSkillView {
    name: String,
    repo_path: String,
    /// Already in resources table (someone else installed it before).
    already_installed: bool,
    /// Already in this user's library.
    in_my_library: bool,
}

#[derive(Serialize)]
pub(super) struct ParseGithubResp {
    owner: String,
    repo: String,
    branch: String,
    plugin_detected: bool,
    skills: Vec<ParseSkillView>,
}

/// POST /api/parse/github  body: {source}
/// Discovers what skills exist inside a GitHub repo WITHOUT installing
/// anything. Returns a list the dashboard renders as checkboxes so the
/// user can pick which ones to import.
pub(super) async fn api_parse_github(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<GithubInstallReq>,
) -> Result<Json<ParseGithubResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<ParseGithubResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let raw = req
            .source
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/');
        let (repo_part, parsed_branch) = if raw.contains('@') {
            let parts: Vec<&str> = raw.splitn(2, '@').collect();
            (parts[0], parts[1].to_string())
        } else {
            (raw, "main".to_string())
        };
        let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(ApiError::BadRequest(
                "expected 'owner/repo' or 'owner/repo@branch' or a full GitHub URL".into(),
            ));
        }
        let branch = req.branch.unwrap_or(parsed_branch);
        let owner = parts[0].to_string();
        let repo = parts[1].to_string();

        let source = mkt::SourceEntry::from_input(&format!("{}/{}@{}", owner, repo, branch))
            .map_err(ApiError::Internal)?;
        let rt =
            tokio::runtime::Runtime::new().map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let extract = rt
            .block_on(mkt::Market::fetch(&source))
            .map_err(|e| ApiError::BadRequest(format!("无法解析仓库: {e}")))?;

        let installed_names: std::collections::HashSet<String> = db
            .list_resources(Some(crate::core::resource::ResourceKind::Skill), None)
            .map_err(ApiError::Internal)?
            .into_iter()
            .map(|r| r.name)
            .collect();
        let in_library: std::collections::HashSet<String> = db
            .library_list(&user.user_id)
            .map_err(ApiError::Internal)?
            .into_iter()
            .collect();

        let skills = extract
            .skills
            .into_iter()
            .map(|s| ParseSkillView {
                already_installed: installed_names.contains(&s.name),
                in_my_library: in_library.contains(&s.name),
                name: s.name,
                repo_path: s.repo_path,
            })
            .collect();
        Ok(ParseGithubResp {
            owner,
            repo,
            branch,
            plugin_detected: extract.plugin_detected,
            skills,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

pub(super) async fn api_install_github(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<GithubInstallReq>,
) -> Result<Json<InstallResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<InstallResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let raw = req
            .source
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/');
        let (repo_part, parsed_branch) = if raw.contains('@') {
            let parts: Vec<&str> = raw.splitn(2, '@').collect();
            (parts[0], parts[1].to_string())
        } else {
            (raw, "main".to_string())
        };
        let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(ApiError::BadRequest(
                "expected 'owner/repo' or 'owner/repo@branch'".into(),
            ));
        }
        let branch = req.branch.unwrap_or(parsed_branch);
        let mgr = SkillManager::new().map_err(ApiError::Internal)?;
        let filter: Option<Vec<String>> = req.skills.filter(|v| !v.is_empty());
        // Phase D: dashboard-driven github install goes to the
        // authenticated user's private pool.
        let (_group, names) = mgr
            .install_github_repo_filtered_for(
                parts[0],
                parts[1],
                &branch,
                CliTarget::Claude,
                filter.as_deref(),
                Some(&user.user_id),
            )
            .map_err(ApiError::Internal)?;
        for name in &names {
            let _ = db.library_add(&user.user_id, name);
        }
        spawn_enrich(&names);
        let size = db.library_count(&user.user_id).unwrap_or(0);
        Ok(InstallResp {
            installed: names,
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

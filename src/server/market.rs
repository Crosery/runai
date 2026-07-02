//! v15 marketplace + GitHub install (any authenticated user).
//!
//! Successful install writes the skill to the installing user's private
//! pool (`<data>/users/<uid>/skills/<name>`) and auto-subscribes it to the
//! user's library so the new skill appears in their "我的库" scope
//! immediately.

use anyhow::Result;
use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::cli_target::CliTarget;
use crate::core::manager::SkillManager;
use crate::core::market::{self as mkt};
use crate::core::paths::AppPaths;

use super::error::ApiError;
use super::state::{AppState, require_user};

/// Refresh every market source's cached skill index. Concurrent fetch
/// per source; failures are silently skipped (one bad source doesn't
/// poison the rest of the index).
async fn refresh_all_sources(sources: &[mkt::SourceEntry], data_dir: &std::path::Path) {
    let mut handles = Vec::new();
    // clippy::unnecessary_to_owned false positive: `sources` is a borrowed
    // slice tied to this fn's non-'static lifetime, but each item is moved
    // into `tokio::spawn(async move { .. })`, which requires 'static. Dropping
    // `.cloned()` here does not compile (E0521: borrowed data escapes
    // outside of function) — verified by trying it directly.
    #[allow(clippy::unnecessary_to_owned)]
    for source in sources.iter().cloned() {
        let data_dir = data_dir.to_path_buf();
        handles.push(tokio::spawn(async move {
            match mkt::Market::fetch(&source).await {
                Ok(extract) => {
                    let _ = mkt::save_cache(&data_dir, &source, &extract.skills);
                    Some(extract.skills.len())
                }
                Err(_) => None,
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

#[derive(Serialize)]
pub(super) struct MarketRefreshResp {
    refreshed_sources: usize,
    total_skills: usize,
}

/// POST /api/market/refresh — re-fetch every market source's skill index.
/// Any logged-in user can trigger; admin-only would be more conservative
/// but we let users pull fresh data themselves to avoid waiting on admin.
pub(super) async fn api_market_refresh(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<MarketRefreshResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MarketRefreshResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_user(&headers, &db)?;
        let paths = AppPaths::default_path();
        let data_dir = paths.data_dir().to_path_buf();
        let sources = mkt::load_sources(&data_dir);
        let rt =
            tokio::runtime::Runtime::new().map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        rt.block_on(refresh_all_sources(&sources, &data_dir));
        // Re-tally what we got back from the freshly-written caches.
        let total: usize = sources
            .iter()
            .filter_map(|s| mkt::load_cache(&data_dir, s).map(|v| v.len()))
            .sum();
        Ok(MarketRefreshResp {
            refreshed_sources: sources.len(),
            total_skills: total,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

/// Best-effort background enrich for freshly installed / changed skills.
/// Mirrors `cli::spawn_targeted_enrich` — `recommend enrich --name N1 ...`.
///
/// stderr is INHERITED, not nulled — the enrich child's `# runai recommend
/// skipped:` / progress lines land in the server log instead of vanishing.
/// (memory: spawn_enrich 永不吞日志.) stdout stays null (it's chatty progress).
pub(super) fn spawn_enrich(names: &[String]) {
    if names.is_empty() {
        return;
    }
    // Stamp each name 富集中 so the dashboard shows the 3-state tag immediately;
    // it clears to 已富集 once the child writes the summary row.
    for n in names {
        super::enrich_state::mark_enriching(n);
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("recommend").arg("enrich");
    for n in names {
        cmd.arg("--name").arg(n);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit());
    if let Err(e) = cmd.spawn() {
        tracing::warn!("spawn_enrich failed for {names:?}: {e}");
    }
}

#[derive(Serialize)]
pub(super) struct MarketSkillView {
    name: String,
    description: String,
    source_repo: String,
    /// "owner/repo" for sources defined as GitHub repos. None for non-GitHub.
    source_label: String,
    /// e.g. "skills/bolder" relative to source_repo root.
    repo_path: String,
    /// "main" or whatever branch the source was configured with.
    branch: String,
    installed: bool,
    in_my_library: bool,
    /// Popularity signals from the skills.sh leaderboard. Zero when unknown.
    installs: u64,
    trending_installs: u64,
    hot_score: u64,
    weekly_installs: Vec<u64>,
    is_official: bool,
}

#[derive(Serialize)]
pub(super) struct MarketSourceStatus {
    label: String,
    owner: String,
    repo: String,
    cached_count: usize,
}

#[derive(Serialize)]
pub(super) struct MarketListResp {
    /// Items in the current page (post-filter, post-sort, post-paging).
    items: Vec<MarketSkillView>,
    /// Total count of items matching `q` + `sort` BEFORE the offset/limit
    /// window. Drives the pager.
    total: usize,
    /// Window offset that produced `items`. Echoed for client convenience.
    offset: usize,
    /// Window size that produced `items`. Echoed for client convenience.
    limit: usize,
    /// True when zero sources have cache on disk. Frontend should fire
    /// `/api/market/refresh` in the background to warm things up.
    needs_refresh: bool,
    /// Per-source breakdown so the user can see e.g. "Vercel: 12 cached,
    /// Anthropic: 23 cached" instead of a single opaque total.
    sources: Vec<MarketSourceStatus>,
}

#[derive(Deserialize)]
pub(super) struct MarketListQuery {
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// `all` (default) | `trending` | `hot` — sort the leaderboard server-side
    /// so the frontend doesn't need to ship every signal to do client-sorts.
    #[serde(default)]
    sort: Option<String>,
    /// Zero-based offset for pagination. Combine with `limit` to walk past
    /// the front of the leaderboard.
    #[serde(default)]
    offset: Option<usize>,
}

pub(super) async fn api_market_list(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<MarketListQuery>,
) -> Result<Json<MarketListResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MarketListResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let paths = AppPaths::default_path();
        let data_dir = paths.data_dir().to_path_buf();
        let sources = mkt::load_sources(&data_dir);
        // Never block this endpoint on GitHub. Returns whatever's cached;
        // when nothing is cached the response carries `needs_refresh: true`
        // and the frontend auto-fires /api/market/refresh in the background.
        let cached_sources = sources
            .iter()
            .filter(|s| mkt::load_cache(&data_dir, s).is_some())
            .count();
        let needs_refresh = cached_sources == 0;
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

        let q = query.q.unwrap_or_default().trim().to_lowercase();
        let limit = query.limit.unwrap_or(50).clamp(1, 500);
        let offset = query.offset.unwrap_or(0);
        let sort = query.sort.as_deref().unwrap_or("all");

        // Flatten every cached source into one searchable list. With the
        // skills.sh aggregator this is effectively the whole catalog;
        // user-added GitHub sources merge in.
        let mut all_skills: Vec<(String, mkt::MarketSkill)> = Vec::new();
        for source in &sources {
            let Some(skills) = mkt::load_cache(&data_dir, source) else {
                continue;
            };
            for s in skills {
                all_skills.push((source.label.clone(), s));
            }
        }

        // Filter on the typed query.
        let filtered: Vec<&(String, mkt::MarketSkill)> = all_skills
            .iter()
            .filter(|(_, s)| {
                if q.is_empty() {
                    return true;
                }
                s.name.to_lowercase().contains(&q) || s.source_repo.to_lowercase().contains(&q)
            })
            .collect();

        // Sort matches the user-selected tab.
        let mut sorted: Vec<&(String, mkt::MarketSkill)> = filtered.clone();
        match sort {
            "trending" => sorted.sort_by(|a, b| {
                b.1.trending_installs
                    .cmp(&a.1.trending_installs)
                    .then(a.1.name.cmp(&b.1.name))
            }),
            "hot" => sorted.sort_by(|a, b| {
                b.1.hot_score
                    .cmp(&a.1.hot_score)
                    .then(a.1.name.cmp(&b.1.name))
            }),
            _ => sorted.sort_by(|a, b| {
                b.1.installs
                    .cmp(&a.1.installs)
                    .then(a.1.name.cmp(&b.1.name))
            }),
        }

        let total = sorted.len();
        let items: Vec<MarketSkillView> = sorted
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(label, s)| MarketSkillView {
                source_label: label.clone(),
                in_my_library: in_library.contains(&s.name),
                installed: installed_names.contains(&s.name),
                description: String::new(),
                name: s.name.clone(),
                source_repo: s.source_repo.clone(),
                repo_path: s.repo_path.clone(),
                branch: s.branch.clone(),
                installs: s.installs,
                trending_installs: s.trending_installs,
                hot_score: s.hot_score,
                weekly_installs: s.weekly_installs.clone(),
                is_official: s.is_official,
            })
            .collect();

        let source_status: Vec<MarketSourceStatus> = sources
            .iter()
            .map(|s| MarketSourceStatus {
                label: s.label.clone(),
                owner: s.owner.clone(),
                repo: s.repo.clone(),
                cached_count: mkt::load_cache(&data_dir, s).map_or(0, |v| v.len()),
            })
            .collect();
        Ok(MarketListResp {
            items,
            total,
            offset,
            limit,
            needs_refresh,
            sources: source_status,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
pub(super) struct MarketInstallReq {
    name: String,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Serialize)]
pub(super) struct InstallResp {
    pub(super) installed: Vec<String>,
    pub(super) library_size: usize,
}

pub(super) async fn api_market_install(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<MarketInstallReq>,
) -> Result<Json<InstallResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<InstallResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        let user = require_user(&headers, &db)?;
        let mgr = SkillManager::new().map_err(ApiError::Internal)?;
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let sources = mkt::load_sources(&data_dir);
        let skill =
            mkt::find_skill_in_sources(&data_dir, &sources, &req.name, req.source.as_deref())
                .ok_or_else(|| {
                    ApiError::BadRequest(format!("skill '{}' not found in market", req.name))
                })?;
        let skill_name = skill.name.clone();
        // Phase D: market install through the dashboard lands in the
        // user's private pool (`<data>/users/<uid>/skills/<name>/`), not
        // the shared public pool. Symlink registration is skipped — a
        // remote dashboard user runs no local Claude Code that would
        // consume those symlinks; they pull skills via /skills/get + /skills/file.
        mgr.paths()
            .ensure_user_dirs(&user.user_id)
            .map_err(ApiError::Internal)?;

        // skills.sh aggregator: MarketSkill carries the real GitHub repo
        // in `source_repo` but no `repo_path` (skill location inside the
        // repo is unknown until we fetch the tree). Re-route through
        // install_github_repo_filtered_for so the install path fetches
        // the tree, locates the skill dir, and downloads the whole
        // bundle into the user's private pool.
        if skill.source_label == "skills.sh" {
            let (owner_part, repo_part) = match skill.source_repo.split_once('/') {
                Some((o, r)) => (o.to_string(), r.to_string()),
                None => {
                    return Err(ApiError::Internal(anyhow::anyhow!(
                        "malformed source_repo {:?} for skills.sh entry",
                        skill.source_repo
                    )));
                }
            };
            let filter = vec![skill_name.clone()];
            let _ = mgr
                .install_github_repo_filtered_for(
                    &owner_part,
                    &repo_part,
                    &skill.branch,
                    CliTarget::Claude,
                    Some(&filter),
                    Some(&user.user_id),
                )
                .map_err(ApiError::Internal)?;
            let _ = db.library_add(&user.user_id, &skill_name);
            spawn_enrich(std::slice::from_ref(&skill_name));
            let size = db.library_count(&user.user_id).unwrap_or(0);
            return Ok(InstallResp {
                installed: vec![skill_name],
                library_size: size,
            });
        }

        let install_root = mgr
            .paths()
            .user_skills_dir(&user.user_id)
            .map_err(ApiError::Internal)?;
        let rt =
            tokio::runtime::Runtime::new().map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        rt.block_on(mkt::Market::install_single(&skill, &install_root))
            .map_err(ApiError::Internal)?;
        let _ = mgr.register_local_skill_for(&skill_name, Some(&user.user_id));
        // Auto-subscribe to installing user's library.
        let _ = db.library_add(&user.user_id, &skill_name);
        spawn_enrich(std::slice::from_ref(&skill_name));
        let size = db.library_count(&user.user_id).unwrap_or(0);
        Ok(InstallResp {
            installed: vec![skill_name],
            library_size: size,
        })
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

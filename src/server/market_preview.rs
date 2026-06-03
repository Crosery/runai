//! Market preview endpoints: SKILL.md preview (multi-mirror race) and the
//! sibling-file listing (jsdelivr tree → GitHub Contents API fallback).
//! Both are read-only GETs gated on `require_user`, with on-disk caches.

use anyhow::Result;
use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::core::paths::AppPaths;

use super::error::ApiError;
use super::state::{AppState, require_user};

#[derive(Deserialize)]
pub(super) struct MarketPreviewQuery {
    /// e.g. "owner/repo"
    source_repo: String,
    /// e.g. "main"
    branch: String,
    /// e.g. "skills/bolder" — the skill dir inside the repo. Empty for
    /// skills.sh aggregator entries (the SKILL.md path inside the GitHub
    /// repo isn't known until we walk the tree); when empty, the server
    /// falls back to trying common layouts using `skill_name`.
    repo_path: String,
    /// Skill name. Required when `repo_path` is empty so the preview
    /// path candidates can be built (`<name>/SKILL.md`,
    /// `skills/<name>/SKILL.md`, etc.).
    #[serde(default)]
    skill_name: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct MarketPreviewResp {
    name: String,
    /// First ~8 KB of SKILL.md. Empty when SKILL.md is missing or fetch failed.
    skill_md: String,
    /// Convenience hint when SKILL.md couldn't be fetched.
    error: Option<String>,
}

/// GET /api/market/preview?source_repo=owner/repo&branch=main&repo_path=skills/foo
/// Fetches the skill's SKILL.md from raw.githubusercontent.com and returns
/// up to 8 KB so the dashboard can preview before installing. Done server-side
/// so the browser doesn't hit GitHub directly (browsers may be on a network
/// that can't, and a server-side fetch can be cached / rate-limited later).
pub(super) async fn api_market_preview(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<MarketPreviewQuery>,
) -> Result<Json<MarketPreviewResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MarketPreviewResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_user(&headers, &db)?;
        let parts: Vec<&str> = q.source_repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(ApiError::BadRequest(
                "source_repo must be 'owner/repo'".into(),
            ));
        }
        let owner = parts[0];
        let repo = parts[1];
        let branch = if q.branch.is_empty() { "main" } else { &q.branch };
        // Trim trailing slash; SKILL.md is appended.
        let path = q.repo_path.trim_end_matches('/');

        // Disk cache: SKILL.md previews live 1h on disk so repeat
        // clicks on the same row stay below 5ms instead of triggering
        // another mirror race. Cache hits (success or empty body) both
        // honor TTL; we keep failures only 5 min so a fluke network
        // blip doesn't poison the entry for an hour.
        let paths_cache = AppPaths::default_path();
        let cache_dir = paths_cache.data_dir().join("market-cache").join("preview-md");
        let _ = std::fs::create_dir_all(&cache_dir);
        let cache_key = format!(
            "{}__{}__{}__{}__{}.json",
            owner,
            repo,
            branch,
            path.replace('/', "_")
                .chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>(),
            q.skill_name.replace('/', "_")
                .chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>(),
        );
        let cache_path = cache_dir.join(cache_key);
        if let Ok(meta) = std::fs::metadata(&cache_path)
            && let Ok(modified) = meta.modified()
            && let Ok(age) = modified.elapsed()
            && let Ok(content) = std::fs::read_to_string(&cache_path)
            && let Ok(cached) = serde_json::from_str::<MarketPreviewResp>(&content)
        {
            let ttl = if cached.error.is_some() { 300 } else { 3600 };
            if age.as_secs() < ttl {
                return Ok(cached);
            }
        }
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(path)
            .to_string();
        // Build candidate paths inside the repo. Each path expands
        // across multiple mirrors and we race them in parallel so the
        // user always sees the SKILL.md from the fastest reachable
        // mirror. Mirror order (informational; race winner decides):
        //   - raw.githubusercontent.com (direct)
        //   - ghfast.top                (China-friendly proxy, ~1s)
        //   - cdn.jsdelivr.net          (fastly CDN, ~2s)
        //   - cdn.jsdmirror.com         (jsdelivr China mirror, fallback)
        let raw_paths: Vec<String> = if !path.is_empty() {
            vec![format!("{path}/SKILL.md")]
        } else if !q.skill_name.is_empty() {
            let n = &q.skill_name;
            vec![
                format!("{n}/SKILL.md"),
                format!("skills/{n}/SKILL.md"),
                format!("agent-skills/{n}/SKILL.md"),
                "SKILL.md".to_string(),
            ]
        } else {
            vec!["SKILL.md".to_string()]
        };
        let mut candidates: Vec<String> = Vec::new();
        for p in &raw_paths {
            candidates.push(format!("https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{p}"));
            candidates.push(format!("https://ghfast.top/https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{p}"));
            candidates.push(format!("https://cdn.jsdelivr.net/gh/{owner}/{repo}@{branch}/{p}"));
            candidates.push(format!("https://cdn.jsdmirror.com/gh/{owner}/{repo}@{branch}/{p}"));
        }
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let result: anyhow::Result<String> = rt.block_on(async {
            let client = reqwest::Client::builder()
                .user_agent("runai/0.11 (+https://github.com/Crosery/runai)")
                .timeout(std::time::Duration::from_secs(6))
                .build()?;

            // Race every candidate URL concurrently — first one that
            // returns 200 with a body wins, the rest get aborted by
            // JoinSet drop. Saves the "raw.github slow → fall back"
            // chain latency. Records 404 / err per URL for diagnostics.
            let mut set = tokio::task::JoinSet::new();
            for url in candidates.iter().cloned() {
                let client = client.clone();
                set.spawn(async move {
                    let resp = client.get(&url).send().await;
                    match resp {
                        Ok(r) if r.status().is_success() => match r.text().await {
                            Ok(body) if !body.is_empty() => Ok((url, body)),
                            Ok(_) => Err((url, "empty body".to_string())),
                            Err(e) => Err((url, e.to_string())),
                        },
                        Ok(r) => Err((url, format!("HTTP {}", r.status()))),
                        Err(e) => Err((url, e.to_string())),
                    }
                });
            }
            let mut errs: Vec<String> = Vec::new();
            while let Some(res) = set.join_next().await {
                match res {
                    Ok(Ok((_url, body))) => {
                        // Abort siblings so they don't burn bandwidth
                        // after we already have a winner.
                        set.abort_all();
                        return Ok(body);
                    }
                    Ok(Err((url, e))) => errs.push(format!("{url} → {e}")),
                    Err(e) => errs.push(format!("join: {e}")),
                }
            }
            anyhow::bail!("SKILL.md unreachable on every mirror: {}", errs.join("; "));
        });
        let (skill_md, error) = match result {
            Ok(t) => (t.chars().take(8000).collect::<String>(), None),
            Err(e) => (String::new(), Some(e.to_string())),
        };
        let resp_out = MarketPreviewResp {
            name,
            skill_md,
            error,
        };
        let _ = std::fs::write(
            &cache_path,
            serde_json::to_string(&resp_out).unwrap_or_default(),
        );
        Ok(resp_out)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

#[derive(Deserialize)]
pub(super) struct MarketPreviewFilesQuery {
    /// e.g. "owner/repo"
    source_repo: String,
    /// Branch on GitHub. Defaults to "main".
    #[serde(default)]
    branch: String,
    /// e.g. "find-skills" — used to build layout candidates when repo_path is empty.
    skill_name: String,
    /// Optional pre-known path; when supplied, only that layout is tried.
    #[serde(default)]
    repo_path: String,
}

#[derive(Serialize, Deserialize)]
pub(super) struct MarketPreviewFile {
    /// Path relative to the skill root, forward-slashes.
    path: String,
    size: u64,
    is_dir: bool,
}

#[derive(Serialize, Deserialize)]
pub(super) struct MarketPreviewFilesResp {
    /// Path inside the GitHub repo that produced this listing (the layout
    /// that hit). Empty when nothing matched.
    matched_path: String,
    entries: Vec<MarketPreviewFile>,
    error: Option<String>,
}

/// GET /api/market/preview-files?source_repo=&branch=&skill_name=
/// Lists the contents of a skill's GitHub directory via the unauthenticated
/// GitHub Contents API. Used by the Market detail modal to show what
/// sibling files (scripts/, references/, etc.) the skill ships with.
/// Tries the same `<n>/SKILL.md` / `skills/<n>/SKILL.md` / `agent-skills/<n>/SKILL.md`
/// layout chain as `api_market_preview`, stopping at the first 200.
pub(super) async fn api_market_preview_files(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(q): Query<MarketPreviewFilesQuery>,
) -> Result<Json<MarketPreviewFilesResp>, ApiError> {
    let resp = tokio::task::spawn_blocking(move || -> Result<MarketPreviewFilesResp, ApiError> {
        let db = state.db().map_err(ApiError::Internal)?;
        require_user(&headers, &db)?;
        let parts: Vec<&str> = q.source_repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Err(ApiError::BadRequest("source_repo must be 'owner/repo'".into()));
        }
        let owner = parts[0];
        let repo = parts[1];
        let branch = if q.branch.is_empty() { "main" } else { &q.branch };
        let candidates: Vec<String> = if !q.repo_path.is_empty() {
            vec![q.repo_path.trim_matches('/').to_string()]
        } else if !q.skill_name.is_empty() {
            let n = &q.skill_name;
            // Last entry is "" → root-skill repos where SKILL.md
            // sits at the repository root (e.g. anysearch-ai/anysearch-skill).
            vec![
                n.clone(),
                format!("skills/{n}"),
                format!("agent-skills/{n}"),
                String::new(),
            ]
        } else {
            vec![String::new()]
        };

        // Disk cache lookup — preview-files results live 1h on disk so
        // repeat clicks on the same skill don't burn the GitHub API
        // budget (60 req/h unauth, 5000/h with GITHUB_TOKEN).
        let paths = AppPaths::default_path();
        let cache_dir = paths.data_dir().join("market-cache").join("preview-files");
        let _ = std::fs::create_dir_all(&cache_dir);
        let cache_key = format!(
            "{}__{}__{}__{}.json",
            owner,
            repo,
            branch,
            q.repo_path.replace('/', "_")
                .chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .collect::<String>()
        );
        let cache_path = cache_dir.join(cache_key);
        if let Ok(meta) = std::fs::metadata(&cache_path)
            && let Ok(modified) = meta.modified()
            && let Ok(age) = modified.elapsed()
            && let Ok(content) = std::fs::read_to_string(&cache_path)
            && let Ok(cached) = serde_json::from_str::<MarketPreviewFilesResp>(&content)
        {
            // Success cached 1h; failures cached 5min so a one-time
            // rate-limit / 404 doesn't keep retrying every click.
            let ttl = if cached.error.is_some() { 300 } else { 3600 };
            if age.as_secs() < ttl {
                return Ok(cached);
            }
        }

        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;
        let result: anyhow::Result<(String, Vec<MarketPreviewFile>)> = rt.block_on(async {
            let client = reqwest::Client::builder()
                .user_agent("runai/0.11 (+https://github.com/Crosery/runai)")
                .timeout(std::time::Duration::from_secs(12))
                .build()?;

            // Primary: jsdelivr fastly endpoint (data.jsdelivr.com — the
            // .net mirror has flaky DNS on some networks). One repo-wide
            // tree fetch, no auth, no rate limit. Walk in-memory to find
            // the matching skill directory.
            let jsd_url = format!(
                "https://data.jsdelivr.com/v1/packages/gh/{owner}/{repo}@{branch}?structure=tree"
            );
            let jsd_err: String = match client
                .get(&jsd_url)
                .header("accept", "application/json")
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let body: serde_json::Value = r.json().await?;
                    let root_files = body
                        .get("files")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    fn find_dir<'a>(
                        nodes: &'a [serde_json::Value],
                        want: &[&str],
                    ) -> Option<&'a [serde_json::Value]> {
                        if want.is_empty() {
                            return None;
                        }
                        for n in nodes {
                            if n.get("type").and_then(|v| v.as_str()) != Some("directory") {
                                continue;
                            }
                            let name = n.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            if name == want[0] {
                                if want.len() == 1 {
                                    return n.get("files").and_then(|v| v.as_array()).map(|a| a.as_slice());
                                }
                                if let Some(child) = n.get("files").and_then(|v| v.as_array())
                                    && let Some(hit) = find_dir(child, &want[1..])
                                {
                                    return Some(hit);
                                }
                            }
                        }
                        None
                    }
                    for path in &candidates {
                        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
                        // Empty segs = repo root listing. Only return
                        // root when there's a SKILL.md sitting at the
                        // top — otherwise we'd surface a generic monorepo
                        // root for a non-root-skill query.
                        let dir_files: &[serde_json::Value] = if segs.is_empty() {
                            let root_has_skill_md = root_files.iter().any(|n| {
                                n.get("type").and_then(|v| v.as_str()) == Some("file")
                                    && n.get("name").and_then(|v| v.as_str()) == Some("SKILL.md")
                            });
                            if !root_has_skill_md { continue; }
                            &root_files
                        } else {
                            match find_dir(&root_files, &segs) {
                                Some(v) => v,
                                None => continue,
                            }
                        };
                        let mut entries: Vec<MarketPreviewFile> = dir_files
                            .iter()
                            .filter_map(|n| {
                                let name = n.get("name").and_then(|v| v.as_str())?.to_string();
                                let is_dir = n.get("type").and_then(|v| v.as_str()) == Some("directory");
                                let size = n.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                                // For root listings, hide the housekeeping noise.
                                if segs.is_empty()
                                    && !crate::core::market::is_root_skill_payload(&name)
                                {
                                    return None;
                                }
                                Some(MarketPreviewFile { path: name, size, is_dir })
                            })
                            .collect();
                        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.path.cmp(&b.path)));
                        return Ok((path.clone(), entries));
                    }
                    format!("jsdelivr tree had no matching dir for {candidates:?}")
                }
                Ok(r) => format!("jsdelivr HTTP {}", r.status()),
                Err(e) => format!("jsdelivr request: {e}"),
            };

            // Fallback: GitHub Contents API. Unauthenticated 60/h limit;
            // bumps to 5000/h when GITHUB_TOKEN env is set on the server.
            let token = std::env::var("GITHUB_TOKEN").ok();
            let mut last_err: Option<String> = Some(format!("jsdelivr → {jsd_err}; github →"));
            for path in &candidates {
                let url = if path.is_empty() {
                    format!("https://api.github.com/repos/{owner}/{repo}/contents?ref={branch}")
                } else {
                    format!("https://api.github.com/repos/{owner}/{repo}/contents/{path}?ref={branch}")
                };
                let mut req = client.get(&url).header("accept", "application/vnd.github+json");
                if let Some(t) = &token {
                    req = req.header("authorization", format!("Bearer {t}"));
                }
                let resp = req.send().await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let arr: serde_json::Value = r.json().await?;
                        let mut entries: Vec<MarketPreviewFile> = Vec::new();
                        if let Some(items) = arr.as_array() {
                            for item in items {
                                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let kind = item.get("type").and_then(|v| v.as_str()).unwrap_or("file");
                                let size = item.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                                if name.is_empty() { continue; }
                                entries.push(MarketPreviewFile {
                                    path: name.to_string(),
                                    size,
                                    is_dir: kind == "dir",
                                });
                            }
                        }
                        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.path.cmp(&b.path)));
                        return Ok((path.clone(), entries));
                    }
                    Ok(r) if r.status().as_u16() == 403 => {
                        anyhow::bail!("{} GitHub 403 (rate-limited)", last_err.unwrap_or_default());
                    }
                    Ok(r) if r.status().as_u16() == 404 => {
                        last_err = Some(format!("{} 404 at {path:?}", last_err.unwrap_or_default()));
                    }
                    Ok(r) => {
                        last_err = Some(format!("{} HTTP {} at {path:?}", last_err.unwrap_or_default(), r.status()));
                    }
                    Err(e) => {
                        last_err = Some(format!("{} {e}", last_err.unwrap_or_default()));
                    }
                }
            }
            anyhow::bail!("file listing failed: {}", last_err.unwrap_or_default());
        });
        let (matched_path, entries, error) = match result {
            Ok((p, e)) => (p, e, None),
            Err(e) => (String::new(), Vec::new(), Some(e.to_string())),
        };
        let resp_out = MarketPreviewFilesResp { matched_path, entries, error };
        // Cache successful results (and short-lived 404s) to keep the
        // 60-req/h GitHub API budget from getting blown.
        let _ = std::fs::write(
            &cache_path,
            serde_json::to_string(&resp_out).unwrap_or_default(),
        );
        Ok(resp_out)
    })
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))??;
    Ok(Json(resp))
}

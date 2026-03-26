use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A market source entry — built-in or user-added, can be enabled/disabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceEntry {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub skill_prefix: String,
    pub label: String,
    pub description: String,
    pub builtin: bool,
    pub enabled: bool,
}

impl SourceEntry {
    fn builtin(
        owner: &str,
        repo: &str,
        branch: &str,
        prefix: &str,
        label: &str,
        desc: &str,
        enabled: bool,
    ) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            branch: branch.into(),
            skill_prefix: prefix.into(),
            label: label.into(),
            description: desc.into(),
            builtin: true,
            enabled,
        }
    }

    /// Parse "owner/repo" or "owner/repo@branch" into a user-added source.
    pub fn from_input(input: &str) -> Result<Self> {
        let input = input
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/');
        let (repo_part, branch) = if input.contains('@') {
            let parts: Vec<&str> = input.splitn(2, '@').collect();
            (parts[0], parts[1].to_string())
        } else {
            (input, "main".to_string())
        };
        let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
        if parts.len() != 2 {
            bail!("expected 'owner/repo', got '{repo_part}'");
        }
        Ok(Self {
            label: format!("{}/{}", parts[0], parts[1]),
            owner: parts[0].into(),
            repo: parts[1].into(),
            branch,
            skill_prefix: String::new(),
            description: "User-added source".into(),
            builtin: false,
            enabled: true,
        })
    }

    pub fn repo_id(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// Default built-in sources. First two enabled by default.
fn builtin_sources() -> Vec<SourceEntry> {
    vec![
        SourceEntry::builtin(
            "anthropics",
            "claude-plugins-official",
            "main",
            "",
            "Anthropic Official",
            "Official Claude plugins & skills (23)",
            true,
        ),
        SourceEntry::builtin(
            "affaan-m",
            "everything-claude-code",
            "main",
            "skills/",
            "Everything Claude Code",
            "Community skills collection (120+)",
            true,
        ),
        SourceEntry::builtin(
            "TerminalSkills",
            "skills",
            "main",
            "skills/",
            "Terminal Skills",
            "Open-source skill library (900+)",
            false,
        ),
        SourceEntry::builtin(
            "sickn33",
            "antigravity-awesome-skills",
            "main",
            "skills/",
            "Antigravity Skills",
            "Agentic skills collection (1300+)",
            false,
        ),
        SourceEntry::builtin(
            "mxyhi",
            "ok-skills",
            "main",
            "",
            "OK Skills",
            "Curated agent skills & playbooks (55)",
            false,
        ),
    ]
}

const SOURCES_FILE: &str = "market-sources.json";

/// Load source list: merge built-ins with user state.
pub fn load_sources(data_dir: &Path) -> Vec<SourceEntry> {
    let path = data_dir.join(SOURCES_FILE);
    let saved: Vec<SourceEntry> = if path.exists() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut result: Vec<SourceEntry> = Vec::new();

    // Merge built-in sources: use saved enabled state if available
    for b in builtin_sources() {
        let enabled = saved
            .iter()
            .find(|s| s.builtin && s.repo_id() == b.repo_id())
            .map(|s| s.enabled)
            .unwrap_or(b.enabled);
        let mut entry = b;
        entry.enabled = enabled;
        result.push(entry);
    }

    // Append user-added sources
    for s in &saved {
        if !s.builtin {
            result.push(s.clone());
        }
    }

    result
}

/// Save source list.
pub fn save_sources(data_dir: &Path, sources: &[SourceEntry]) -> Result<()> {
    let path = data_dir.join(SOURCES_FILE);
    std::fs::write(&path, serde_json::to_string_pretty(sources)?)?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSkill {
    pub name: String,
    pub repo_path: String, // e.g. "skills/brainstorming"
    pub source_label: String,
    pub source_repo: String, // "owner/repo"
    pub branch: String,
    #[serde(skip)]
    pub installed: bool,
}

const CACHE_DIR: &str = "market-cache";
const CACHE_MAX_AGE_SECS: u64 = 3600; // 1 hour

/// Load cached skill list from disk. Returns None if missing or stale.
pub fn load_cache(data_dir: &Path, source: &SourceEntry) -> Option<Vec<MarketSkill>> {
    let path = data_dir
        .join(CACHE_DIR)
        .join(format!("{}.json", cache_key(source)));
    let meta = std::fs::metadata(&path).ok()?;
    let age = meta.modified().ok()?.elapsed().ok()?.as_secs();
    if age > CACHE_MAX_AGE_SECS {
        return None; // stale
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Save skill list to disk cache.
pub fn save_cache(data_dir: &Path, source: &SourceEntry, skills: &[MarketSkill]) -> Result<()> {
    let dir = data_dir.join(CACHE_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", cache_key(source)));
    std::fs::write(&path, serde_json::to_string(skills)?)?;
    Ok(())
}

/// Mark a source as a Claude plugin (not a skill collection).
pub fn save_plugin_marker(data_dir: &Path, source: &SourceEntry) {
    let dir = data_dir.join(CACHE_DIR);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}.plugin", cache_key(source)));
    let _ = std::fs::write(&path, &source.repo);
}

/// Check if a source was detected as a Claude plugin.
pub fn is_plugin_source(data_dir: &Path, source: &SourceEntry) -> bool {
    data_dir
        .join(CACHE_DIR)
        .join(format!("{}.plugin", cache_key(source)))
        .exists()
}

/// Find a skill in market cache by name, with optional source filter (matches label or repo_id).
pub fn find_skill_in_sources(
    data_dir: &Path,
    sources: &[SourceEntry],
    skill_name: &str,
    source_filter: Option<&str>,
) -> Option<MarketSkill> {
    for src in sources {
        if !src.enabled {
            continue;
        }
        if let Some(filter) = source_filter {
            let f = filter.to_lowercase();
            if !src.label.to_lowercase().contains(&f) && !src.repo_id().to_lowercase().contains(&f)
            {
                continue;
            }
        }
        if let Some(cached) = load_cache(data_dir, src) {
            if let Some(skill) = cached.into_iter().find(|s| s.name == skill_name) {
                return Some(skill);
            }
        }
    }
    None
}

fn cache_key(source: &SourceEntry) -> String {
    format!("{}_{}", source.owner, source.repo)
}

pub(crate) struct ExtractResult {
    pub skills: Vec<MarketSkill>,
    pub plugin_detected: bool,
    pub tree: GitTree,
}

/// A single file download task for batch concurrent downloads.
pub(crate) struct DownloadTask {
    pub skill_name: String,
    pub url: String,
    pub dest_path: std::path::PathBuf,
}

pub struct Market;

impl Market {
    /// Extract skills from a git tree. Also detects .claude-plugin format.
    pub(crate) fn extract_skills(tree: GitTree, source: &SourceEntry) -> ExtractResult {
        let label = &source.label;
        let repo_id = source.repo_id();
        let mut skills = Vec::new();
        let mut plugin_detected = false;

        for node in &tree.tree {
            if node.path.contains(".claude-plugin") {
                plugin_detected = true;
                continue;
            }

            if !node.path.ends_with("/SKILL.md") && node.path != "SKILL.md" {
                continue;
            }

            if node.path == "SKILL.md" {
                skills.push(MarketSkill {
                    name: source.repo.clone(),
                    repo_path: String::new(),
                    source_label: label.clone(),
                    source_repo: repo_id.clone(),
                    branch: source.branch.clone(),
                    installed: false,
                });
                continue;
            }

            let dir = node.path.trim_end_matches("/SKILL.md");
            let name = if !source.skill_prefix.is_empty() {
                match dir.strip_prefix(source.skill_prefix.as_str()) {
                    Some(s) => s.rsplit('/').next().unwrap_or(s).to_string(),
                    None => continue,
                }
            } else {
                dir.rsplit('/').next().unwrap_or(dir).to_string()
            };

            if name.is_empty() {
                continue;
            }

            skills.push(MarketSkill {
                name,
                repo_path: dir.to_string(),
                source_label: label.clone(),
                source_repo: repo_id.clone(),
                branch: source.branch.clone(),
                installed: false,
            });
        }

        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills.dedup_by(|a, b| a.name == b.name);
        ExtractResult {
            skills,
            plugin_detected,
            tree,
        }
    }

    /// Fetch skill list from GitHub API.
    pub(crate) async fn fetch(source: &SourceEntry) -> Result<ExtractResult> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
            source.owner, source.repo, source.branch,
        );

        let client = reqwest::Client::builder().user_agent("runai/0.5").build()?;

        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!(
                "GitHub API {} for {}/{}",
                resp.status(),
                source.owner,
                source.repo
            );
        }

        let body: GitTree = resp.json().await?;
        Ok(Self::extract_skills(body, source))
    }

    /// Get all file paths belonging to a skill from the git tree.
    pub(crate) fn get_skill_files(tree: &GitTree, repo_path: &str) -> Vec<String> {
        let prefix = format!("{repo_path}/");
        tree.tree
            .iter()
            .filter(|n| n.path.starts_with(&prefix))
            .map(|n| n.path.clone())
            .collect()
    }

    /// Collect all file download tasks for all skills in an ExtractResult.
    /// No network — just builds the list of (url, dest_path) pairs.
    pub(crate) fn collect_download_tasks(
        extract: &ExtractResult,
        paths: &crate::core::paths::AppPaths,
    ) -> Vec<DownloadTask> {
        let mut tasks = Vec::new();
        for skill in &extract.skills {
            let parts: Vec<&str> = skill.source_repo.splitn(2, '/').collect();
            if parts.len() != 2 {
                continue;
            }
            let (owner, repo) = (parts[0], parts[1]);
            let repo_path = if skill.repo_path.is_empty() {
                &skill.name
            } else {
                &skill.repo_path
            };
            let files = Self::get_skill_files(&extract.tree, repo_path);
            let prefix = format!("{repo_path}/");
            let skill_dir = paths.skills_dir().join(&skill.name);

            for file_path in files {
                let url = format!(
                    "https://raw.githubusercontent.com/{owner}/{repo}/{}/{}",
                    skill.branch, file_path
                );
                let rel = file_path
                    .strip_prefix(&prefix)
                    .unwrap_or(&file_path)
                    .to_string();
                let dest_path = skill_dir.join(&rel);
                tasks.push(DownloadTask {
                    skill_name: skill.name.clone(),
                    url,
                    dest_path,
                });
            }
        }
        tasks
    }

    /// Download all tasks concurrently. Returns set of skill names that had at least one file downloaded.
    pub(crate) async fn execute_downloads(
        tasks: Vec<DownloadTask>,
    ) -> std::collections::HashSet<String> {
        let client = match reqwest::Client::builder().user_agent("runai/0.5").build() {
            Ok(c) => c,
            Err(_) => return std::collections::HashSet::new(),
        };

        let mut set = tokio::task::JoinSet::new();
        for task in tasks {
            let client = client.clone();
            set.spawn(async move {
                let result = client.get(&task.url).send().await;
                match result {
                    Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                        Ok(bytes) => (task.skill_name, task.dest_path, Some(bytes)),
                        Err(_) => (task.skill_name, task.dest_path, None),
                    },
                    _ => (task.skill_name, task.dest_path, None),
                }
            });
        }

        let mut downloaded = std::collections::HashSet::new();
        while let Some(join_result) = set.join_next().await {
            if let Ok((skill_name, dest_path, Some(content))) = join_result {
                if let Some(parent) = dest_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(&dest_path, &content).is_ok() {
                    downloaded.insert(skill_name);
                }
            }
        }
        downloaded
    }

    /// Install a single skill using git tree (fast: raw downloads, no Contents API).
    /// If tree is provided, uses it to find files; otherwise falls back to Contents API.
    pub(crate) async fn install_single_with_tree(
        skill: &MarketSkill,
        paths: &crate::core::paths::AppPaths,
        tree: Option<&GitTree>,
    ) -> Result<()> {
        let parts: Vec<&str> = skill.source_repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            bail!("invalid source_repo: {}", skill.source_repo);
        }
        let (owner, repo) = (parts[0], parts[1]);
        let client = reqwest::Client::builder().user_agent("runai/0.5").build()?;
        let skill_dir = paths.skills_dir().join(&skill.name);
        std::fs::create_dir_all(&skill_dir)?;

        let repo_path = if skill.repo_path.is_empty() {
            &skill.name
        } else {
            &skill.repo_path
        };

        if let Some(tree) = tree {
            // Fast path: concurrent raw downloads from raw.githubusercontent.com
            let files = Self::get_skill_files(tree, repo_path);
            let prefix = format!("{repo_path}/");

            // Launch all downloads concurrently using tokio JoinSet
            let mut set = tokio::task::JoinSet::new();
            for file_path in files {
                let raw_url = format!(
                    "https://raw.githubusercontent.com/{owner}/{repo}/{}/{}",
                    skill.branch, file_path
                );
                let client = client.clone();
                set.spawn(async move {
                    let resp = client
                        .get(&raw_url)
                        .send()
                        .await
                        .ok()
                        .filter(|r| r.status().is_success());
                    let bytes = match resp {
                        Some(r) => r.bytes().await.ok(),
                        None => None,
                    };
                    (file_path, bytes)
                });
            }

            // Collect results and write files to disk
            while let Some(Ok((file_path, Some(content)))) = set.join_next().await {
                let rel = file_path.strip_prefix(&prefix).unwrap_or(&file_path);
                let dest = skill_dir.join(rel);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&dest, &content)?;
            }
        } else {
            // Fallback: Contents API (slower but works without tree)
            Self::download_directory_recursive(
                &client,
                owner,
                repo,
                &skill.branch,
                repo_path,
                &skill_dir,
            )
            .await?;
        }

        Ok(())
    }

    /// Install a single skill (backwards-compatible, uses Contents API fallback).
    pub async fn install_single(
        skill: &MarketSkill,
        paths: &crate::core::paths::AppPaths,
    ) -> Result<()> {
        Self::install_single_with_tree(skill, paths, None).await
    }

    /// Recursively download all files in a GitHub directory.
    async fn download_directory_recursive(
        client: &reqwest::Client,
        owner: &str,
        repo: &str,
        branch: &str,
        api_path: &str,
        local_dir: &std::path::Path,
    ) -> Result<()> {
        let url = if api_path.is_empty() {
            format!("https://api.github.com/repos/{owner}/{repo}/contents?ref={branch}",)
        } else {
            format!("https://api.github.com/repos/{owner}/{repo}/contents/{api_path}?ref={branch}",)
        };

        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!(
                "GitHub Contents API returned HTTP {} for {}",
                resp.status(),
                url
            );
        }

        let items: Vec<GitHubContentItem> = resp.json().await?;

        for item in &items {
            match item.item_type.as_str() {
                "file" => {
                    let raw_url = format!(
                        "https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{}",
                        item.path,
                    );
                    let file_resp = client.get(&raw_url).send().await?;
                    if !file_resp.status().is_success() {
                        bail!(
                            "Failed to download {}: HTTP {}",
                            item.path,
                            file_resp.status()
                        );
                    }
                    let content = file_resp.bytes().await?;
                    let file_path = local_dir.join(&item.name);
                    std::fs::write(&file_path, &content)?;
                }
                "dir" => {
                    let sub_dir = local_dir.join(&item.name);
                    std::fs::create_dir_all(&sub_dir)?;
                    Box::pin(Self::download_directory_recursive(
                        client, owner, repo, branch, &item.path, &sub_dir,
                    ))
                    .await?;
                }
                _ => {} // skip symlinks, submodules, etc.
            }
        }

        Ok(())
    }

    pub fn mark_installed(skills: &mut [MarketSkill], installed_names: &[String]) {
        for skill in skills.iter_mut() {
            skill.installed = installed_names.iter().any(|n| n == &skill.name);
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct GitTree {
    pub(crate) tree: Vec<GitTreeNode>,
}

#[derive(Deserialize)]
pub(crate) struct GitTreeNode {
    pub(crate) path: String,
}

#[derive(Deserialize)]
struct GitHubContentItem {
    name: String,
    path: String,
    #[serde(rename = "type")]
    item_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_detects_claude_plugin_format() {
        let tree = GitTree {
            tree: vec![
                GitTreeNode {
                    path: ".claude-plugin/plugin.json".into(),
                },
                GitTreeNode {
                    path: "README.md".into(),
                },
                GitTreeNode {
                    path: "skills/brainstorming/SKILL.md".into(),
                },
            ],
        };

        let source = SourceEntry {
            owner: "test".into(),
            repo: "test-plugin".into(),
            branch: "main".into(),
            skill_prefix: String::new(),
            label: "Test".into(),
            description: "test".into(),
            builtin: false,
            enabled: true,
        };

        let result = Market::extract_skills(tree, &source);
        assert!(result.plugin_detected);
        assert_eq!(result.skills.len(), 1);
    }

    #[test]
    fn extract_file_paths_from_tree() {
        let tree = GitTree {
            tree: vec![
                GitTreeNode {
                    path: "README.md".into(),
                },
                GitTreeNode {
                    path: "find-skills/SKILL.md".into(),
                },
                GitTreeNode {
                    path: "deep-research/SKILL.md".into(),
                },
                GitTreeNode {
                    path: "deep-research/agents/openai.yaml".into(),
                },
                GitTreeNode {
                    path: "deep-research/prompts/search.md".into(),
                },
                GitTreeNode {
                    path: "other-dir/not-a-skill.txt".into(),
                },
            ],
        };

        // Get files for find-skills (single file)
        let files = Market::get_skill_files(&tree, "find-skills");
        assert_eq!(files, vec!["find-skills/SKILL.md"]);

        // Get files for deep-research (multiple files)
        let mut files = Market::get_skill_files(&tree, "deep-research");
        files.sort();
        assert_eq!(
            files,
            vec![
                "deep-research/SKILL.md",
                "deep-research/agents/openai.yaml",
                "deep-research/prompts/search.md",
            ]
        );
    }

    #[test]
    fn find_skill_in_cache_matches_by_label_and_repo_id() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path();

        // Create a source
        let source = SourceEntry {
            owner: "mxyhi".into(),
            repo: "ok-skills".into(),
            branch: "main".into(),
            skill_prefix: String::new(),
            label: "OK Skills".into(),
            description: "test".into(),
            builtin: false,
            enabled: true,
        };

        // Save cache with a skill
        let skills = vec![MarketSkill {
            name: "find-skills".into(),
            repo_path: "find-skills".into(),
            source_label: "OK Skills".into(),
            source_repo: "mxyhi/ok-skills".into(),
            branch: "main".into(),
            installed: false,
        }];
        save_cache(data_dir, &source, &skills).unwrap();

        // Find by repo_id
        let found = find_skill_in_sources(
            data_dir,
            &[source.clone()],
            "find-skills",
            Some("mxyhi/ok-skills"),
        );
        assert!(found.is_some(), "should find by repo_id");

        // Find by label
        let found = find_skill_in_sources(
            data_dir,
            &[source.clone()],
            "find-skills",
            Some("OK Skills"),
        );
        assert!(found.is_some(), "should find by label");

        // Find without source filter
        let found = find_skill_in_sources(data_dir, &[source], "find-skills", None);
        assert!(found.is_some(), "should find without filter");

        // Not found
        let found = find_skill_in_sources(data_dir, &[], "nonexistent", None);
        assert!(found.is_none(), "should not find nonexistent");
    }

    #[test]
    fn collect_download_tasks_maps_all_files_across_skills() {
        let tree = GitTree {
            tree: vec![
                GitTreeNode {
                    path: "README.md".into(),
                },
                GitTreeNode {
                    path: "skill-a/SKILL.md".into(),
                },
                GitTreeNode {
                    path: "skill-a/helper.md".into(),
                },
                GitTreeNode {
                    path: "skill-b/SKILL.md".into(),
                },
                GitTreeNode {
                    path: "skill-b/scripts/run.sh".into(),
                },
            ],
        };

        let source = SourceEntry {
            owner: "test".into(),
            repo: "repo".into(),
            branch: "main".into(),
            skill_prefix: String::new(),
            label: "Test".into(),
            description: "test".into(),
            builtin: false,
            enabled: true,
        };

        let extract = Market::extract_skills(tree, &source);
        assert_eq!(extract.skills.len(), 2);

        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::core::paths::AppPaths::with_base(tmp.path().to_path_buf());
        paths.ensure_dirs().unwrap();

        let tasks = Market::collect_download_tasks(&extract, &paths);

        // Should have 4 file tasks total (2 for skill-a, 2 for skill-b)
        assert_eq!(tasks.len(), 4, "should collect all files across all skills");

        // Verify path mapping
        let skill_a_files: Vec<_> = tasks.iter().filter(|t| t.skill_name == "skill-a").collect();
        assert_eq!(skill_a_files.len(), 2);
        assert!(
            skill_a_files
                .iter()
                .any(|t| t.dest_path.ends_with("SKILL.md"))
        );
        assert!(
            skill_a_files
                .iter()
                .any(|t| t.dest_path.ends_with("helper.md"))
        );

        // Verify URL format
        assert!(
            tasks[0]
                .url
                .starts_with("https://raw.githubusercontent.com/test/repo/main/")
        );
    }
}

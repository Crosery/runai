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
    fn builtin(owner: &str, repo: &str, branch: &str, prefix: &str, label: &str, desc: &str, enabled: bool) -> Self {
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
        let input = input.trim()
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
        SourceEntry::builtin("anthropics", "claude-plugins-official", "main", "",
            "Anthropic Official", "Official Claude plugins & skills (23)", true),
        SourceEntry::builtin("affaan-m", "everything-claude-code", "main", "skills/",
            "Everything Claude Code", "Community skills collection (120+)", true),
        SourceEntry::builtin("TerminalSkills", "skills", "main", "skills/",
            "Terminal Skills", "Open-source skill library (900+)", false),
        SourceEntry::builtin("sickn33", "antigravity-awesome-skills", "main", "skills/",
            "Antigravity Skills", "Agentic skills collection (1300+)", false),
        SourceEntry::builtin("mxyhi", "ok-skills", "main", "",
            "OK Skills", "Curated agent skills & playbooks (55)", false),
    ]
}

const SOURCES_FILE: &str = "market-sources.json";

/// Load source list: merge built-ins with user state.
pub fn load_sources(data_dir: &Path) -> Vec<SourceEntry> {
    let path = data_dir.join(SOURCES_FILE);
    let saved: Vec<SourceEntry> = if path.exists() {
        std::fs::read_to_string(&path).ok()
            .and_then(|c| serde_json::from_str(&c).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let mut result: Vec<SourceEntry> = Vec::new();

    // Merge built-in sources: use saved enabled state if available
    for b in builtin_sources() {
        let enabled = saved.iter()
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
    pub repo_path: String,      // e.g. "skills/brainstorming"
    pub source_label: String,
    pub source_repo: String,    // "owner/repo"
    pub branch: String,
    #[serde(skip)]
    pub installed: bool,
}

const CACHE_DIR: &str = "market-cache";
const CACHE_MAX_AGE_SECS: u64 = 3600; // 1 hour

/// Load cached skill list from disk. Returns None if missing or stale.
pub fn load_cache(data_dir: &Path, source: &SourceEntry) -> Option<Vec<MarketSkill>> {
    let path = data_dir.join(CACHE_DIR).join(format!("{}.json", cache_key(source)));
    let meta = std::fs::metadata(&path).ok()?;
    let age = meta.modified().ok()?
        .elapsed().ok()?
        .as_secs();
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

fn cache_key(source: &SourceEntry) -> String {
    format!("{}_{}", source.owner, source.repo)
}

pub struct Market;

impl Market {
    /// Fetch skill list from GitHub API.
    pub async fn fetch(source: &SourceEntry) -> Result<Vec<MarketSkill>> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
            source.owner, source.repo, source.branch,
        );

        let client = reqwest::Client::builder()
            .user_agent("skill-manager/0.1")
            .build()?;

        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("GitHub API {} for {}/{}", resp.status(), source.owner, source.repo);
        }

        let body: GitTree = resp.json().await?;
        let label = &source.label;
        let repo_id = source.repo_id();
        let mut skills = Vec::new();

        for node in &body.tree {
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

            if name.is_empty() { continue; }

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
        Ok(skills)
    }

    /// Install a single skill: download the entire skill directory from GitHub.
    /// Uses GitHub Contents API to list all files, then downloads each via raw URL.
    pub async fn install_single(skill: &MarketSkill, paths: &crate::core::paths::AppPaths) -> Result<()> {
        let parts: Vec<&str> = skill.source_repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            bail!("invalid source_repo: {}", skill.source_repo);
        }
        let (owner, repo) = (parts[0], parts[1]);

        let client = reqwest::Client::builder()
            .user_agent("skill-manager/0.1")
            .build()?;

        let skill_dir = paths.skills_dir().join(&skill.name);
        std::fs::create_dir_all(&skill_dir)?;

        // Use the repo_path as the directory to list; if empty, use root
        let api_path = if skill.repo_path.is_empty() {
            String::new()
        } else {
            skill.repo_path.clone()
        };

        Self::download_directory_recursive(
            &client, owner, repo, &skill.branch, &api_path, &skill_dir,
        ).await?;

        Ok(())
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
            format!(
                "https://api.github.com/repos/{owner}/{repo}/contents?ref={branch}",
            )
        } else {
            format!(
                "https://api.github.com/repos/{owner}/{repo}/contents/{api_path}?ref={branch}",
            )
        };

        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("GitHub Contents API returned HTTP {} for {}", resp.status(), url);
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
                        bail!("Failed to download {}: HTTP {}", item.path, file_resp.status());
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
                    )).await?;
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
struct GitTree {
    tree: Vec<GitTreeNode>,
}

#[derive(Deserialize)]
struct GitTreeNode {
    path: String,
}

#[derive(Deserialize)]
struct GitHubContentItem {
    name: String,
    path: String,
    #[serde(rename = "type")]
    item_type: String,
}

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

/// Sentinel `owner` value used to flag a `SourceEntry` as the
/// skills.sh aggregator rather than a real GitHub repo. `Market::fetch`
/// branches on this and walks the public sitemap to harvest every
/// `owner/repo/skill` triple skills.sh indexes.
pub const SKILLSHUB_SENTINEL: &str = "*skills-hub*";

impl SourceEntry {
    /// True when this entry is the skills.sh aggregator (sitemap-driven,
    /// no underlying GitHub tree — every `MarketSkill` it yields carries
    /// the real repo in `source_repo` for install).
    pub fn is_skillshub(&self) -> bool {
        self.owner == SKILLSHUB_SENTINEL
    }

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

/// Default built-in sources. The runai Market is now a thin layer over
/// the skills.sh ecosystem — the old per-repo `SourceEntry` list was
/// retired so the Market tab shows one canonical aggregated catalog.
/// User-added GitHub repos (via `+ GitHub`) still work — they're stored
/// as non-builtin entries in `market-sources.json`.
fn builtin_sources() -> Vec<SourceEntry> {
    vec![SourceEntry::builtin(
        SKILLSHUB_SENTINEL,
        SKILLSHUB_SENTINEL,
        "main",
        "",
        "skills.sh",
        "Open Agent Skills ecosystem — 20K+ skills aggregated from 2.6K GitHub repos",
        true,
    )]
}

/// Base URL for raw GitHub mirror. jsdelivr CDN by default (measured
/// ~1s/file in mainland China vs raw.githubusercontent.com's 7s+).
/// Override with `RUNAI_GH_MIRROR` env to point at a different host —
/// must serve `/gh/<owner>/<repo>@<branch>/<path>` like jsdelivr does.
/// Set `RUNAI_GH_MIRROR=raw` to fall back to raw.githubusercontent.com
/// (useful when behind GFW-free networks where jsdelivr's fastly route
/// is actually slower).
fn mirror_base() -> String {
    let v = std::env::var("RUNAI_GH_MIRROR").unwrap_or_default();
    let v = v.trim().trim_end_matches('/');
    if v.is_empty() || v == "default" {
        return "https://cdn.jsdelivr.net".into();
    }
    if v == "raw" || v == "github" {
        // Sentinel for "go direct to raw.githubusercontent.com" — handled
        // by callers via the special prefix check.
        return "https://raw.githubusercontent.com".into();
    }
    v.to_string()
}

/// Build a raw-file URL for `owner/repo@branch/path` honoring the
/// configured mirror. Encapsulates the path-shape difference between
/// jsdelivr ("/gh/o/r@b/p") and raw.github ("/o/r/b/p").
pub(crate) fn raw_url_for(owner: &str, repo: &str, branch: &str, path: &str) -> String {
    let base = mirror_base();
    if base == "https://raw.githubusercontent.com" {
        format!("{base}/{owner}/{repo}/{branch}/{path}")
    } else {
        format!("{base}/gh/{owner}/{repo}@{branch}/{path}")
    }
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
    /// All-time install count harvested from skills.sh leaderboard SSR.
    /// `0` for entries known only through the sitemap (no popularity
    /// signal). Used to power the "All Time" ordering and INSTALLS column.
    #[serde(default)]
    pub installs: u64,
    /// 24-hour install delta (Trending tab). `0` when unknown.
    #[serde(default)]
    pub trending_installs: u64,
    /// Hot score from skills.sh `/hot` ranking. `0` when unknown.
    /// Concretely populated as "installs from /hot SSR" — skills.sh's
    /// internal score isn't exposed but the ordered list is.
    #[serde(default)]
    pub hot_score: u64,
    /// 8-week install trend from skills.sh `weeklyInstalls`. Empty when
    /// unknown. Used by the SPARKLINE column in the leaderboard UI.
    #[serde(default)]
    pub weekly_installs: Vec<u64>,
    /// Marked official by skills.sh.
    #[serde(default)]
    pub is_official: bool,
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
        if let Some(cached) = load_cache(data_dir, src)
            && let Some(skill) = cached.into_iter().find(|s| s.name == skill_name)
        {
            return Some(skill);
        }
    }
    None
}

fn cache_key(source: &SourceEntry) -> String {
    format!("{}_{}", source.owner, source.repo)
}

/// One row from a skills.sh leaderboard SSR HTML page. The same shape
/// shows up on `/`, `/trending`, `/hot`; the meaning of `installs`
/// depends on which page it came from (all-time, 24h delta, hot score).
#[derive(Debug, Clone)]
pub(crate) struct LeaderboardRow {
    pub source_repo: String,
    pub skill_id: String,
    pub installs: u64,
    pub weekly_installs: Vec<u64>,
    pub is_official: bool,
}

/// Parse the streamed Next.js SSR payload embedded in a skills.sh page.
/// The payload is escaped JSON inside `self.__next_f.push(...)` calls;
/// each leaderboard row reads literally as
/// `\"source\":\"<owner/repo>\",\"skillId\":\"<slug>\",\"name\":\"...\",\"installs\":<n>[,\"weeklyInstalls\":[...]][,\"isOfficial\":true]`.
/// Stdlib-only — works on `/`, `/trending`, `/hot` identically.
pub(crate) fn parse_leaderboard(body: &str) -> Vec<LeaderboardRow> {
    let mut out = Vec::new();
    // Find each `\"source\":\"` anchor (literal backslash + quote in source).
    let needle = "\\\"source\\\":\\\"";
    let mut search_from = 0;
    while let Some(rel) = body[search_from..].find(needle) {
        let start = search_from + rel + needle.len();
        search_from = start;
        // Capture source until the closing `\"`.
        let Some(end_source) = body[start..].find("\\\"") else { break };
        let source_repo = body[start..start + end_source].to_string();
        if source_repo.is_empty() || !source_repo.contains('/') {
            continue;
        }
        let after_source = start + end_source + needle.len() - "source\\\":\\\"".len();
        // Look ahead within a bounded window for skillId + installs (+ optional fields).
        let window_end = (after_source + 800).min(body.len());
        let window = &body[after_source..window_end];

        let Some(skill_id) = extract_quoted_field(window, "skillId") else { continue };
        let Some(installs_str) = extract_field(window, "installs") else { continue };
        let Ok(installs) = installs_str.parse::<u64>() else { continue };
        let weekly_installs = extract_array_field(window, "weeklyInstalls")
            .unwrap_or_default();
        let is_official = window.contains("\\\"isOfficial\\\":true");

        out.push(LeaderboardRow {
            source_repo,
            skill_id,
            installs,
            weekly_installs,
            is_official,
        });
    }
    out
}

fn extract_quoted_field(window: &str, name: &str) -> Option<String> {
    let needle = format!("\\\"{name}\\\":\\\"");
    let start = window.find(&needle)? + needle.len();
    let end = window[start..].find("\\\"")?;
    Some(window[start..start + end].to_string())
}

fn extract_field(window: &str, name: &str) -> Option<String> {
    let needle = format!("\\\"{name}\\\":");
    let start = window.find(&needle)? + needle.len();
    let mut end = start;
    let bytes = window.as_bytes();
    while end < bytes.len() {
        let c = bytes[end];
        if !c.is_ascii_digit() && c != b'-' {
            break;
        }
        end += 1;
    }
    if end == start {
        return None;
    }
    Some(window[start..end].to_string())
}

fn extract_array_field(window: &str, name: &str) -> Option<Vec<u64>> {
    let needle = format!("\\\"{name}\\\":[");
    let start = window.find(&needle)? + needle.len();
    let end = window[start..].find(']')?;
    let inner = &window[start..start + end];
    let nums: Vec<u64> = inner
        .split(',')
        .filter_map(|n| n.trim().parse::<u64>().ok())
        .collect();
    if nums.is_empty() {
        return None;
    }
    Some(nums)
}

/// Whitelist filter for "root-skill" repos where the entire repository
/// IS the skill (e.g. `anysearch-ai/anysearch-skill`). Drops VCS / CI
/// metadata and top-level license / readme noise that shouldn't end up
/// under `<install_root>/<skill>/`. Anything else (SKILL.md, scripts/,
/// references/, .env.example, runtime.conf.example, …) passes.
pub(crate) fn is_root_skill_payload(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    // VCS / CI infra — never part of the skill payload.
    if lower.starts_with(".git/")
        || lower == ".gitignore"
        || lower == ".gitattributes"
        || lower.starts_with(".github/")
        || lower.starts_with(".husky/")
        || lower.starts_with(".devcontainer/")
        || lower.starts_with(".vscode/")
        || lower.starts_with(".idea/")
    {
        return false;
    }
    // Top-level repo docs that the skill itself doesn't need (SKILL.md
    // is its own canonical doc). Subdirectory README.md (e.g.
    // scripts/README.md) IS kept because it may be skill-internal docs.
    let is_top_level = !path.contains('/');
    if is_top_level
        && matches!(
            lower.as_str(),
            "readme.md"
                | "license"
                | "license.md"
                | "license.txt"
                | "license-mit"
                | "license-apache"
                | "code_of_conduct.md"
                | "contributing.md"
                | "changelog.md"
                | "security.md"
        )
    {
        return false;
    }
    true
}

/// Extract every `<loc>https://…</loc>` URL from a sitemap XML document.
/// Stdlib-only — sitemap shape is fixed enough that a regex / xml crate
/// would be overkill. Tolerates whitespace and weird wrapping; ignores
/// `<loc>` payloads that don't start with `http`.
fn extract_sitemap_locs(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<loc>") {
        let after = &rest[start + 5..];
        let Some(end) = after.find("</loc>") else { break };
        let raw = after[..end].trim();
        if raw.starts_with("http") {
            out.push(raw.to_string());
        }
        rest = &after[end + 6..];
    }
    out
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
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: false,
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
                installs: 0,
                trending_installs: 0,
                hot_score: 0,
                weekly_installs: Vec::new(),
                is_official: false,
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

    /// Fetch skill list from GitHub API, or from the skills.sh sitemap
    /// when the source carries the [`SKILLSHUB_SENTINEL`] owner.
    pub(crate) async fn fetch(source: &SourceEntry) -> Result<ExtractResult> {
        if source.is_skillshub() {
            let skills = Self::fetch_skillshub().await?;
            // The aggregator has no underlying git tree; subsequent
            // install paths re-fetch the real repo's tree on demand.
            return Ok(ExtractResult {
                skills,
                plugin_detected: false,
                tree: GitTree { tree: Vec::new() },
            });
        }
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

    /// Build the unified skills.sh catalog.
    ///
    /// Pipeline:
    /// 1. Hit `/`, `/trending`, `/hot` — each SSRs ~600 leaderboard rows
    ///    with `source / skillId / installs / weeklyInstalls / isOfficial`.
    ///    Parse those into the popularity signals (`installs`,
    ///    `trending_installs`, `hot_score`, `weekly_installs`).
    /// 2. Hit `sitemap-skills-{1,2}.xml` — flatten 20K+ `owner/repo/skill`
    ///    URLs for skills the leaderboard didn't cover.
    /// 3. Merge by `(source_repo, name)` key. Leaderboard rows win
    ///    (they're enriched); sitemap entries fill the long tail with
    ///    zero popularity.
    pub(crate) async fn fetch_skillshub() -> Result<Vec<MarketSkill>> {
        let client = reqwest::Client::builder()
            .user_agent("runai/0.11")
            .build()?;

        let (all_html, trending_html, hot_html) = tokio::try_join!(
            async { client.get("https://www.skills.sh/").send().await?.error_for_status()?.text().await },
            async { client.get("https://www.skills.sh/trending").send().await?.error_for_status()?.text().await },
            async { client.get("https://www.skills.sh/hot").send().await?.error_for_status()?.text().await },
        )?;

        let mut by_key: std::collections::HashMap<String, MarketSkill> =
            std::collections::HashMap::new();
        let make_key =
            |source_repo: &str, name: &str| format!("{source_repo}//{name}");

        // /  → installs (All Time) + weeklyInstalls + isOfficial.
        for r in parse_leaderboard(&all_html) {
            let k = make_key(&r.source_repo, &r.skill_id);
            by_key
                .entry(k.clone())
                .or_insert_with(|| MarketSkill {
                    name: r.skill_id.clone(),
                    repo_path: String::new(),
                    source_label: "skills.sh".to_string(),
                    source_repo: r.source_repo.clone(),
                    branch: "main".to_string(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: false,
                    installed: false,
                });
            if let Some(s) = by_key.get_mut(&k) {
                s.installs = r.installs;
                s.weekly_installs = r.weekly_installs;
                s.is_official = r.is_official;
            }
        }
        // /trending → trending_installs (24h delta).
        for r in parse_leaderboard(&trending_html) {
            let k = make_key(&r.source_repo, &r.skill_id);
            by_key
                .entry(k.clone())
                .or_insert_with(|| MarketSkill {
                    name: r.skill_id.clone(),
                    repo_path: String::new(),
                    source_label: "skills.sh".to_string(),
                    source_repo: r.source_repo.clone(),
                    branch: "main".to_string(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: r.is_official,
                    installed: false,
                });
            if let Some(s) = by_key.get_mut(&k) {
                s.trending_installs = r.installs;
            }
        }
        // /hot → hot_score.
        for r in parse_leaderboard(&hot_html) {
            let k = make_key(&r.source_repo, &r.skill_id);
            by_key
                .entry(k.clone())
                .or_insert_with(|| MarketSkill {
                    name: r.skill_id.clone(),
                    repo_path: String::new(),
                    source_label: "skills.sh".to_string(),
                    source_repo: r.source_repo.clone(),
                    branch: "main".to_string(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: r.is_official,
                    installed: false,
                });
            if let Some(s) = by_key.get_mut(&k) {
                s.hot_score = r.installs;
            }
        }

        // Sitemap fills the long tail. Skips entries the leaderboard
        // already covered to avoid double-counting (HashMap key check).
        const SHARDS: &[&str] = &[
            "https://www.skills.sh/sitemap-skills-1.xml",
            "https://www.skills.sh/sitemap-skills-2.xml",
        ];
        for shard in SHARDS {
            let body = client.get(*shard).send().await?.error_for_status()?.text().await?;
            for url in extract_sitemap_locs(&body) {
                let rest = match url
                    .strip_prefix("https://www.skills.sh/")
                    .or_else(|| url.strip_prefix("https://skills.sh/"))
                {
                    Some(r) => r,
                    None => continue,
                };
                let parts: Vec<&str> = rest.splitn(3, '/').collect();
                if parts.len() != 3 {
                    continue;
                }
                let (owner, repo, skill_name) = (parts[0], parts[1], parts[2]);
                if owner.is_empty() || repo.is_empty() || skill_name.is_empty() {
                    continue;
                }
                let source_repo = format!("{owner}/{repo}");
                let k = make_key(&source_repo, skill_name);
                by_key.entry(k).or_insert_with(|| MarketSkill {
                    name: skill_name.to_string(),
                    repo_path: String::new(),
                    source_label: "skills.sh".to_string(),
                    source_repo,
                    branch: "main".to_string(),
                    installs: 0,
                    trending_installs: 0,
                    hot_score: 0,
                    weekly_installs: Vec::new(),
                    is_official: false,
                    installed: false,
                });
            }
        }

        let mut skills: Vec<MarketSkill> = by_key.into_values().collect();
        // Default order: by all-time installs desc, then name asc. The
        // server-side `?sort=` query re-sorts the cached list per request.
        skills.sort_by(|a, b| {
            b.installs
                .cmp(&a.installs)
                .then(a.source_repo.cmp(&b.source_repo))
                .then(a.name.cmp(&b.name))
        });
        Ok(skills)
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
    /// Build download tasks that drop every skill's files under `install_root`.
    /// For public installs `install_root = paths.skills_dir()`; for private
    /// installs it's `paths.user_skills_dir(uid)?`. The function itself does
    /// not consult `AppPaths` — the caller decides where the bytes land.
    pub(crate) fn collect_download_tasks(
        extract: &ExtractResult,
        install_root: &std::path::Path,
    ) -> Vec<DownloadTask> {
        let mut tasks = Vec::new();
        for skill in &extract.skills {
            let parts: Vec<&str> = skill.source_repo.splitn(2, '/').collect();
            if parts.len() != 2 {
                continue;
            }
            let (owner, repo) = (parts[0], parts[1]);
            let skill_dir = install_root.join(&skill.name);

            // Root-skill install (`repo_path == "."`): the repo *is*
            // the skill. Take everything except VCS / CI / license /
            // top-level README junk. Tree paths land verbatim under
            // `skill_dir/`.
            if skill.repo_path == "." {
                for node in &extract.tree.tree {
                    if !is_root_skill_payload(&node.path) {
                        continue;
                    }
                    let url = raw_url_for(owner, repo, &skill.branch, &node.path);
                    let dest_path = skill_dir.join(&node.path);
                    tasks.push(DownloadTask {
                        skill_name: skill.name.clone(),
                        url,
                        dest_path,
                    });
                }
                continue;
            }

            let repo_path = if skill.repo_path.is_empty() {
                &skill.name
            } else {
                &skill.repo_path
            };
            let files = Self::get_skill_files(&extract.tree, repo_path);
            let prefix = format!("{repo_path}/");

            for file_path in files {
                // Route raw downloads through the configured GitHub mirror
                // (jsdelivr CDN by default — measured ~1s vs raw.github's
                // 7s+ from mainland China networks).
                let url = raw_url_for(owner, repo, &skill.branch, &file_path);
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
    /// `install_root` is where `<skill.name>/` is created. Owner-aware
    /// callers pass `&paths.user_skills_dir(uid)?` for private installs and
    /// `&paths.skills_dir()` for the public pool.
    pub(crate) async fn install_single_with_tree(
        skill: &MarketSkill,
        install_root: &std::path::Path,
        tree: Option<&GitTree>,
    ) -> Result<()> {
        let parts: Vec<&str> = skill.source_repo.splitn(2, '/').collect();
        if parts.len() != 2 {
            bail!("invalid source_repo: {}", skill.source_repo);
        }
        let (owner, repo) = (parts[0], parts[1]);
        let client = reqwest::Client::builder().user_agent("runai/0.5").build()?;
        let skill_dir = install_root.join(&skill.name);
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
                let raw_url = raw_url_for(owner, repo, &skill.branch, &file_path);
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
    /// `install_root` is the parent directory under which `<skill.name>/` is created.
    pub async fn install_single(
        skill: &MarketSkill,
        install_root: &std::path::Path,
    ) -> Result<()> {
        Self::install_single_with_tree(skill, install_root, None).await
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
                    let raw_url = raw_url_for(owner, repo, branch, &item.path);
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
            installs: 0,
            trending_installs: 0,
            hot_score: 0,
            weekly_installs: Vec::new(),
            is_official: false,
            installed: false,
        }];
        save_cache(data_dir, &source, &skills).unwrap();

        // Find by repo_id
        let found = find_skill_in_sources(
            data_dir,
            std::slice::from_ref(&source),
            "find-skills",
            Some("mxyhi/ok-skills"),
        );
        assert!(found.is_some(), "should find by repo_id");

        // Find by label
        let found = find_skill_in_sources(
            data_dir,
            std::slice::from_ref(&source),
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

        let tasks = Market::collect_download_tasks(&extract, &paths.skills_dir());

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

        // Verify URL format — v15 routes all raw downloads through the
        // configured mirror (jsdelivr by default, switchable via
        // RUNAI_GH_MIRROR). The test asserts the new shape.
        let expected_prefix = if std::env::var("RUNAI_GH_MIRROR").unwrap_or_default() == "raw" {
            "https://raw.githubusercontent.com/test/repo/main/".to_string()
        } else {
            "https://cdn.jsdelivr.net/gh/test/repo@main/".to_string()
        };
        assert!(
            tasks[0].url.starts_with(&expected_prefix),
            "url = {}, expected prefix = {}",
            tasks[0].url,
            expected_prefix
        );
    }

    #[test]
    fn builtin_sources_only_contain_skillshub_aggregator() {
        // Per the user's 2025 spec the Market is now a thin skills.sh
        // layer — the old per-repo `SourceEntry` list was retired. Any
        // GitHub repo a user wants tracked goes through `+ GitHub`
        // (non-builtin user source) instead.
        let sources = builtin_sources();
        assert_eq!(sources.len(), 1, "exactly one builtin source");
        let only = &sources[0];
        assert!(only.is_skillshub());
        assert_eq!(only.label, "skills.sh");
        assert!(only.enabled, "skills.sh aggregator default-on");
    }

    // ───────────────────────── skills.sh aggregator ─────────────────────────

    #[test]
    fn extract_sitemap_locs_parses_multiple_urls() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<urlset>
  <url><loc>https://www.skills.sh/anthropics/skills/foo</loc></url>
  <url><loc>https://www.skills.sh/vercel-labs/agent-skills/bar</loc></url>
  <url><loc>not-a-url</loc></url>
  <url><loc>  https://www.skills.sh/microsoft/azure-skills/baz  </loc></url>
</urlset>"#;
        let urls = extract_sitemap_locs(xml);
        assert_eq!(urls.len(), 3);
        assert_eq!(urls[0], "https://www.skills.sh/anthropics/skills/foo");
        assert_eq!(urls[1], "https://www.skills.sh/vercel-labs/agent-skills/bar");
        assert_eq!(urls[2], "https://www.skills.sh/microsoft/azure-skills/baz");
    }

    #[test]
    fn extract_sitemap_locs_handles_empty_and_malformed() {
        assert!(extract_sitemap_locs("").is_empty());
        assert!(extract_sitemap_locs("<loc>no-closing").is_empty());
        assert!(extract_sitemap_locs("<urlset></urlset>").is_empty());
    }

    #[test]
    fn skillshub_sentinel_present_in_builtin_sources() {
        let entry = builtin_sources()
            .into_iter()
            .find(|s| s.is_skillshub())
            .expect("skills.sh aggregator must be in builtin_sources");
        assert_eq!(entry.label, "skills.sh");
        assert!(entry.enabled, "must default to enabled for first-run Market tab");
        assert_eq!(entry.owner, SKILLSHUB_SENTINEL);
    }

    #[test]
    fn parse_leaderboard_extracts_installs_and_weekly() {
        // Skills.sh SSR embeds rows as escaped JSON inside `__next_f.push`.
        // The pattern is stable across `/`, `/trending`, `/hot`.
        let snippet = r#"
            ...random stuff...
            \"source\":\"vercel-labs/skills\",\"skillId\":\"find-skills\",\"name\":\"find-skills\",\"installs\":1765053,\"weeklyInstalls\":[100113,116613,115950,102724,94569,101582,116305,37369],\"isOfficial\":true},
            \"source\":\"anthropics/skills\",\"skillId\":\"frontend-design\",\"name\":\"frontend-design\",\"installs\":478598,\"weeklyInstalls\":[33429,31868,29995,26231,19072,26733,30547,9776]},
            \"source\":\"random/repo\",\"skillId\":\"no-weekly\",\"name\":\"no-weekly\",\"installs\":42},
        "#;
        let rows = parse_leaderboard(snippet);
        assert_eq!(rows.len(), 3, "must extract all three rows");

        assert_eq!(rows[0].skill_id, "find-skills");
        assert_eq!(rows[0].source_repo, "vercel-labs/skills");
        assert_eq!(rows[0].installs, 1_765_053);
        assert_eq!(rows[0].weekly_installs.len(), 8);
        assert_eq!(rows[0].weekly_installs[0], 100_113);
        assert!(rows[0].is_official);

        assert_eq!(rows[1].skill_id, "frontend-design");
        assert_eq!(rows[1].installs, 478_598);
        assert!(!rows[1].is_official);

        // No weeklyInstalls / isOfficial in the third row — defaults must apply.
        assert_eq!(rows[2].skill_id, "no-weekly");
        assert_eq!(rows[2].installs, 42);
        assert!(rows[2].weekly_installs.is_empty());
        assert!(!rows[2].is_official);
    }

    #[test]
    fn root_skill_payload_filter_keeps_skill_md_skips_metadata() {
        // Real-world repo: anysearch-ai/anysearch-skill ships SKILL.md
        // at root + scripts/ + runtime.conf.example. Must keep skill
        // payload, drop housekeeping.
        let keep = [
            "SKILL.md",
            ".env.example",
            "runtime.conf.example",
            "scripts/anysearch_cli.sh",
            "scripts/shared/constants.json",
            "scripts/README.md", // sub-dir README is skill-internal, keep
        ];
        for p in keep {
            assert!(is_root_skill_payload(p), "expected {p} kept");
        }
        let drop = [
            ".gitignore",
            ".gitattributes",
            ".github/workflows/ci.yml",
            ".husky/pre-commit",
            ".vscode/settings.json",
            "README.md",
            "LICENSE",
            "LICENSE.md",
            "CHANGELOG.md",
            "CONTRIBUTING.md",
            "",
        ];
        for p in drop {
            assert!(!is_root_skill_payload(p), "expected {p} dropped");
        }
    }

    #[test]
    fn parse_leaderboard_handles_empty_and_garbage() {
        assert!(parse_leaderboard("").is_empty());
        assert!(parse_leaderboard("no JSON at all").is_empty());
        // Source without slash is rejected.
        let bad = r#"\"source\":\"justname\",\"skillId\":\"x\",\"installs\":1"#;
        assert!(parse_leaderboard(bad).is_empty());
    }

    #[test]
    fn skillshub_load_sources_preserves_user_enabled_toggle() {
        let tmp = tempfile::tempdir().unwrap();
        let mut sources = load_sources(tmp.path());
        let entry = sources
            .iter_mut()
            .find(|s| s.is_skillshub())
            .expect("aggregator entry present");
        entry.enabled = true;
        save_sources(tmp.path(), &sources).unwrap();

        let reloaded = load_sources(tmp.path());
        let entry = reloaded.iter().find(|s| s.is_skillshub()).unwrap();
        assert!(entry.enabled, "user toggle must persist across reload");
    }
}

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
pub(super) fn builtin_sources() -> Vec<SourceEntry> {
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

#[cfg(test)]
mod tests {
    use super::*;

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

use anyhow::Result;
use std::path::Path;

use super::sources::SourceEntry;
use super::types::MarketSkill;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

use super::SkillManager;
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::linker::Linker;
use crate::core::paths::AppPaths;
use crate::core::resource::ResourceKind;
use crate::core::scanner::Scanner;
use anyhow::Result;
use std::path::Path;

impl SkillManager {
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    // --- Scan ---

    pub fn scan(&self) -> Result<crate::core::scanner::ScanResult> {
        Scanner::scan_all(&self.paths, &self.db)
    }

    pub fn status(&self, target: CliTarget) -> Result<(usize, usize)> {
        let mut skill_enabled = 0;
        if let Ok(skills) = self.db.list_resources(Some(ResourceKind::Skill), None) {
            // Dedupe by name first — same skill may have multiple DB rows from
            // historical adopts. Without this, a duplicated skill counts twice
            // toward `enabled` while the list-view (which dedupes via
            // list_resources) shows it once, and the header/list disagree.
            let mut seen = std::collections::HashSet::new();
            for skill in &skills {
                if !seen.insert(skill.name.clone()) {
                    continue;
                }
                // Check both skills/ and .agents/skills/. Use `symlink_metadata`
                // (via `Linker::is_symlink`) so DANGLING symlinks still count
                // as enabled — `path.exists()` follows symlinks and returns
                // false for those, which is what made the header undercount.
                // The on-disk symlink IS the source of truth for "enabled";
                // whether it's currently dangling is a separate concern that
                // `runai doctor` reports.
                let primary = target.skills_dir().join(&skill.name);
                let agents = target.agents_skills_dir().join(&skill.name);
                if Linker::is_symlink(&primary)
                    || Linker::is_symlink(&agents)
                    || primary.exists()
                    || agents.exists()
                {
                    skill_enabled += 1;
                }
            }
        }
        let mcp_status = Self::read_mcp_status_from_configs();
        let mcp_enabled = mcp_status
            .values()
            .filter(|targets| targets.get(&target).copied().unwrap_or(false))
            .count();
        Ok((skill_enabled, mcp_enabled))
    }

    // --- Internal ---

    pub(super) fn extract_description(skill_dir: &Path) -> String {
        Scanner::extract_description(skill_dir)
    }

    pub fn is_first_launch(&self) -> bool {
        let (skills, mcps) = self.resource_count();
        skills + mcps == 0
    }

    /// Count total skills (from DB) + total MCPs (active + disabled by SM).
    pub fn resource_count(&self) -> (usize, usize) {
        let skills = self.db.skill_count().unwrap_or(0);
        // Active MCPs from config files
        let active_mcps = Self::read_mcp_status_from_configs();
        // Disabled MCPs backed up by SM
        let mut total_mcp_names: std::collections::HashSet<String> =
            active_mcps.keys().cloned().collect();
        let mcps_dir = self.paths.mcps_dir();
        if mcps_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&mcps_dir)
        {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("json")
                    && let Some(name) = entry.path().file_stem().and_then(|s| s.to_str())
                {
                    total_mcp_names.insert(name.to_string());
                }
            }
        }
        (skills, total_mcp_names.len())
    }
}

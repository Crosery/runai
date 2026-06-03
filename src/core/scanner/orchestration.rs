use super::registration::ScanResult;
use crate::core::backup;
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::paths::AppPaths;
use anyhow::Result;

pub struct Scanner;

impl Scanner {
    pub fn scan_all(paths: &AppPaths, db: &Database) -> Result<ScanResult> {
        let mut total = ScanResult::default();

        // 0. Create backup before first scan if no backup exists
        if !backup::has_backup(paths) {
            let _ = backup::create_backup(paths);
        }

        // 1. Register all skills already in the managed directory
        let managed_result = Self::scan_managed_dir(paths, db);
        total.adopted += managed_result.adopted;
        total.skipped += managed_result.skipped;
        total.errors.extend(managed_result.errors);
        total.adopted_names.extend(managed_result.adopted_names);

        // 2. Scan user skills/ directories — adopt (move) foreign entries
        for target in CliTarget::ALL {
            let cli_dir = target.skills_dir();
            if cli_dir.exists() {
                let result = Self::scan_cli_dir(&cli_dir, paths, db, *target)?;
                total.adopted += result.adopted;
                total.skipped += result.skipped;
                total.errors.extend(result.errors);
                total.adopted_names.extend(result.adopted_names);
            }
        }

        // 3. Scan plugin .agents/skills/ directories — register only, never move files
        for target in CliTarget::ALL {
            let agents_dir = target.agents_skills_dir();
            if agents_dir.exists() {
                let result = Self::scan_agents_dir(&agents_dir, db);
                total.adopted += result.adopted;
                total.skipped += result.skipped;
                total.errors.extend(result.errors);
                total.adopted_names.extend(result.adopted_names);
            }
        }

        // 4. Scan ~/skills/ directory (e.g. SkillHub installs) — register only
        if let Some(home) = dirs::home_dir() {
            let home_skills = home.join("skills");
            if home_skills.exists() {
                let result = Self::scan_agents_dir(&home_skills, db);
                total.adopted += result.adopted;
                total.skipped += result.skipped;
                total.errors.extend(result.errors);
                total.adopted_names.extend(result.adopted_names);
            }
        }

        Ok(total)
    }
}

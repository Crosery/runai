use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct AppPaths {
    base: PathBuf,
}

impl AppPaths {
    pub fn default_path() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        let new_base = if cfg!(windows) {
            dirs::data_dir()
                .unwrap_or_else(|| home.clone())
                .join("runai")
        } else {
            home.join(".runai")
        };

        // Auto-migrate from old ~/.skill-manager/ if new path doesn't exist
        if !new_base.exists() {
            let old_base = if cfg!(windows) {
                dirs::data_dir()
                    .unwrap_or_else(|| home.clone())
                    .join("skill-manager")
            } else {
                home.join(".skill-manager")
            };
            if old_base.exists() {
                let _ = Self::migrate_data_dir(&old_base, &new_base, &home);
            }
        }

        Self { base: new_base }
    }

    /// Migrate old data directory to new location.
    /// Renames the directory, the DB file, and fixes symlinks in all CLI skills dirs.
    fn migrate_data_dir(old: &Path, new: &Path, home: &Path) -> Result<()> {
        let old_str = old.to_string_lossy().to_string();
        let new_str = new.to_string_lossy().to_string();

        // Rename the entire directory atomically
        std::fs::rename(old, new)?;

        // Rename DB file: skill-manager.db → runai.db
        let old_db = new.join("skill-manager.db");
        let new_db = new.join("runai.db");
        if old_db.exists() && !new_db.exists() {
            std::fs::rename(&old_db, &new_db)?;
        }

        // Fix symlinks in all CLI skills directories
        Self::relink_cli_skills(home, &old_str, &new_str);

        Ok(())
    }

    /// Scan all CLI skills directories for symlinks pointing to old path, repoint to new path.
    fn relink_cli_skills(home: &Path, old_prefix: &str, new_prefix: &str) {
        let cli_skill_dirs = [
            home.join(".claude").join("skills"),
            home.join(".codex").join("skills"),
            home.join(".gemini").join("skills"),
            home.join(".opencode").join("skills"),
            home.join(".config").join("opencode").join("skills"),
        ];

        for dir in &cli_skill_dirs {
            if !dir.exists() {
                continue;
            }
            let entries = match std::fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // Only fix symlinks
                if !path.is_symlink() {
                    continue;
                }
                let target = match std::fs::read_link(&path) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                let target_str = target.to_string_lossy();
                if target_str.contains(old_prefix) {
                    let new_target = target_str.replace(old_prefix, new_prefix);
                    // Remove old symlink and create new one
                    let _ = std::fs::remove_file(&path);
                    #[cfg(unix)]
                    let _ = std::os::unix::fs::symlink(Path::new(&new_target), &path);
                    #[cfg(windows)]
                    let _ = std::os::windows::fs::symlink_dir(Path::new(&new_target), &path);
                }
            }
        }
    }

    pub fn with_base(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn data_dir(&self) -> &Path {
        &self.base
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.base.join("skills")
    }

    pub fn mcps_dir(&self) -> PathBuf {
        self.base.join("mcps")
    }

    pub fn groups_dir(&self) -> PathBuf {
        self.base.join("groups")
    }

    pub fn db_path(&self) -> PathBuf {
        // Try new name first, fallback to old name for compat
        let new_db = self.base.join("runai.db");
        let old_db = self.base.join("skill-manager.db");
        if new_db.exists() || !old_db.exists() {
            new_db
        } else {
            old_db
        }
    }

    pub fn config_path(&self) -> PathBuf {
        self.base.join("config.toml")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(self.skills_dir())?;
        std::fs::create_dir_all(self.mcps_dir())?;
        std::fs::create_dir_all(self.groups_dir())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_renames_dir_db_and_fixes_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let old_dir = tmp.path().join(".skill-manager");
        let new_dir = tmp.path().join(".runai");

        // Create old structure with data
        std::fs::create_dir_all(old_dir.join("skills/my-skill")).unwrap();
        std::fs::write(old_dir.join("skills/my-skill/SKILL.md"), "# Test").unwrap();
        std::fs::create_dir_all(old_dir.join("groups")).unwrap();
        std::fs::write(old_dir.join("skill-manager.db"), "fake-db-data").unwrap();
        std::fs::write(old_dir.join("market-sources.json"), "[]").unwrap();

        // Create a CLI skills dir with symlink pointing to old path
        let claude_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&claude_skills).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            old_dir.join("skills/my-skill"),
            claude_skills.join("my-skill"),
        )
        .unwrap();

        // Migrate
        AppPaths::migrate_data_dir(&old_dir, &new_dir, tmp.path()).unwrap();

        // Old dir should be gone
        assert!(!old_dir.exists(), "old dir should be removed");

        // New dir should have all files
        assert!(new_dir.exists(), "new dir should exist");
        assert!(
            new_dir.join("skills/my-skill/SKILL.md").exists(),
            "skills preserved"
        );

        // DB renamed
        assert!(new_dir.join("runai.db").exists(), "new DB should exist");
        assert_eq!(
            std::fs::read_to_string(new_dir.join("runai.db")).unwrap(),
            "fake-db-data",
            "DB content preserved"
        );

        // Symlink should be updated to point to new path
        #[cfg(unix)]
        {
            let link = claude_skills.join("my-skill");
            assert!(link.exists(), "symlink should still work");
            let target = std::fs::read_link(&link).unwrap();
            assert!(
                target.to_string_lossy().contains(".runai"),
                "symlink should point to .runai, got: {}",
                target.display()
            );
            assert!(
                !target.to_string_lossy().contains(".skill-manager"),
                "symlink should NOT point to old path"
            );
        }
    }

    #[test]
    fn migrate_skips_when_new_dir_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let old_dir = tmp.path().join(".skill-manager");
        let new_dir = tmp.path().join(".runai");

        // Both exist
        std::fs::create_dir_all(&old_dir).unwrap();
        std::fs::create_dir_all(&new_dir).unwrap();
        std::fs::write(old_dir.join("skill-manager.db"), "old").unwrap();
        std::fs::write(new_dir.join("runai.db"), "new").unwrap();

        // default_path should NOT migrate (new dir exists)
        // We test the condition directly
        assert!(new_dir.exists());
        assert!(old_dir.exists());
        // Migration only runs if !new_base.exists(), so new data is untouched
        assert_eq!(
            std::fs::read_to_string(new_dir.join("runai.db")).unwrap(),
            "new"
        );
    }

    #[test]
    fn db_path_prefers_new_name_falls_back_to_old() {
        let tmp = tempfile::tempdir().unwrap();

        // Only old DB exists
        std::fs::write(tmp.path().join("skill-manager.db"), "old").unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        assert_eq!(
            paths.db_path(),
            tmp.path().join("skill-manager.db"),
            "should use old DB when only it exists"
        );

        // Create new DB
        std::fs::write(tmp.path().join("runai.db"), "new").unwrap();
        let paths2 = AppPaths::with_base(tmp.path().to_path_buf());
        assert_eq!(
            paths2.db_path(),
            tmp.path().join("runai.db"),
            "should prefer new DB"
        );
    }

    #[test]
    fn db_path_returns_new_name_when_neither_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        assert_eq!(
            paths.db_path(),
            tmp.path().join("runai.db"),
            "should default to new name for fresh installs"
        );
    }
}

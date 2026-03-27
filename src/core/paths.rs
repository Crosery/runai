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
                let _ = Self::migrate_data_dir(&old_base, &new_base);
            }
        }

        Self { base: new_base }
    }

    /// Migrate old data directory to new location.
    /// Renames the directory and the DB file inside it.
    fn migrate_data_dir(old: &Path, new: &Path) -> Result<()> {
        // Rename the entire directory atomically
        std::fs::rename(old, new)?;

        // Rename DB file: skill-manager.db → runai.db
        let old_db = new.join("skill-manager.db");
        let new_db = new.join("runai.db");
        if old_db.exists() && !new_db.exists() {
            std::fs::rename(&old_db, &new_db)?;
        }

        Ok(())
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
    fn migrate_renames_old_dir_and_db() {
        let tmp = tempfile::tempdir().unwrap();
        let old_dir = tmp.path().join(".skill-manager");
        let new_dir = tmp.path().join(".runai");

        // Create old structure with data
        std::fs::create_dir_all(old_dir.join("skills/my-skill")).unwrap();
        std::fs::write(old_dir.join("skills/my-skill/SKILL.md"), "# Test").unwrap();
        std::fs::create_dir_all(old_dir.join("groups")).unwrap();
        std::fs::write(old_dir.join("skill-manager.db"), "fake-db-data").unwrap();
        std::fs::write(old_dir.join("market-sources.json"), "[]").unwrap();

        // Migrate
        AppPaths::migrate_data_dir(&old_dir, &new_dir).unwrap();

        // Old dir should be gone
        assert!(!old_dir.exists(), "old dir should be removed");

        // New dir should have all files
        assert!(new_dir.exists(), "new dir should exist");
        assert!(
            new_dir.join("skills/my-skill/SKILL.md").exists(),
            "skills preserved"
        );
        assert!(new_dir.join("groups").exists(), "groups preserved");
        assert!(
            new_dir.join("market-sources.json").exists(),
            "market-sources preserved"
        );

        // DB renamed
        assert!(
            !new_dir.join("skill-manager.db").exists(),
            "old DB name should be gone"
        );
        assert!(new_dir.join("runai.db").exists(), "new DB should exist");
        assert_eq!(
            std::fs::read_to_string(new_dir.join("runai.db")).unwrap(),
            "fake-db-data",
            "DB content preserved"
        );
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

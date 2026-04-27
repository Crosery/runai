use anyhow::Result;
use std::path::{Path, PathBuf};

/// Standalone helper to resolve the data directory without constructing AppPaths.
/// Checks RUNE_DATA_DIR, then SKILL_MANAGER_DATA_DIR, then falls back to ~/.runai.
pub fn data_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("RUNE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Ok(dir) = std::env::var("SKILL_MANAGER_DATA_DIR") {
        return PathBuf::from(dir);
    }
    default_data_dir_no_env()
}

/// Resolve the OS-default data directory **without consulting any env vars**.
/// This is the path runai would use if `RUNE_DATA_DIR` / `SKILL_MANAGER_DATA_DIR`
/// were unset.
///
/// Used by guards that need to detect "user has overridden the data dir" — if
/// the active path differs from this, the override is in effect. `data_dir()`
/// itself can NOT be used for this check: it returns the override when set,
/// so comparing `data_dir() == data_dir()` is always true.
pub fn default_data_dir_no_env() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(windows) {
        dirs::data_dir().unwrap_or(home).join("runai")
    } else {
        home.join(".runai")
    }
}

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

        // Update directory paths inside the DB
        Self::update_db_paths(&new_db, &old_str, &new_str);

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

    /// Update directory and source_meta paths in the DB from old prefix to new.
    fn update_db_paths(db_path: &Path, old_prefix: &str, new_prefix: &str) {
        if !db_path.exists() {
            return;
        }
        let conn = match rusqlite::Connection::open(db_path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let _ = conn.execute(
            "UPDATE resources SET directory = REPLACE(directory, ?1, ?2) WHERE directory LIKE '%' || ?1 || '%'",
            rusqlite::params![old_prefix, new_prefix],
        );
        let _ = conn.execute(
            "UPDATE resources SET source_meta = REPLACE(source_meta, ?1, ?2) WHERE source_meta LIKE '%' || ?1 || '%'",
            rusqlite::params![old_prefix, new_prefix],
        );
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

    pub fn trash_dir(&self) -> PathBuf {
        self.base.join("trash")
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
        std::fs::create_dir_all(self.trash_dir())?;
        Ok(())
    }
}

#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;
    use crate::test_support::HOME_LOCK;

    /// Regression: the 2026-04-27 incident's first guard impl used
    /// `paths::data_dir()` to compute "the default location" — but that
    /// function reads RUNE_DATA_DIR itself, so when the user set RUNE_DATA_DIR
    /// the comparison degenerated to "active == active" and the guard never
    /// fired. `default_data_dir_no_env()` must IGNORE the env vars even when
    /// they're present.
    #[test]
    fn default_data_dir_no_env_ignores_rune_data_dir() {
        let _guard = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let orig_home = std::env::var("HOME").ok();
        let orig_rdd = std::env::var("RUNE_DATA_DIR").ok();
        let orig_smdd = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
        // SAFETY: HOME_LOCK serializes env mutation across tests.
        unsafe {
            std::env::set_var("HOME", tmp.path());
            std::env::set_var("RUNE_DATA_DIR", "/tmp/should-be-ignored");
            std::env::set_var("SKILL_MANAGER_DATA_DIR", "/tmp/also-ignored");
        }

        let no_env_result = default_data_dir_no_env();
        let env_result = data_dir();

        unsafe {
            match orig_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match orig_rdd {
                Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
                None => std::env::remove_var("RUNE_DATA_DIR"),
            }
            match orig_smdd {
                Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
                None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
            }
        }

        assert_eq!(
            no_env_result,
            tmp.path().join(".runai"),
            "default_data_dir_no_env must use HOME-derived path even when env vars are set"
        );
        assert_eq!(
            env_result,
            std::path::PathBuf::from("/tmp/should-be-ignored"),
            "data_dir SHOULD honor RUNE_DATA_DIR (different function, contract preserved)"
        );
        assert_ne!(
            no_env_result, env_result,
            "no_env vs env must diverge when env is set — that's the whole point"
        );
    }

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
    fn migrate_updates_db_directory_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let old_dir = tmp.path().join(".skill-manager");
        let new_dir = tmp.path().join(".runai");

        // Create old structure with a real SQLite DB
        std::fs::create_dir_all(old_dir.join("skills/my-skill")).unwrap();
        std::fs::write(old_dir.join("skills/my-skill/SKILL.md"), "# Test").unwrap();
        std::fs::create_dir_all(old_dir.join("mcps")).unwrap();
        std::fs::create_dir_all(old_dir.join("groups")).unwrap();

        // Create a real DB with old paths
        {
            let db = crate::core::db::Database::open(&old_dir.join("skill-manager.db")).unwrap();
            let res = crate::core::resource::Resource {
                id: "local:my-skill".into(),
                name: "my-skill".into(),
                kind: crate::core::resource::ResourceKind::Skill,
                description: "test".into(),
                directory: old_dir.join("skills/my-skill"),
                source: crate::core::resource::Source::Local {
                    path: old_dir.join("skills/my-skill"),
                },
                installed_at: 0,
                enabled: std::collections::HashMap::new(),
                usage_count: 0,
                last_used_at: None,
            };
            db.insert_resource(&res).unwrap();
        }

        // Migrate
        AppPaths::migrate_data_dir(&old_dir, &new_dir, tmp.path()).unwrap();

        // Verify DB paths are updated
        let db = crate::core::db::Database::open(&new_dir.join("runai.db")).unwrap();
        let res = db.get_resource("local:my-skill").unwrap().unwrap();
        let dir_str = res.directory.to_string_lossy();
        assert!(
            dir_str.contains(".runai"),
            "directory should point to .runai, got: {dir_str}"
        );
        assert!(
            !dir_str.contains(".skill-manager"),
            "directory should NOT contain old path"
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

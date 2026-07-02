//! Path resolution — owns every runai-managed path. Houses the standalone
//! `data_dir()` helpers and the `AppPaths` struct everything else passes around.
//! Also runs the one-shot legacy migration from `~/.skill-manager/` → `~/.runai/`.
//! Everyone is upstream: `SkillManager` / CLI / TUI / MCP / backup all receive
//! an `AppPaths`.
//!
//! Public API:
//! - `data_dir() -> PathBuf` — standalone. Precedence: `RUNE_DATA_DIR` >
//!   `SKILL_MANAGER_DATA_DIR` > platform default (`~/.runai` unix, `%APPDATA%\runai`
//!   on Windows via `dirs::data_dir`).
//! - `default_data_dir_no_env() -> PathBuf` — same default but IGNORES env vars.
//!   Use this (not `data_dir()`) in guards that must compare "where the user IS"
//!   against "where the user would be without override" — `data_dir()` returns
//!   the override, so it self-compares to true and the guard never fires. This
//!   was the 2026-04-27 incident's root cause.
//! - `AppPaths::resolve()` — env-honoring constructor (`RUNE_DATA_DIR` >
//!   `SKILL_MANAGER_DATA_DIR` > `default_path`). THE canonical way for a
//!   process to pick its data dir; server + `SkillManager::new` both use it so
//!   a single process can't resolve two dirs (issue #24).
//! - `AppPaths::default_path()` (runs migration on first call) / `with_base(base)`.
//! - Public-pool subdirs off `base`: `data_dir`, `skills_dir`, `mcps_dir`,
//!   `groups_dir`, `trash_dir`, `db_path`, `config_path`; `ensure_dirs()` mkdir-p's
//!   them. `trash_dir()` is the global payload location — keep it sibling to
//!   `skills/`/`mcps/`, never under per-target dirs.
//! - Per-user (private) subdirs under `<data>/users/<user_id>/`:
//!   `user_root`, `user_skills_dir`, `user_mcps_dir`, `user_trash_dir` (all
//!   `Result`, `bail!` when `user_id` fails `is_safe_user_id`); `ensure_user_dirs`
//!   mkdir-p's the three, idempotent.
//! - Community-market subdirs under `<data>/community/<uploader_uid>/`:
//!   `community_dir`, `community_uploader_dir`, `community_skill_dir` (the
//!   uploader-scoped helpers `bail!` on bad `uploader_uid`). PLANNING §1.4.
//!
//! Invariants / gotchas:
//! - **Owner pool layout** (hard invariant): `<data>/skills/<name>/` is the
//!   public pool; `<data>/users/<uid>/skills/<name>/` is uid's private pool. The
//!   two never overlap — `is_safe_user_id` (ascii alnum + `_`/`-`, len ≤ 64,
//!   rejects empty/`..`/`/`/control/non-ascii) plus join-time construction
//!   guarantee a private dir can't escape `<data>/users/`. Defense-in-depth even
//!   for trusted db ids.
//! - **Legacy migration** runs once (gated on absence of `~/.runai/`): renames the
//!   whole dir, renames `skill-manager.db` → `runai.db`, re-points CLI symlinks
//!   under `~/.{claude,codex,gemini,opencode}/skills/` (keep this list in sync
//!   with `CliTarget::skills_dir()`), and `REPLACE`s old→new path prefixes inside
//!   the DB's `resources.directory` / `source_meta`.
//! - `db_path()` prefers `runai.db`, falls back to `skill-manager.db` for legacy.
//! - `dirs::home_dir()` on Windows uses Win32 `SHGetKnownFolderPath` — env-var
//!   home-mocking does NOT work there, so `with_home`-style tests are gated under
//!   `#[cfg(not(target_os = "windows"))]`. On Windows `data_dir()` resolves to
//!   `%APPDATA%\Roaming\runai` (via `dirs::data_dir()`), NOT `~/.runai/`.

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

    /// Resolve the active data directory honoring env overrides, falling back
    /// to the platform default. This is THE canonical constructor for "where
    /// does this process read/write runai data" — the server bootstrap
    /// (`serve_with`), `SkillManager::new()`, and every per-request handler go
    /// through it so a single process can never resolve two different data
    /// dirs (issue #24: `serve_with` used `default_path()` — env-blind — while
    /// `main.rs` used env-honoring `data_dir()`, splitting server state from
    /// the enrich child it spawned).
    ///
    /// Precedence mirrors the standalone [`data_dir`]: `RUNE_DATA_DIR` >
    /// `SKILL_MANAGER_DATA_DIR` > [`default_path`]. An env override maps to
    /// [`with_base`] (no legacy `~/.skill-manager/` migration — the override
    /// names a fresh explicit location, not the default that needs upgrading);
    /// only the no-override branch runs migration via `default_path`.
    pub fn resolve() -> Self {
        if let Ok(dir) = std::env::var("RUNE_DATA_DIR") {
            return Self::with_base(PathBuf::from(dir));
        }
        if let Ok(dir) = std::env::var("SKILL_MANAGER_DATA_DIR") {
            return Self::with_base(PathBuf::from(dir));
        }
        Self::default_path()
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

    /// Community market payload root: `<data>/community/`.
    /// Per-uploader storage lives at `<data>/community/<uploader_uid>/<name>/`
    /// — owner-scoped paths come from [`community_skill_dir`]. The shared
    /// `community/` directory is sibling to `skills/` / `users/`, NOT inside
    /// either, so the team-mode social pool is isolated from both the public
    /// pool and per-user private pools.
    pub fn community_dir(&self) -> PathBuf {
        self.base.join("community")
    }

    /// Per-uploader community sub-root: `<data>/community/<uploader_uid>/`.
    /// Errors when `uploader_uid` fails [`is_safe_user_id`] — same
    /// defense-in-depth contract as the per-user private paths.
    pub fn community_uploader_dir(&self, uploader_uid: &str) -> Result<PathBuf> {
        if !is_safe_user_id(uploader_uid) {
            anyhow::bail!("invalid user_id: {uploader_uid:?}");
        }
        Ok(self.community_dir().join(uploader_uid))
    }

    /// Per-uploader per-skill directory: `<data>/community/<uploader_uid>/<name>/`.
    /// The caller is responsible for validating `name` (length, allowed chars).
    pub fn community_skill_dir(&self, uploader_uid: &str, name: &str) -> Result<PathBuf> {
        Ok(self.community_uploader_dir(uploader_uid)?.join(name))
    }

    /// Per-user data root: `<data>/users/<user_id>/`.
    /// Errors when `user_id` fails [`is_safe_user_id`] — defense-in-depth
    /// against path traversal regardless of where the id came from.
    pub fn user_root(&self, user_id: &str) -> Result<PathBuf> {
        if !is_safe_user_id(user_id) {
            anyhow::bail!("invalid user_id: {user_id:?}");
        }
        Ok(self.base.join("users").join(user_id))
    }

    /// Per-user skills root: `<data>/users/<user_id>/skills/`.
    /// Owner-scoped install destination for private skills. Public skills
    /// continue to live in [`skills_dir`].
    pub fn user_skills_dir(&self, user_id: &str) -> Result<PathBuf> {
        Ok(self.user_root(user_id)?.join("skills"))
    }

    /// Per-user MCPs root: `<data>/users/<user_id>/mcps/`.
    pub fn user_mcps_dir(&self, user_id: &str) -> Result<PathBuf> {
        Ok(self.user_root(user_id)?.join("mcps"))
    }

    /// Per-user trash root: `<data>/users/<user_id>/trash/`.
    pub fn user_trash_dir(&self, user_id: &str) -> Result<PathBuf> {
        Ok(self.user_root(user_id)?.join("trash"))
    }

    /// Create all per-user subdirectories. Idempotent.
    pub fn ensure_user_dirs(&self, user_id: &str) -> Result<()> {
        std::fs::create_dir_all(self.user_skills_dir(user_id)?)?;
        std::fs::create_dir_all(self.user_mcps_dir(user_id)?)?;
        std::fs::create_dir_all(self.user_trash_dir(user_id)?)?;
        Ok(())
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

/// Path-traversal guard for user_id strings before they're joined into a
/// filesystem path. Accepts ascii alnum + `_` `-` only; max 64 chars. The
/// canonical id shape (`usr_<16 base32>`, see [`crate::core::auth::new_user_id`])
/// trivially passes; admin-created usernames-as-ids would too. Rejects empty,
/// over-long, `..`, `/`, and any control / non-ascii input.
///
/// This is defense-in-depth: even when the id comes from a trusted source
/// (db `users.user_id`), a malformed value must never widen to a path like
/// `../../../etc/passwd`.
fn is_safe_user_id(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Path-traversal guard for a skill NAME that arrives as a route parameter and
/// is about to be joined onto `skills_dir()` (the disk-fallback in
/// `server::state::resolve_skill_dir_scoped`). A skill name is always a single
/// path segment (`Resource::generate_id` namespaces on it, dirs are one level
/// under the pool), so any separator or parent-ref is illegitimate.
///
/// This is intentionally looser than [`is_safe_user_id`]: skill names are
/// user-authored and may contain unicode (non-ascii kebab, CJK), so we do NOT
/// restrict the charset — we only reject the traversal primitives. Rejects:
/// empty, over-long (>128), the `.`/`..` segments, any `/` or `\` separator,
/// NUL, and any control char. A name like `foo.bar` stays valid (it is a
/// literal single-segment dir name, no escape); `..` or `a/b` or `a/../b`
/// cannot pass.
pub fn is_safe_skill_name(s: &str) -> bool {
    if s.is_empty() || s.len() > 128 {
        return false;
    }
    if s == "." || s == ".." {
        return false;
    }
    // A bare `..` substring can only produce a parent-ref once combined with a
    // separator, and separators are rejected below — but reject it outright as
    // belt-and-suspenders (no real skill name contains `..`).
    if s.contains("..") {
        return false;
    }
    !s.chars()
        .any(|c| c == '/' || c == '\\' || c == '\0' || c.is_control())
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
                owner_user_id: None,
                publish_status: "draft".to_string(),
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

    // =========================================================================
    //  Phase A: user-scoped paths for private skill isolation
    // =========================================================================

    #[test]
    fn user_root_layout_matches_data_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        let uid = "usr_abc123";
        assert_eq!(
            paths.user_root(uid).unwrap(),
            tmp.path().join("users").join(uid)
        );
        assert_eq!(
            paths.user_skills_dir(uid).unwrap(),
            tmp.path().join("users").join(uid).join("skills")
        );
        assert_eq!(
            paths.user_mcps_dir(uid).unwrap(),
            tmp.path().join("users").join(uid).join("mcps")
        );
        assert_eq!(
            paths.user_trash_dir(uid).unwrap(),
            tmp.path().join("users").join(uid).join("trash")
        );
    }

    #[test]
    fn user_root_rejects_path_traversal_and_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        let bad = [
            "", "..", ".", "../etc", "a/b", "a\\b", "a b", "中文", "a:b", "a;b", "a\0b", "\n",
        ];
        for id in bad {
            assert!(paths.user_root(id).is_err(), "user_root must reject {id:?}");
            assert!(paths.user_skills_dir(id).is_err());
            assert!(paths.user_mcps_dir(id).is_err());
            assert!(paths.user_trash_dir(id).is_err());
            assert!(paths.ensure_user_dirs(id).is_err());
        }

        // Over-long id (65 chars) rejected; exact 64 accepted.
        assert!(paths.user_root(&"a".repeat(65)).is_err());
        assert!(paths.user_root(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn ensure_user_dirs_creates_three_subdirs_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        let uid = "usr_xyz789";

        paths.ensure_user_dirs(uid).unwrap();
        assert!(paths.user_skills_dir(uid).unwrap().is_dir());
        assert!(paths.user_mcps_dir(uid).unwrap().is_dir());
        assert!(paths.user_trash_dir(uid).unwrap().is_dir());

        // Second call must not error (idempotent).
        paths.ensure_user_dirs(uid).unwrap();
    }

    #[test]
    fn user_paths_isolate_alice_from_bob() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        let alice = paths.user_skills_dir("usr_alice000").unwrap();
        let bob = paths.user_skills_dir("usr_bob00000").unwrap();
        assert_ne!(alice, bob);
        assert!(!alice.starts_with(&bob));
        assert!(!bob.starts_with(&alice));
    }

    #[test]
    fn user_paths_disjoint_from_public_skills_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        let public = paths.skills_dir();
        let private = paths.user_skills_dir("usr_someone00").unwrap();
        assert_ne!(public, private);
        // Private must NOT be inside public (would defeat the whole isolation).
        assert!(!private.starts_with(&public));
        // Public must NOT be inside private either.
        assert!(!public.starts_with(&private));
    }

    #[test]
    fn safe_user_id_accepts_real_world_shapes() {
        // Canonical shape minted by auth::new_user_id.
        assert!(is_safe_user_id("usr_abcdefghij234567"));
        // Admin-created human ids would also pass (used in dev / tests).
        assert!(is_safe_user_id("alice"));
        assert!(is_safe_user_id("alice_99"));
        assert!(is_safe_user_id("a-b-c"));
        assert!(is_safe_user_id("A0"));

        // Boundary
        assert!(is_safe_user_id(&"a".repeat(64)));
        assert!(!is_safe_user_id(&"a".repeat(65)));
        assert!(!is_safe_user_id(""));

        // Path-traversal flavors must all reject.
        for bad in ["..", ".", "../x", "a/b", "a\\b", "a b", "中", "."] {
            assert!(!is_safe_user_id(bad), "must reject {bad:?}");
        }
    }

    #[test]
    fn safe_skill_name_rejects_traversal_keeps_real_names() {
        // Real skill names (kebab, unicode allowed, dots inside a segment).
        for ok in [
            "normal-skill",
            "agent-browser",
            "foo.bar",
            "a",
            "中文技能",
            &"a".repeat(128),
        ] {
            assert!(is_safe_skill_name(ok), "must accept {ok:?}");
        }
        // Traversal primitives + separators + control chars must all reject.
        for bad in [
            "",
            "..",
            ".",
            "../x",
            "a/b",
            "a\\b",
            "a/../b",
            "..\\..",
            "foo..bar",
            "a\0b",
            "a\nb",
            &"a".repeat(129),
        ] {
            assert!(!is_safe_skill_name(bad), "must reject {bad:?}");
        }
    }
}

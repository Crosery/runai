use super::SkillManager;
use crate::core::db::Database;
use crate::core::mcp_canonical::{is_corrupt, to_canonical};
use crate::core::paths::AppPaths;
use anyhow::Result;
use std::path::PathBuf;

impl SkillManager {
    pub fn new() -> Result<Self> {
        // Auto-migrate old "skill-manager" MCP entries to "runai" on first launch
        if let Some(home) = dirs::home_dir() {
            crate::core::mcp_register::McpRegister::migrate_all(&home);
        }

        let paths = AppPaths::default_path();
        paths.ensure_dirs()?;
        // Normalize MCP backups to canonical shape; quarantine corrupt ones.
        let _ = Self::migrate_mcp_backups(&paths);
        let db = Database::open(&paths.db_path())?;
        // Silent dedupe at startup: collapse any duplicate skill rows that
        // accumulated from prior installs/adopts. The header showing
        // `0/280 skills` while the list shows 278 is exactly this — fix at
        // load time so the user never sees the divergence again.
        let _ = db.dedupe_skills_by_name();
        // Sweep orphan library entries left behind by pre-2025-fix trash
        // flows so the dashboard's "我的库" count never includes rows
        // that 404 on click.
        let _ = db.cleanup_orphan_library_entries();
        Ok(Self { paths, db })
    }

    pub fn with_base(base: PathBuf) -> Result<Self> {
        let paths = AppPaths::with_base(base);
        paths.ensure_dirs()?;
        let _ = Self::migrate_mcp_backups(&paths);
        let db = Database::open(&paths.db_path())?;
        let _ = db.dedupe_skills_by_name();
        let _ = db.cleanup_orphan_library_entries();
        Ok(Self { paths, db })
    }

    /// Walk `~/.runai/mcps/*.json` and normalize backups in place:
    ///   - Rewrite OpenCode-shaped entries (command:array) into canonical (command:string + args).
    ///   - Move corrupt entries (empty command) into `mcps/.corrupt/<name>.json`.
    ///   - Leave already-canonical entries untouched (idempotent).
    ///
    /// Returns `(rewritten, quarantined)` for diagnostics. Errors are logged, never propagated.
    pub fn migrate_mcp_backups(paths: &AppPaths) -> (usize, usize) {
        let mcps_dir = paths.mcps_dir();
        if !mcps_dir.exists() {
            return (0, 0);
        }

        let entries = match std::fs::read_dir(&mcps_dir) {
            Ok(d) => d,
            Err(_) => return (0, 0),
        };

        let mut rewritten = 0usize;
        let mut quarantined = 0usize;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = match path.file_stem().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            let raw = match std::fs::read_to_string(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let value: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if is_corrupt(&value) {
                let corrupt_dir = mcps_dir.join(".corrupt");
                if std::fs::create_dir_all(&corrupt_dir).is_err() {
                    continue;
                }
                let dest = corrupt_dir.join(format!("{name}.json"));
                eprintln!(
                    "[runai] quarantining corrupt MCP backup '{name}' -> {}",
                    dest.display()
                );
                if std::fs::rename(&path, &dest).is_ok() {
                    quarantined += 1;
                }
                continue;
            }

            let canonical = to_canonical(&value);
            if canonical == value {
                continue; // already canonical
            }
            match serde_json::to_string_pretty(&canonical)
                .ok()
                .and_then(|out| std::fs::write(&path, out).ok())
            {
                Some(()) => {
                    eprintln!("[runai] normalized MCP backup '{name}' to canonical format");
                    rewritten += 1;
                }
                None => continue,
            }
        }

        (rewritten, quarantined)
    }
}

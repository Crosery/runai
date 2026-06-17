//! `runai doctor` — environment health checks and the `--fix` repair pass.
//!
//! Runs a battery of checks over `~/.runai/` layout, MCP registration in each
//! CLI config, DB reachability, and symlink health, returning results in
//! display order.
//!
//! ## Public surface
//! - `struct CheckResult { name, status, detail }`,
//!   `enum CheckStatus { Ok, Warn, Fail }`, `CheckResult::icon() -> &str`
//!   (`✓` / `△` / `✘`).
//! - `run_doctor() -> Vec<CheckResult>` — read-only; safe to run anytime.
//! - `run_doctor_fix() -> FixReport { broken_symlinks_removed, dedupe_rows_removed,
//!   orphan_users_reaped, missing_dir_rows_removed }` — the `--fix` repair pass.
//!   It (1) walks the CLI skills dirs (`~/.{claude,codex,gemini,opencode}/skills/`
//!   plus both `.opencode/skills` and `.config/opencode/skills`), removing
//!   symlinks whose target no longer exists; (2) re-runs
//!   `Database::dedupe_skills_by_name()`; (3) **reconciles orphan-user residue**
//!   (PLANNING owner/auth cleanup): for every `owner_user_id` / physical
//!   `<data>/users/<uid>/` dir whose user no longer exists, runs
//!   `SkillManager::delete_user_cascade` (trash-first, guarded — recoverable);
//!   and (4) removes public skill rows whose `directory` vanished out-of-band.
//!   Steps 3-4 are DESTRUCTIVE (move dirs to trash / delete rows) — gated behind
//!   the explicit `--fix` flag, never run by the read-only `run_doctor`.
//!
//! ## Invariants
//! - `run_doctor` is read-only. `run_doctor_fix` is the ONLY mutating surface
//!   here — call it solely from the `--fix` handler. Its dedupe is idempotent;
//!   the symlink prune is bounded to the CLI skills dirs (no walk into home root).
//! - Dedupe also runs silently in `SkillManager::new()/with_base()`, so `--fix`
//!   typically reports 0 rows — it's the explicit recovery surface for mid-session drift.
//! - Every `Fail` must carry a `detail` suggesting a concrete fix
//!   (e.g. `"Run 'runai register' to fix"`). Use `Warn` for optional features,
//!   `Fail` for things that block basic operation. Order checks user-fixable-first.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Debug)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, PartialEq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckResult {
    fn ok(name: &str, detail: &str) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Ok,
            detail: detail.into(),
        }
    }
    fn warn(name: &str, detail: &str) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warn,
            detail: detail.into(),
        }
    }
    fn fail(name: &str, detail: &str) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            detail: detail.into(),
        }
    }

    pub fn icon(&self) -> &str {
        match self.status {
            CheckStatus::Ok => "✓",
            CheckStatus::Warn => "△",
            CheckStatus::Fail => "✘",
        }
    }
}

/// Run all doctor checks and return results.
pub fn run_doctor() -> Vec<CheckResult> {
    let mut results = Vec::new();
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

    // 1. Binary check
    results.push(check_binary());

    // 2. Data directory
    let data_dir = crate::core::paths::data_dir();
    results.push(check_data_dir(&data_dir));

    // 3. Database
    results.push(check_database(&data_dir));

    // 4. MCP server health
    results.push(check_mcp_server());

    // 5. CLI registrations
    results.extend(check_cli_registrations(&home));

    // 6. Symlink health
    results.push(check_symlinks(&home));

    results
}

/// Repair operations triggered by `runai doctor --fix`.
/// Currently:
///   - Removes broken symlinks under `~/.{claude,codex,gemini,opencode}/skills/`
///     whose target no longer exists. Skill enabled-state lives in the
///     filesystem, so a dangling symlink is nothing but a stale "this used
///     to be enabled" marker — pruning it brings reality and the TUI's
///     enabled count back into sync.
///   - The DB-side dedupe already runs silently in `SkillManager::new()` /
///     `with_base()`, so this returns the count for reporting only.
pub struct FixReport {
    pub broken_symlinks_removed: Vec<String>,
    pub dedupe_rows_removed: usize,
    /// Deleted-user owners whose orphan skills + per-user subtree were reaped
    /// via `SkillManager::delete_user_cascade` (PLANNING owner/auth cleanup).
    pub orphan_users_reaped: Vec<String>,
    /// Public skill rows whose `directory` had vanished — DB rows removed.
    pub missing_dir_rows_removed: Vec<String>,
}

pub fn run_doctor_fix() -> FixReport {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let cli_skill_dirs = [
        home.join(".claude/skills"),
        home.join(".codex/skills"),
        home.join(".gemini/skills"),
        home.join(".opencode/skills"),
        home.join(".config/opencode/skills"),
    ];
    let mut removed = Vec::new();
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
            if !path.is_symlink() {
                continue;
            }
            // `path.exists()` follows the symlink — false means dangling.
            if !path.exists()
                && let Ok(()) = std::fs::remove_file(&path)
            {
                removed.push(path.display().to_string());
            }
        }
    }

    // Run a fresh dedupe pass and report the count. Manager's startup pass
    // already ran one, so this typically reports 0 — but if a duplicate
    // appeared mid-session it gets caught here.
    let data_dir = crate::core::paths::data_dir();
    let dedupe_rows_removed = match rusqlite::Connection::open(data_dir.join("runai.db")) {
        Ok(_) => match crate::core::db::Database::open(&data_dir.join("runai.db")) {
            Ok(db) => db.dedupe_skills_by_name().unwrap_or(0),
            Err(_) => 0,
        },
        Err(_) => 0,
    };

    // Orphan-user reconciliation (PLANNING owner/auth cleanup B5/D2). Reaps the
    // residue the pre-cascade delete path left: skills/dirs owned by a deleted
    // user, and public skill rows whose directory vanished out-of-band. The
    // owner reap reuses the tested `delete_user_cascade` (trash-first, guarded).
    let mut orphan_users_reaped = Vec::new();
    let mut missing_dir_rows_removed = Vec::new();
    if let Ok(mgr) = crate::core::manager::SkillManager::with_base(data_dir.clone()) {
        let mut orphan_uids: std::collections::BTreeSet<String> = Default::default();
        if let Ok(ids) = mgr.db().orphan_owner_user_ids() {
            orphan_uids.extend(ids);
        }
        // Physical <data>/users/<uid>/ dirs whose user no longer exists.
        if let Ok(rd) = std::fs::read_dir(data_dir.join("users")) {
            for e in rd.flatten() {
                if e.path().is_dir()
                    && let Some(uid) = e.file_name().to_str().map(String::from)
                    && mgr.db().find_user_by_id(&uid).ok().flatten().is_none()
                {
                    orphan_uids.insert(uid);
                }
            }
        }
        for uid in &orphan_uids {
            if mgr.delete_user_cascade(uid).is_ok() {
                orphan_users_reaped.push(uid.clone());
            }
        }
        // Public skill rows whose directory vanished (e.g. removed out-of-band).
        if let Ok(rows) = mgr
            .db()
            .list_resources_for_user(Some(crate::core::resource::ResourceKind::Skill), None)
        {
            for r in rows {
                if !r.directory.exists() && mgr.db().delete_resource(&r.id).is_ok() {
                    let _ = mgr.db().library_remove_for_all(&r.name);
                    missing_dir_rows_removed.push(r.name);
                }
            }
        }
    }

    FixReport {
        broken_symlinks_removed: removed,
        dedupe_rows_removed,
        orphan_users_reaped,
        missing_dir_rows_removed,
    }
}

/// Check if current binary is valid and executable.
fn check_binary() -> CheckResult {
    match std::env::current_exe() {
        Ok(exe) => {
            if exe.exists() {
                let version = env!("CARGO_PKG_VERSION");
                CheckResult::ok("Binary", &format!("v{version} at {}", exe.display()))
            } else {
                CheckResult::fail("Binary", &format!("path does not exist: {}", exe.display()))
            }
        }
        Err(e) => CheckResult::fail("Binary", &format!("cannot determine path: {e}")),
    }
}

/// Check data directory exists and is writable.
fn check_data_dir(data_dir: &Path) -> CheckResult {
    if !data_dir.exists() {
        return CheckResult::fail(
            "Data dir",
            &format!("{} does not exist", data_dir.display()),
        );
    }
    // Test write
    let test_file = data_dir.join(".doctor-test");
    match std::fs::write(&test_file, b"ok") {
        Ok(_) => {
            let _ = std::fs::remove_file(&test_file);
            CheckResult::ok("Data dir", &data_dir.display().to_string())
        }
        Err(_) => CheckResult::fail(
            "Data dir",
            &format!("{} is not writable", data_dir.display()),
        ),
    }
}

/// Check database file is accessible.
fn check_database(data_dir: &Path) -> CheckResult {
    let db_path = data_dir.join("runai.db");
    if !db_path.exists() {
        return CheckResult::warn(
            "Database",
            "runai.db not found (will be created on first use)",
        );
    }
    match rusqlite::Connection::open(&db_path) {
        Ok(conn) => match conn.query_row("SELECT COUNT(*) FROM resources", [], |row| {
            row.get::<_, i64>(0)
        }) {
            Ok(count) => CheckResult::ok("Database", &format!("{count} resources in DB")),
            Err(_) => CheckResult::warn("Database", "file exists but resources table missing"),
        },
        Err(e) => CheckResult::fail("Database", &format!("cannot open: {e}")),
    }
}

/// Check if MCP server can start and stay alive waiting for stdin.
/// A healthy MCP server blocks on stdin; a broken one exits immediately.
fn check_mcp_server() -> CheckResult {
    let binary = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(_) => return CheckResult::fail("MCP server", "cannot determine binary path"),
    };

    // Start with stdin piped (kept open) so server blocks waiting for input
    let mut child = match Command::new(&binary)
        .arg("mcp-serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return CheckResult::fail("MCP server", &format!("cannot start: {e}")),
    };

    // Give it a moment to crash (if it's going to)
    std::thread::sleep(Duration::from_millis(300));

    match child.try_wait() {
        Ok(None) => {
            // Still running — server is healthy, waiting for stdio input
            let _ = child.kill();
            let _ = child.wait();
            CheckResult::ok("MCP server", "starts and listens on stdio")
        }
        Ok(Some(status)) => {
            CheckResult::fail("MCP server", &format!("crashed on startup (exit {status})"))
        }
        Err(e) => CheckResult::fail("MCP server", &format!("check failed: {e}")),
    }
}

/// Check each CLI config for runai registration.
fn check_cli_registrations(home: &Path) -> Vec<CheckResult> {
    vec![
        check_json_registration(home, ".claude.json", "Claude", "mcpServers"),
        check_json_registration(home, ".gemini/settings.json", "Gemini", "mcpServers"),
        check_codex_registration(home),
        check_json_registration(home, ".config/opencode/opencode.json", "OpenCode", "mcp"),
    ]
}

fn check_json_registration(
    home: &Path,
    rel_path: &str,
    cli_name: &str,
    servers_key: &str,
) -> CheckResult {
    let path = home.join(rel_path);
    let label = format!("{cli_name} MCP");

    if !path.exists() {
        return CheckResult::warn(&label, "config file not found — not registered");
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return CheckResult::fail(&label, "cannot read config file"),
    };

    let config: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return CheckResult::fail(&label, "config file is not valid JSON"),
    };

    let entry = config.get(servers_key).and_then(|s| s.get("runai"));
    match entry {
        None => CheckResult::warn(&label, "not registered — run 'runai register'"),
        Some(e) => {
            // Check command path
            let cmd = if servers_key == "mcp" {
                // OpenCode uses command as array
                e.get("command")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
            } else {
                e.get("command").and_then(|v| v.as_str())
            };

            match cmd {
                Some(path) if Path::new(path).exists() => {
                    CheckResult::ok(&label, &format!("registered → {path}"))
                }
                Some(path) => CheckResult::fail(
                    &label,
                    &format!("binary not found: {path} — run 'runai register'"),
                ),
                None => CheckResult::warn(&label, "registered but command path missing"),
            }
        }
    }
}

fn check_codex_registration(home: &Path) -> CheckResult {
    let path = home.join(".codex/config.toml");
    let label = "Codex MCP";

    if !path.exists() {
        return CheckResult::warn(label, "config file not found — not registered");
    }

    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return CheckResult::fail(label, "cannot read config file"),
    };

    let table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(_) => return CheckResult::fail(label, "config file is not valid TOML"),
    };

    let entry = table
        .get("mcp_servers")
        .and_then(|s| s.as_table())
        .and_then(|s| s.get("runai"));

    match entry {
        None => CheckResult::warn(label, "not registered — run 'runai register'"),
        Some(e) => {
            let cmd = e
                .as_table()
                .and_then(|t| t.get("command"))
                .and_then(|v| v.as_str());
            match cmd {
                Some(path) if Path::new(path).exists() => {
                    CheckResult::ok(label, &format!("registered → {path}"))
                }
                Some(path) => CheckResult::fail(
                    label,
                    &format!("binary not found: {path} — run 'runai register'"),
                ),
                None => CheckResult::warn(label, "registered but command path missing"),
            }
        }
    }
}

/// Check for broken symlinks in skills directory.
fn check_symlinks(home: &Path) -> CheckResult {
    let skills_dir = home.join(".claude/skills");
    if !skills_dir.exists() {
        return CheckResult::warn("Symlinks", "~/.claude/skills/ not found");
    }

    let entries = match std::fs::read_dir(&skills_dir) {
        Ok(e) => e,
        Err(_) => return CheckResult::fail("Symlinks", "cannot read ~/.claude/skills/"),
    };

    let mut total = 0;
    let mut broken = 0;
    let mut broken_names = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_symlink() {
            total += 1;
            if !path.exists() {
                broken += 1;
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    broken_names.push(name.to_string());
                }
            }
        }
    }

    if broken > 0 {
        let names = broken_names.join(", ");
        CheckResult::warn("Symlinks", &format!("{broken}/{total} broken: {names}"))
    } else {
        CheckResult::ok("Symlinks", &format!("{total} symlinks, all valid"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_binary_succeeds() {
        let result = check_binary();
        assert_eq!(result.status, CheckStatus::Ok);
        assert!(result.detail.contains("v"));
    }

    #[test]
    fn check_data_dir_missing() {
        let result = check_data_dir(Path::new("/nonexistent/path"));
        assert_eq!(result.status, CheckStatus::Fail);
    }

    #[test]
    fn check_data_dir_writable() {
        let tmp = tempfile::tempdir().unwrap();
        let result = check_data_dir(tmp.path());
        assert_eq!(result.status, CheckStatus::Ok);
    }

    #[test]
    fn check_json_registration_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let result = check_json_registration(tmp.path(), "missing.json", "Test", "mcpServers");
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.detail.contains("not found"));
    }

    #[test]
    fn check_json_registration_not_registered() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("test.json"), r#"{"mcpServers":{}}"#).unwrap();
        let result = check_json_registration(tmp.path(), "test.json", "Test", "mcpServers");
        assert_eq!(result.status, CheckStatus::Warn);
        assert!(result.detail.contains("not registered"));
    }

    #[test]
    fn check_json_registration_bad_path() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("test.json"),
            r#"{"mcpServers":{"runai":{"command":"/nonexistent/runai","args":["mcp-serve"]}}}"#,
        )
        .unwrap();
        let result = check_json_registration(tmp.path(), "test.json", "Test", "mcpServers");
        assert_eq!(result.status, CheckStatus::Fail);
        assert!(result.detail.contains("not found"));
    }
}

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

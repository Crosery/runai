//! Physical end-to-end tests for runai MCP self-registration across the four
//! supported CLIs (Claude / Codex / Gemini / OpenCode).
//!
//! Each test sandboxes HOME in a fresh tempdir, leaving the real user
//! `~/.{claude,codex,gemini,opencode}/*` config files untouched. Per
//! AGENTS.md safety contract: register / unregister mutate the four CLI
//! config files, so any change here is high-risk and must run in an
//! isolated HOME with `RUNE_DATA_DIR` controlled.
#![cfg(not(target_os = "windows"))]

use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────
//
// `run_in_home*` helpers are used by later #[test] fns appended in subsequent
// commits (the `register` and `unregister` CLI features). They are kept here
// up-front to avoid touching this `use`/helpers block in append-only edits.

#[allow(dead_code)]
fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Spawn `runai <args>` with HOME pinned to the given tempdir and
/// RUNAI_NO_AUTOSPAWN=1 so the binary won't try to launch a background
/// dashboard server.
#[allow(dead_code)]
fn run_in_home(home: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = runai_cmd();
    cmd.args(args)
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd.output().expect("runai binary spawn")
}

#[allow(dead_code)]
fn run_in_home_with_data(home: &Path, data: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = runai_cmd();
    cmd.args(args)
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", data)
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd.output().expect("runai binary spawn")
}

fn read_json(path: &Path) -> serde_json::Value {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("parse JSON {}: {}", path.display(), e))
}

fn read_toml(path: &Path) -> toml::Table {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    content
        .parse()
        .unwrap_or_else(|e| panic!("parse TOML {}: {}", path.display(), e))
}

// ─── 5.23 core::mcp_register tests ─────────────────────────────────────────

/// 5.23 #1 — register_all() registers runai in all four CLI configs symmetrically.
#[test]
fn register_all_symmetric_across_targets() {
    use runai::core::mcp_register::McpRegister;

    let home = TempDir::new().unwrap();

    let result = McpRegister::register_all(home.path());
    assert!(
        result.errors.is_empty(),
        "no registration errors expected, got: {:?}",
        result.errors
    );
    assert_eq!(
        result.registered.len(),
        4,
        "all 4 CLIs should be registered, got {:?}",
        result.registered
    );

    // Claude: ~/.claude.json -> mcpServers.runai
    let claude = read_json(&home.path().join(".claude.json"));
    assert!(
        claude["mcpServers"]["runai"]["command"].is_string(),
        "Claude config missing mcpServers.runai entry"
    );
    assert_eq!(claude["mcpServers"]["runai"]["args"][0], "mcp-serve");

    // Gemini: ~/.gemini/settings.json -> mcpServers.runai
    let gemini = read_json(&home.path().join(".gemini/settings.json"));
    assert!(
        gemini["mcpServers"]["runai"]["command"].is_string(),
        "Gemini config missing mcpServers.runai entry"
    );

    // Codex: ~/.codex/config.toml -> [mcp_servers.runai]
    let codex = read_toml(&home.path().join(".codex/config.toml"));
    let codex_servers = codex
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .expect("Codex config missing [mcp_servers]");
    let codex_runai = codex_servers
        .get("runai")
        .and_then(|v| v.as_table())
        .expect("Codex config missing [mcp_servers.runai]");
    assert!(
        codex_runai.get("command").and_then(|v| v.as_str()).is_some(),
        "Codex [mcp_servers.runai] missing command"
    );

    // OpenCode: ~/.config/opencode/opencode.json -> mcp.runai
    let opencode = read_json(&home.path().join(".config/opencode/opencode.json"));
    assert!(
        opencode["mcp"]["runai"]["command"].is_array(),
        "OpenCode mcp.runai.command must be an array, got: {}",
        opencode["mcp"]["runai"]["command"]
    );
}

/// 5.23 #2 — register_all() is idempotent: re-running marks all four as skipped
/// and does not duplicate entries.
#[test]
fn register_idempotent() {
    use runai::core::mcp_register::McpRegister;

    let home = TempDir::new().unwrap();

    let first = McpRegister::register_all(home.path());
    assert!(
        first.errors.is_empty(),
        "first register errors: {:?}",
        first.errors
    );
    assert_eq!(first.registered.len(), 4, "first run registers all 4");

    // Second run: nothing should change because the binary path is unchanged.
    let second = McpRegister::register_all(home.path());
    assert!(
        second.errors.is_empty(),
        "second register errors: {:?}",
        second.errors
    );
    assert!(
        second.registered.is_empty(),
        "second run should not re-register anything, got: {:?}",
        second.registered
    );
    assert_eq!(
        second.skipped.len(),
        4,
        "second run should skip all 4, got: {:?}",
        second.skipped
    );

    // Claude config has exactly one runai entry, not duplicated.
    let claude = read_json(&home.path().join(".claude.json"));
    let servers = claude["mcpServers"]
        .as_object()
        .expect("mcpServers must be object");
    let runai_count = servers.keys().filter(|k| *k == "runai").count();
    assert_eq!(runai_count, 1, "exactly one runai entry, not duplicated");
}

/// 5.23 #4 — OpenCode `mcp.runai.command` must be a JSON array, NOT a string.
/// String form breaks OpenCode's MCP parser (see AGENTS.md MCP backup canonical
/// invariant).
#[test]
fn register_opencode_command_array() {
    use runai::core::mcp_register::McpRegister;

    let home = TempDir::new().unwrap();
    let result = McpRegister::register_all(home.path());
    assert!(
        result.registered.contains(&"opencode".to_string()),
        "opencode should be registered, registered={:?} errors={:?}",
        result.registered,
        result.errors
    );

    let opencode = read_json(&home.path().join(".config/opencode/opencode.json"));
    let command = &opencode["mcp"]["runai"]["command"];
    assert!(
        command.is_array(),
        "OpenCode command must be an array, got: {command}"
    );
    let arr = command.as_array().unwrap();
    assert!(
        arr.len() >= 2,
        "command array should have at least binary + arg, got: {arr:?}"
    );
    assert_eq!(
        arr.last().and_then(|v| v.as_str()),
        Some("mcp-serve"),
        "last element should be 'mcp-serve'"
    );
}

/// 5.23 #5 — Codex TOML entry must include `type = "stdio"` and a string
/// `command` (not array).
#[test]
fn register_codex_toml_format() {
    use runai::core::mcp_register::McpRegister;

    let home = TempDir::new().unwrap();
    let result = McpRegister::register_all(home.path());
    assert!(
        result.registered.contains(&"codex".to_string()),
        "codex should be registered, registered={:?} errors={:?}",
        result.registered,
        result.errors
    );

    let codex = read_toml(&home.path().join(".codex/config.toml"));
    let servers = codex
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .expect("missing [mcp_servers]");
    let runai = servers
        .get("runai")
        .and_then(|v| v.as_table())
        .expect("missing [mcp_servers.runai]");

    assert_eq!(
        runai.get("type").and_then(|v| v.as_str()),
        Some("stdio"),
        "Codex runai entry must have type = \"stdio\""
    );
    assert!(
        runai.get("command").and_then(|v| v.as_str()).is_some(),
        "Codex command must be a string, got: {:?}",
        runai.get("command")
    );
}

/// 5.23 #6 — register creates missing CLI dirs (Gemini's `.gemini/`,
/// Codex's `.codex/`, OpenCode's `.config/opencode/`).
#[test]
fn register_creates_cli_dirs() {
    use runai::core::mcp_register::McpRegister;

    let home = TempDir::new().unwrap();
    // Do NOT pre-create any CLI dirs.
    assert!(!home.path().join(".gemini").exists());
    assert!(!home.path().join(".codex").exists());
    assert!(!home.path().join(".config/opencode").exists());

    let result = McpRegister::register_all(home.path());
    assert!(
        result.errors.is_empty(),
        "register errors: {:?}",
        result.errors
    );

    // All four config files should now exist.
    assert!(home.path().join(".claude.json").exists());
    assert!(home.path().join(".gemini/settings.json").exists());
    assert!(home.path().join(".codex/config.toml").exists());
    assert!(
        home.path()
            .join(".config/opencode/opencode.json")
            .exists()
    );
}

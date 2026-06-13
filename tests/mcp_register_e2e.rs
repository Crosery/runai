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

// ─── 1.28 register CLI tests (high-risk: spawn real binary) ────────────────

/// 1.28 #1 — `runai register` writes runai entries to all 4 CLI configs, is
/// idempotent on a second run.
#[test]
fn register_writes_to_all_cli_configs() {
    let home = TempDir::new().unwrap();

    // Pre-create the four config files with empty JSON / TOML so we exercise
    // the "merge into existing" path on at least Claude/Gemini/OpenCode.
    std::fs::write(home.path().join(".claude.json"), "{}").unwrap();
    std::fs::create_dir_all(home.path().join(".gemini")).unwrap();
    std::fs::write(home.path().join(".gemini/settings.json"), "{}").unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    std::fs::write(home.path().join(".codex/config.toml"), "").unwrap();
    std::fs::create_dir_all(home.path().join(".config/opencode")).unwrap();
    std::fs::write(
        home.path().join(".config/opencode/opencode.json"),
        "{}",
    )
    .unwrap();

    let out = run_in_home(home.path(), &["register"]);
    assert!(
        out.status.success(),
        "runai register failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // All four config files should now have a runai entry.
    let claude = read_json(&home.path().join(".claude.json"));
    assert!(claude["mcpServers"]["runai"]["command"].is_string());

    let gemini = read_json(&home.path().join(".gemini/settings.json"));
    assert!(gemini["mcpServers"]["runai"]["command"].is_string());

    let codex = read_toml(&home.path().join(".codex/config.toml"));
    assert!(
        codex
            .get("mcp_servers")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("runai"))
            .is_some(),
        "Codex config missing [mcp_servers.runai]"
    );

    let opencode = read_json(&home.path().join(".config/opencode/opencode.json"));
    assert!(opencode["mcp"]["runai"]["command"].is_array());

    // Second run: idempotent — stdout should mention "already registered" / skip.
    let out2 = run_in_home(home.path(), &["register"]);
    assert!(
        out2.status.success(),
        "second register failed: stderr={}",
        String::from_utf8_lossy(&out2.stderr)
    );
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        stdout2.contains("already registered") || !stdout2.contains("Registered to"),
        "second register should be idempotent (no 'Registered to' lines), got: {stdout2}"
    );
}

/// 1.28 #2 — `runai register` creates missing CLI config dirs/files.
#[test]
fn register_creates_missing_dirs_via_cli() {
    let home = TempDir::new().unwrap();
    // Do NOT create any .cli dirs upfront.

    let out = run_in_home(home.path(), &["register"]);
    assert!(
        out.status.success(),
        "runai register failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(home.path().join(".gemini").is_dir(), "missing .gemini dir");
    assert!(home.path().join(".codex").is_dir(), "missing .codex dir");
    assert!(
        home.path().join(".config/opencode").is_dir(),
        "missing .config/opencode dir"
    );

    // All four config files must exist.
    assert!(home.path().join(".claude.json").exists());
    assert!(home.path().join(".gemini/settings.json").exists());
    assert!(home.path().join(".codex/config.toml").exists());
    assert!(
        home.path()
            .join(".config/opencode/opencode.json")
            .exists()
    );
}

/// 1.28 #3 — register is symmetric across the 4 targets: same binary path,
/// runai entries pointing at mcp-serve in all 4.
#[test]
fn register_symmetric_across_targets_via_cli() {
    let home = TempDir::new().unwrap();

    let out = run_in_home(home.path(), &["register"]);
    assert!(out.status.success(), "register failed");

    let claude = read_json(&home.path().join(".claude.json"));
    let gemini = read_json(&home.path().join(".gemini/settings.json"));
    let codex = read_toml(&home.path().join(".codex/config.toml"));
    let opencode = read_json(&home.path().join(".config/opencode/opencode.json"));

    let claude_cmd = claude["mcpServers"]["runai"]["command"]
        .as_str()
        .expect("Claude runai.command must be string")
        .to_string();
    let gemini_cmd = gemini["mcpServers"]["runai"]["command"]
        .as_str()
        .expect("Gemini runai.command must be string")
        .to_string();
    let codex_cmd = codex["mcp_servers"]["runai"]
        .as_table()
        .unwrap()
        .get("command")
        .and_then(|v| v.as_str())
        .expect("Codex runai.command must be string")
        .to_string();
    let opencode_cmd = opencode["mcp"]["runai"]["command"][0]
        .as_str()
        .expect("OpenCode runai.command[0] must be string")
        .to_string();

    // All four configs must point at the same binary path.
    assert_eq!(
        claude_cmd, gemini_cmd,
        "Claude vs Gemini binary path mismatch"
    );
    assert_eq!(
        claude_cmd, codex_cmd,
        "Claude vs Codex binary path mismatch"
    );
    assert_eq!(
        claude_cmd, opencode_cmd,
        "Claude vs OpenCode binary path mismatch"
    );

    // All four configs must invoke mcp-serve.
    assert_eq!(claude["mcpServers"]["runai"]["args"][0], "mcp-serve");
    assert_eq!(gemini["mcpServers"]["runai"]["args"][0], "mcp-serve");
    let codex_args = codex["mcp_servers"]["runai"]
        .as_table()
        .unwrap()
        .get("args")
        .and_then(|v| v.as_array())
        .expect("Codex args must be array");
    assert_eq!(
        codex_args.first().and_then(|v| v.as_str()),
        Some("mcp-serve")
    );
    assert_eq!(opencode["mcp"]["runai"]["command"][1], "mcp-serve");
}

/// 1.28 #4 — register acts on HOME, not RUNE_DATA_DIR. Changing RUNE_DATA_DIR
/// must not affect which config files get touched, and the CLI configs must
/// still be home-rooted.
#[test]
fn register_ignores_rune_data_dir() {
    let home = TempDir::new().unwrap();
    let alt_data = TempDir::new().unwrap();
    let alt_path = alt_data.path().join("runai-alt");

    let out = run_in_home_with_data(home.path(), &alt_path, &["register"]);
    assert!(
        out.status.success(),
        "runai register failed under RUNE_DATA_DIR={}: stderr={}",
        alt_path.display(),
        String::from_utf8_lossy(&out.stderr)
    );

    // CLI configs MUST be at HOME-rooted paths, not under the alt data dir.
    assert!(
        home.path().join(".claude.json").exists(),
        "Claude config must be at HOME/.claude.json"
    );
    assert!(
        home.path().join(".gemini/settings.json").exists(),
        "Gemini config must be at HOME/.gemini/settings.json"
    );
    assert!(
        home.path().join(".codex/config.toml").exists(),
        "Codex config must be at HOME/.codex/config.toml"
    );
    assert!(
        home.path()
            .join(".config/opencode/opencode.json")
            .exists(),
        "OpenCode config must be at HOME/.config/opencode/opencode.json"
    );

    // RUNE_DATA_DIR must NOT contain any CLI config files: register writes
    // to HOME-rooted dotfiles, not to the data dir.
    assert!(
        !alt_path.join(".claude.json").exists(),
        "RUNE_DATA_DIR must NOT contain CLI config files"
    );
    assert!(
        !alt_path.join(".gemini").exists(),
        "RUNE_DATA_DIR must NOT contain .gemini dir"
    );
    assert!(
        !alt_path.join(".codex").exists(),
        "RUNE_DATA_DIR must NOT contain .codex dir"
    );
}

// ─── 1.29 unregister CLI tests ──────────────────────────────────────────────

/// 1.29 #1 — `runai unregister` removes the runai entry from all 4 CLI configs
/// while preserving any other MCP entries the user had.
#[test]
fn unregister_removes_runai_from_all_configs() {
    let home = TempDir::new().unwrap();

    // Set up all four config files with both runai and a sibling "other" MCP.
    std::fs::write(
        home.path().join(".claude.json"),
        r#"{"mcpServers":{"runai":{"command":"runai","args":["mcp-serve"]},"other":{"command":"foo"}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join(".gemini")).unwrap();
    std::fs::write(
        home.path().join(".gemini/settings.json"),
        r#"{"mcpServers":{"runai":{"command":"runai","args":["mcp-serve"]},"other":{"command":"bar"}}}"#,
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    std::fs::write(
        home.path().join(".codex/config.toml"),
        r#"
[mcp_servers.runai]
type = "stdio"
command = "runai"
args = ["mcp-serve"]

[mcp_servers.other]
type = "stdio"
command = "baz"
"#,
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join(".config/opencode")).unwrap();
    std::fs::write(
        home.path().join(".config/opencode/opencode.json"),
        r#"{"mcp":{"runai":{"command":["runai","mcp-serve"],"enabled":true,"type":"local"},"other":{"command":["bar","x"],"enabled":true,"type":"local"}}}"#,
    )
    .unwrap();

    let out = run_in_home(home.path(), &["unregister"]);
    assert!(
        out.status.success(),
        "runai unregister failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Unregistered"),
        "stdout should mention 'Unregistered', got: {stdout}"
    );

    // Claude: runai gone, other preserved.
    let claude = read_json(&home.path().join(".claude.json"));
    assert!(
        claude["mcpServers"].get("runai").is_none()
            || claude["mcpServers"]["runai"].is_null(),
        "Claude runai entry should be removed"
    );
    assert!(
        claude["mcpServers"]["other"].is_object(),
        "Claude 'other' MCP should be preserved"
    );

    // Gemini: runai gone, other preserved.
    let gemini = read_json(&home.path().join(".gemini/settings.json"));
    assert!(
        gemini["mcpServers"].get("runai").is_none()
            || gemini["mcpServers"]["runai"].is_null(),
        "Gemini runai entry should be removed"
    );
    assert!(
        gemini["mcpServers"]["other"].is_object(),
        "Gemini 'other' MCP should be preserved"
    );

    // Codex: runai gone, other preserved.
    let codex = read_toml(&home.path().join(".codex/config.toml"));
    let codex_servers = codex
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .expect("[mcp_servers] must remain");
    assert!(
        codex_servers.get("runai").is_none(),
        "Codex runai entry should be removed"
    );
    assert!(
        codex_servers.get("other").is_some(),
        "Codex 'other' MCP should be preserved"
    );

    // OpenCode: runai gone, other preserved.
    let opencode = read_json(&home.path().join(".config/opencode/opencode.json"));
    assert!(
        opencode["mcp"].get("runai").is_none()
            || opencode["mcp"]["runai"].is_null(),
        "OpenCode runai entry should be removed"
    );
    assert!(
        opencode["mcp"]["other"].is_object(),
        "OpenCode 'other' MCP should be preserved"
    );
}

/// 1.29 #2 — unregister is symmetric: register all 4, then unregister all 4,
/// none of the 4 configs should still have a runai entry.
#[test]
fn unregister_symmetric_across_targets() {
    let home = TempDir::new().unwrap();

    // First register so all four configs end up with runai entries.
    let r_out = run_in_home(home.path(), &["register"]);
    assert!(r_out.status.success(), "register failed");

    // Then unregister.
    let u_out = run_in_home(home.path(), &["unregister"]);
    assert!(
        u_out.status.success(),
        "unregister failed: stderr={}",
        String::from_utf8_lossy(&u_out.stderr)
    );

    let claude = read_json(&home.path().join(".claude.json"));
    assert!(
        claude["mcpServers"].get("runai").is_none()
            || claude["mcpServers"]["runai"].is_null(),
        "Claude still has runai entry after unregister"
    );

    let gemini = read_json(&home.path().join(".gemini/settings.json"));
    assert!(
        gemini["mcpServers"].get("runai").is_none()
            || gemini["mcpServers"]["runai"].is_null(),
        "Gemini still has runai entry after unregister"
    );

    let codex = read_toml(&home.path().join(".codex/config.toml"));
    let codex_runai = codex
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("runai"));
    assert!(
        codex_runai.is_none(),
        "Codex still has [mcp_servers.runai] after unregister"
    );

    let opencode = read_json(&home.path().join(".config/opencode/opencode.json"));
    assert!(
        opencode["mcp"].get("runai").is_none()
            || opencode["mcp"]["runai"].is_null(),
        "OpenCode still has runai entry after unregister"
    );
}

/// 1.29 #3 — unregister is idempotent: with no config files present, the
/// command must exit 0 and not crash.
#[test]
fn unregister_unit_idempotent() {
    let home = TempDir::new().unwrap();
    // No .cli config files anywhere.

    let out = run_in_home(home.path(), &["unregister"]);
    assert!(
        out.status.success(),
        "unregister on empty HOME should succeed (exit 0): stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // No config files should have been created by unregister itself.
    assert!(
        !home.path().join(".claude.json").exists(),
        "unregister must not create .claude.json out of thin air"
    );
    assert!(
        !home.path().join(".gemini/settings.json").exists(),
        "unregister must not create .gemini/settings.json out of thin air"
    );
}

//! P2 e2e tests for `runai register` and `runai unregister`.
//!
//! These tests drive the real `runai` binary against an isolated HOME
//! tempdir to verify that the self-registration / un-registration commands
//! correctly mutate the 4 CLI config files (Claude / Gemini / Codex /
//! OpenCode) without touching anything outside the sandbox.
//!
//! Skipped on Windows because `dirs::home_dir()` ignores the HOME env var
//! there (Win32 SHGetKnownFolderPath), so the sandbox does not isolate.

#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

fn runai_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runai"))
}

/// Spawn the cargo-built runai binary with an isolated HOME and RUNE_DATA_DIR.
fn run_runai(home: &Path, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let data_dir = home.join(".runai");
    let out = Command::new(runai_binary())
        .env("HOME", home)
        .env("RUNE_DATA_DIR", &data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .args(args)
        .output()
        .expect("runai binary failed to spawn");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status,
    )
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn read_toml(path: &Path) -> toml::Table {
    std::fs::read_to_string(path).unwrap().parse().unwrap()
}

/// `runai register` writes a runai entry into all four CLI config files.
/// Re-running emits "already registered" for each CLI (idempotency).
#[test]
fn register_writes_to_all_cli_configs() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // First run: should create the 4 config files with runai entries.
    let (stdout, stderr, status) = run_runai(home, &["register"]);
    assert!(
        status.success(),
        "register failed: status={status:?}\nstdout: {stdout}\nstderr: {stderr}"
    );

    // Claude config
    let claude = read_json(&home.join(".claude.json"));
    assert!(
        claude["mcpServers"]["runai"]["command"].is_string(),
        "claude config missing runai.command. content: {claude}"
    );
    assert_eq!(claude["mcpServers"]["runai"]["args"][0], "mcp-serve");

    // Gemini config
    let gemini = read_json(&home.join(".gemini/settings.json"));
    assert!(
        gemini["mcpServers"]["runai"]["command"].is_string(),
        "gemini config missing runai.command. content: {gemini}"
    );
    assert_eq!(gemini["mcpServers"]["runai"]["args"][0], "mcp-serve");

    // Codex TOML config
    let codex = read_toml(&home.join(".codex/config.toml"));
    let codex_runai = codex["mcp_servers"].as_table().unwrap()["runai"]
        .as_table()
        .unwrap();
    assert!(
        codex_runai["command"].as_str().is_some(),
        "codex config missing runai.command"
    );
    let args = codex_runai["args"].as_array().unwrap();
    assert_eq!(args[0].as_str(), Some("mcp-serve"));

    // OpenCode config (command is an array)
    let opencode = read_json(&home.join(".config/opencode/opencode.json"));
    let oc_runai = &opencode["mcp"]["runai"];
    assert!(oc_runai["command"].is_array(), "opencode command not array");
    assert_eq!(oc_runai["command"][1], "mcp-serve");

    // stdout should mention all 4 CLIs were registered.
    for cli in &["claude", "gemini", "codex", "opencode"] {
        assert!(
            stdout.contains(cli),
            "stdout missing CLI '{cli}'. stdout was: {stdout}"
        );
    }

    // Second run: should be idempotent — every CLI marked "already registered".
    let (stdout2, _stderr2, status2) = run_runai(home, &["register"]);
    assert!(status2.success(), "second register failed");
    for cli in &["claude", "gemini", "codex", "opencode"] {
        assert!(
            stdout2.contains(&format!("{cli} (already registered)")),
            "expected '{cli} (already registered)' in stdout: {stdout2}"
        );
    }
}

/// `runai register` creates missing CLI config directories and files.
#[test]
fn register_creates_missing_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Sanity: none of the CLI dirs exist yet.
    assert!(!home.join(".gemini").exists());
    assert!(!home.join(".codex").exists());
    assert!(!home.join(".config/opencode").exists());

    let (_stdout, stderr, status) = run_runai(home, &["register"]);
    assert!(status.success(), "register failed. stderr: {stderr}");

    // All 4 config files now exist.
    assert!(home.join(".claude.json").is_file(), "claude config missing");
    assert!(
        home.join(".gemini/settings.json").is_file(),
        "gemini config missing"
    );
    assert!(
        home.join(".codex/config.toml").is_file(),
        "codex config missing"
    );
    assert!(
        home.join(".config/opencode/opencode.json").is_file(),
        "opencode config missing"
    );
}

/// All 4 targets get the same runai binary path. The runai entry uses
/// `mcp-serve` as the first arg uniformly across all targets.
#[test]
fn register_symmetric_across_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let (_stdout, _stderr, status) = run_runai(home, &["register"]);
    assert!(status.success(), "register failed");

    let claude = read_json(&home.join(".claude.json"));
    let claude_bin = claude["mcpServers"]["runai"]["command"]
        .as_str()
        .unwrap()
        .to_string();

    let gemini = read_json(&home.join(".gemini/settings.json"));
    let gemini_bin = gemini["mcpServers"]["runai"]["command"]
        .as_str()
        .unwrap()
        .to_string();

    let codex = read_toml(&home.join(".codex/config.toml"));
    let codex_bin = codex["mcp_servers"].as_table().unwrap()["runai"]
        .as_table()
        .unwrap()["command"]
        .as_str()
        .unwrap()
        .to_string();

    let opencode = read_json(&home.join(".config/opencode/opencode.json"));
    let opencode_bin = opencode["mcp"]["runai"]["command"][0]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(
        claude_bin, gemini_bin,
        "claude vs gemini binary path differs"
    );
    assert_eq!(claude_bin, codex_bin, "claude vs codex binary path differs");
    assert_eq!(
        claude_bin, opencode_bin,
        "claude vs opencode binary path differs"
    );
    // All entries point at mcp-serve.
    assert_eq!(claude["mcpServers"]["runai"]["args"][0], "mcp-serve");
    assert_eq!(gemini["mcpServers"]["runai"]["args"][0], "mcp-serve");
    assert_eq!(opencode["mcp"]["runai"]["command"][1], "mcp-serve");
}

/// `runai register` writes to `~/.{claude,gemini,codex,opencode}` (HOME-rooted),
/// not under `RUNE_DATA_DIR`. Setting RUNE_DATA_DIR to a different path should
/// not affect what `register` does.
#[test]
fn register_ignores_rune_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let alt_data = tempfile::tempdir().unwrap();

    // Run register with an explicit RUNE_DATA_DIR pointing elsewhere.
    let out = Command::new(runai_binary())
        .env("HOME", home)
        .env("RUNE_DATA_DIR", alt_data.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .arg("register")
        .output()
        .expect("runai binary failed to spawn");
    assert!(
        out.status.success(),
        "register failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // CLI configs should be under HOME, not under alt_data.
    assert!(home.join(".claude.json").is_file(), "claude config under HOME missing");
    assert!(home.join(".gemini/settings.json").is_file());
    assert!(home.join(".codex/config.toml").is_file());
    assert!(home.join(".config/opencode/opencode.json").is_file());

    // alt_data dir should NOT have CLI configs written into it.
    assert!(
        !alt_data.path().join(".claude.json").exists(),
        "register polluted RUNE_DATA_DIR: {}/.claude.json exists",
        alt_data.path().display()
    );
    assert!(!alt_data.path().join(".gemini").exists());
    assert!(!alt_data.path().join(".codex").exists());
}

// ---------------------------------------------------------------------------
// unregister tests
// ---------------------------------------------------------------------------

/// Seed each CLI config file with both a `runai` entry and at least one
/// unrelated MCP entry. `unregister` should strip only `runai`, never the
/// neighbour MCPs.
#[test]
fn unregister_removes_runai_from_all_configs() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Seed Claude config with runai + a sibling MCP.
    let claude_seed = serde_json::json!({
        "mcpServers": {
            "runai": {"command": "/bin/runai", "args": ["mcp-serve"]},
            "other-mcp": {"command": "/bin/other", "args": ["serve"]}
        }
    });
    std::fs::write(
        home.join(".claude.json"),
        serde_json::to_string_pretty(&claude_seed).unwrap(),
    )
    .unwrap();

    // Seed Gemini config.
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    let gemini_seed = serde_json::json!({
        "mcpServers": {
            "runai": {"command": "/bin/runai", "args": ["mcp-serve"]},
            "gemini-side": {"command": "/bin/side", "args": []}
        }
    });
    std::fs::write(
        home.join(".gemini/settings.json"),
        serde_json::to_string_pretty(&gemini_seed).unwrap(),
    )
    .unwrap();

    // Seed Codex TOML config.
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(
        home.join(".codex/config.toml"),
        r#"
[mcp_servers.runai]
type = "stdio"
command = "/bin/runai"
args = ["mcp-serve"]

[mcp_servers.codex-side]
command = "/bin/codex-side"
args = ["go"]
"#,
    )
    .unwrap();

    // Seed OpenCode config.
    std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
    let opencode_seed = serde_json::json!({
        "mcp": {
            "runai": {"command": ["/bin/runai", "mcp-serve"], "enabled": true, "type": "local"},
            "opencode-side": {"command": ["/bin/oc-side"], "enabled": true, "type": "local"}
        }
    });
    std::fs::write(
        home.join(".config/opencode/opencode.json"),
        serde_json::to_string_pretty(&opencode_seed).unwrap(),
    )
    .unwrap();

    let (stdout, stderr, status) = run_runai(home, &["unregister"]);
    assert!(
        status.success(),
        "unregister failed: stderr={stderr}\nstdout={stdout}"
    );
    assert!(
        stdout.contains("Unregistered from all CLIs"),
        "unexpected unregister stdout: {stdout}"
    );

    // Claude: runai gone, sibling kept.
    let claude = read_json(&home.join(".claude.json"));
    assert!(
        claude["mcpServers"].get("runai").is_none(),
        "claude still has runai entry: {claude}"
    );
    assert!(
        claude["mcpServers"].get("other-mcp").is_some(),
        "claude lost sibling MCP: {claude}"
    );

    // Gemini: runai gone, sibling kept.
    let gemini = read_json(&home.join(".gemini/settings.json"));
    assert!(gemini["mcpServers"].get("runai").is_none());
    assert!(gemini["mcpServers"].get("gemini-side").is_some());

    // Codex: runai gone, sibling kept.
    let codex = read_toml(&home.join(".codex/config.toml"));
    let codex_servers = codex["mcp_servers"].as_table().unwrap();
    assert!(
        !codex_servers.contains_key("runai"),
        "codex still has runai entry"
    );
    assert!(
        codex_servers.contains_key("codex-side"),
        "codex lost sibling MCP"
    );

    // OpenCode: runai gone, sibling kept.
    let opencode = read_json(&home.join(".config/opencode/opencode.json"));
    assert!(opencode["mcp"].get("runai").is_none());
    assert!(opencode["mcp"].get("opencode-side").is_some());
}

/// Register then unregister: all 4 CLI configs lose the runai entry.
/// Verifies no target is left half-registered (4-target symmetry).
#[test]
fn unregister_symmetric_across_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Plant runai entries via the real register command.
    let (_o1, e1, st1) = run_runai(home, &["register"]);
    assert!(st1.success(), "register failed: {e1}");
    // Sanity: claude has runai.
    assert!(
        read_json(&home.join(".claude.json"))["mcpServers"]
            .get("runai")
            .is_some()
    );

    // Now unregister.
    let (stdout, stderr, st2) = run_runai(home, &["unregister"]);
    assert!(st2.success(), "unregister failed: stderr={stderr}");
    assert!(stdout.contains("Unregistered from all CLIs"));

    // All 4 configs must be free of runai — no target preference.
    let claude = read_json(&home.join(".claude.json"));
    assert!(
        claude.get("mcpServers")
            .and_then(|s| s.get("runai"))
            .is_none(),
        "claude still has runai after unregister: {claude}"
    );

    let gemini = read_json(&home.join(".gemini/settings.json"));
    assert!(
        gemini.get("mcpServers")
            .and_then(|s| s.get("runai"))
            .is_none(),
        "gemini still has runai after unregister: {gemini}"
    );

    let codex = read_toml(&home.join(".codex/config.toml"));
    if let Some(toml::Value::Table(servers)) = codex.get("mcp_servers") {
        assert!(
            !servers.contains_key("runai"),
            "codex still has runai after unregister: {codex:?}"
        );
    }

    let opencode = read_json(&home.join(".config/opencode/opencode.json"));
    assert!(
        opencode.get("mcp").and_then(|s| s.get("runai")).is_none(),
        "opencode still has runai after unregister: {opencode}"
    );
}

/// Running `unregister` against an empty HOME (no config files exist) should
/// succeed silently — guaranteed idempotency, no crash on missing inputs.
#[test]
fn unregister_unit_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // No CLI configs present, no registration ever happened.
    assert!(!home.join(".claude.json").exists());
    assert!(!home.join(".gemini/settings.json").exists());
    assert!(!home.join(".codex/config.toml").exists());
    assert!(!home.join(".config/opencode/opencode.json").exists());

    let (stdout, stderr, status) = run_runai(home, &["unregister"]);
    assert!(
        status.success(),
        "unregister with no configs failed: stderr={stderr}\nstdout={stdout}"
    );
    // Even with nothing to do, the success message is emitted.
    assert!(
        stdout.contains("Unregistered from all CLIs"),
        "unexpected stdout: {stdout}"
    );

    // Calling it a second time must also succeed (true idempotency).
    let (stdout2, _stderr2, status2) = run_runai(home, &["unregister"]);
    assert!(status2.success(), "second unregister failed");
    assert!(stdout2.contains("Unregistered from all CLIs"));
}

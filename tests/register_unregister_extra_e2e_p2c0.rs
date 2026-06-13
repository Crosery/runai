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

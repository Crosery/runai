//! Physical e2e: drive the real `runai` binary against an isolated HOME
//! to verify cross-CLI MCP enable/disable round-trips the canonical schema.
//!
//! These tests exist because the 2026-04-28 incident showed that MCPs
//! disabled from OpenCode (command:array shape) got written verbatim into
//! `~/.claude.json`, breaking Claude Code's MCP config parser. Lib tests
//! cover the `manager.rs` logic; this file pins the binary-level wiring
//! so a regression cannot slip past clap dispatch unnoticed.
//!
//! Skipped on Windows: HOME mocking via env var is unix-only (see manager tests).

#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

fn runai_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runai"))
}

/// Run runai with isolated HOME and RUNE_DATA_DIR. Returns (stdout, stderr, status).
fn run_runai(home: &Path, args: &[&str]) -> (String, String, std::process::ExitStatus) {
    let data_dir = home.join(".runai");
    let out = Command::new(runai_binary())
        .env("HOME", home)
        .env("RUNE_DATA_DIR", &data_dir)
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

#[test]
fn e2e_disable_opencode_then_enable_claude_writes_canonical_to_claude() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Pre-existing OpenCode config with `crosery-search` registered natively.
    let oc_dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&oc_dir).unwrap();
    std::fs::write(
        oc_dir.join("opencode.json"),
        r#"{
            "mcp": {
                "crosery-search": {
                    "command": ["/bin/crosery-search", "--port", "9999"],
                    "enabled": true,
                    "type": "local"
                }
            }
        }"#,
    )
    .unwrap();
    std::fs::write(home.join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();

    let (_, stderr, status) =
        run_runai(home, &["disable", "crosery-search", "--target", "opencode"]);
    assert!(status.success(), "disable opencode failed: stderr={stderr}");

    let (_, stderr, status) = run_runai(home, &["enable", "crosery-search", "--target", "claude"]);
    assert!(status.success(), "enable claude failed: stderr={stderr}");

    let claude = read_json(&home.join(".claude.json"));
    let entry = &claude["mcpServers"]["crosery-search"];
    assert_eq!(
        entry["command"],
        serde_json::json!("/bin/crosery-search"),
        "Claude entry must have command as string, not OpenCode array"
    );
    assert_eq!(entry["args"], serde_json::json!(["--port", "9999"]));
    assert!(
        entry.get("enabled").is_none(),
        "Claude entry must not carry OpenCode `enabled` key"
    );
    assert_ne!(
        entry.get("type").and_then(|v| v.as_str()),
        Some("local"),
        "Claude entry must not carry OpenCode `type:local`"
    );
}

#[test]
fn e2e_disable_claude_then_enable_opencode_emits_command_array() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    std::fs::write(
        home.join(".claude.json"),
        r#"{"mcpServers":{"foo":{"command":"/bin/foo","args":["x","y"]}}}"#,
    )
    .unwrap();
    let oc_dir = home.join(".config").join("opencode");
    std::fs::create_dir_all(&oc_dir).unwrap();
    std::fs::write(oc_dir.join("opencode.json"), r#"{"mcp":{}}"#).unwrap();

    let (_, stderr, status) = run_runai(home, &["disable", "foo", "--target", "claude"]);
    assert!(status.success(), "disable claude failed: stderr={stderr}");
    let (_, stderr, status) = run_runai(home, &["enable", "foo", "--target", "opencode"]);
    assert!(status.success(), "enable opencode failed: stderr={stderr}");

    let oc = read_json(&oc_dir.join("opencode.json"));
    let entry = &oc["mcp"]["foo"];
    assert_eq!(
        entry["command"],
        serde_json::json!(["/bin/foo", "x", "y"]),
        "OpenCode entry must merge cmd+args into single array"
    );
    assert_eq!(entry["enabled"], serde_json::json!(true));
    assert_eq!(entry["type"], serde_json::json!("local"));
}

#[test]
fn e2e_corrupt_backup_quarantined_at_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // Plant a corrupt backup before binary runs
    let mcps_dir = home.join(".runai").join("mcps");
    std::fs::create_dir_all(&mcps_dir).unwrap();
    std::fs::write(
        mcps_dir.join("broken.json"),
        r#"{"command":[""],"enabled":true,"type":"local"}"#,
    )
    .unwrap();

    // Run any command that constructs SkillManager (status is cheap)
    let (_, _, status) = run_runai(home, &["status"]);
    assert!(status.success());

    assert!(
        !mcps_dir.join("broken.json").exists(),
        "corrupt backup must be moved out of mcps/"
    );
    assert!(
        mcps_dir.join(".corrupt").join("broken.json").exists(),
        "corrupt backup must land in mcps/.corrupt/"
    );
}

#[test]
fn e2e_opencode_shaped_backup_normalized_at_startup() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let mcps_dir = home.join(".runai").join("mcps");
    std::fs::create_dir_all(&mcps_dir).unwrap();
    std::fs::write(
        mcps_dir.join("foo.json"),
        r#"{"command":["/bin/foo","arg1","arg2"],"enabled":true,"type":"local"}"#,
    )
    .unwrap();

    let (_, _, status) = run_runai(home, &["status"]);
    assert!(status.success());

    let after = read_json(&mcps_dir.join("foo.json"));
    assert_eq!(
        after["command"],
        serde_json::json!("/bin/foo"),
        "command normalized to string"
    );
    assert_eq!(after["args"], serde_json::json!(["arg1", "arg2"]));
    assert!(
        after.get("enabled").is_none(),
        "OpenCode-only `enabled` stripped"
    );
    assert!(
        after.get("type").is_none(),
        "OpenCode-only `type:local` stripped"
    );
}

#[test]
fn e2e_codex_disable_enable_preserves_tools_and_env_subtables() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    std::fs::write(
        codex_dir.join("config.toml"),
        r#"[mcp_servers.design-gateway]
type = "stdio"
command = "/bin/dg"
args = ["server.js"]

[mcp_servers.design-gateway.env]
DG_KEY = "secret-value"

[mcp_servers.design-gateway.tools.cdp_navigate]
approval_mode = "approve"

[mcp_servers.design-gateway.tools.export_node_as_image]
approval_mode = "approve"
"#,
    )
    .unwrap();

    let (_, stderr, status) = run_runai(home, &["disable", "design-gateway", "--target", "codex"]);
    assert!(status.success(), "disable codex failed: {stderr}");
    let (_, stderr, status) = run_runai(home, &["enable", "design-gateway", "--target", "codex"]);
    assert!(status.success(), "enable codex failed: {stderr}");

    let after = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
    assert!(
        after.contains("DG_KEY = \"secret-value\""),
        "env subtable preserved: {after}"
    );
    assert!(after.contains("cdp_navigate"), "tool 1 preserved: {after}");
    assert!(
        after.contains("export_node_as_image"),
        "tool 2 preserved: {after}"
    );
    assert!(
        after.contains("approval_mode = \"approve\""),
        "approval_mode preserved: {after}"
    );
}

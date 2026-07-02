//! P2 extra e2e tests for `runai register` / `runai unregister`.
//!
//! Spawns the workspace-built `runai` binary (`env!("CARGO_BIN_EXE_runai")`)
//! inside an isolated `HOME=$(mktemp -d)` with `RUNAI_NO_AUTOSPAWN=1` and a scoped
//! `RUNE_DATA_DIR`, then asserts the 4 CLI config files (`.claude.json`,
//! `.gemini/settings.json`, `.codex/config.toml`,
//! `.config/opencode/opencode.json`) are touched in the right shape and
//! never reach into the real user home.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

fn runai_cmd(home: &Path) -> Command {
    let mut cmd = Command::new(RUNAI_BIN);
    cmd.env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", home.join(".runai"))
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd
}

fn scratch_home() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\nstdout:\n{}\nstderr:\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn read_json(path: &Path) -> serde_json::Value {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|_| panic!("read {} failed", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|_| panic!("parse {} as json failed", path.display()))
}

fn read_toml(path: &Path) -> toml::Value {
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|_| panic!("read {} failed", path.display()));
    raw.parse::<toml::Value>()
        .unwrap_or_else(|_| panic!("parse {} as toml failed", path.display()))
}

fn claude_config(home: &Path) -> PathBuf {
    home.join(".claude.json")
}
fn gemini_config(home: &Path) -> PathBuf {
    home.join(".gemini/settings.json")
}
fn codex_config(home: &Path) -> PathBuf {
    home.join(".codex/config.toml")
}
fn opencode_config(home: &Path) -> PathBuf {
    home.join(".config/opencode/opencode.json")
}

// ---------------------------------------------------------------------------
// 1.28 register
// ---------------------------------------------------------------------------

#[test]
fn register_writes_to_all_cli_configs() {
    let home_t = scratch_home();
    let home = home_t.path();

    // Pre-create the 4 CLI configs with a foreign MCP so we can prove
    // existing entries survive register.
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
    std::fs::write(
        claude_config(home),
        r#"{"mcpServers":{"other":{"command":"x","args":[]}}}"#,
    )
    .unwrap();
    std::fs::write(
        gemini_config(home),
        r#"{"general":{"k":"v"},"mcpServers":{"other":{"command":"y","args":[]}}}"#,
    )
    .unwrap();
    std::fs::write(
        codex_config(home),
        "[mcp_servers.other]\ncommand = \"z\"\nargs = []\n",
    )
    .unwrap();
    std::fs::write(
        opencode_config(home),
        r#"{"mcp":{"other":{"command":["q","arg"],"enabled":true,"type":"local"}}}"#,
    )
    .unwrap();

    let out = runai_cmd(home).arg("register").output().unwrap();
    dump(&out, "register pass 1");
    assert!(out.status.success(), "register must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("claude")
            && stdout.contains("gemini")
            && stdout.contains("codex")
            && stdout.contains("opencode"),
        "register stdout must list all 4 CLIs: got {stdout:?}"
    );

    // 1) .claude.json: mcpServers.runai.command = absolute path, mcpServers.other survives
    let cj = read_json(&claude_config(home));
    assert!(
        cj["mcpServers"]["runai"]["command"].as_str().is_some(),
        ".claude.json missing mcpServers.runai.command"
    );
    let cmd = cj["mcpServers"]["runai"]["command"].as_str().unwrap();
    assert!(
        Path::new(cmd).is_absolute(),
        "claude command must be absolute path, got {cmd:?}"
    );
    assert_eq!(
        cj["mcpServers"]["runai"]["args"][0]
            .as_str()
            .unwrap_or_default(),
        "mcp-serve",
        "claude args[0] must be mcp-serve"
    );
    assert!(
        cj["mcpServers"]["other"]["command"].is_string(),
        "preexisting claude entry must survive"
    );

    // 2) .gemini/settings.json
    let gj = read_json(&gemini_config(home));
    assert!(gj["mcpServers"]["runai"]["command"].as_str().is_some());
    assert_eq!(
        gj["mcpServers"]["runai"]["args"][0].as_str().unwrap(),
        "mcp-serve"
    );
    assert!(gj["mcpServers"]["other"]["command"].is_string());
    assert_eq!(gj["general"]["k"], serde_json::json!("v"));

    // 3) .codex/config.toml: [mcp_servers.runai] table
    let cx = read_toml(&codex_config(home));
    let runai_entry = cx["mcp_servers"]["runai"]
        .as_table()
        .expect("[mcp_servers.runai] table missing");
    let codex_cmd = runai_entry["command"].as_str().unwrap();
    assert!(Path::new(codex_cmd).is_absolute());
    let args = runai_entry["args"].as_array().unwrap();
    assert_eq!(args[0].as_str().unwrap(), "mcp-serve");
    assert!(
        cx["mcp_servers"]["other"]["command"].is_str(),
        "preexisting codex entry must survive"
    );

    // 4) .config/opencode/opencode.json: mcp.runai.command = array
    let oj = read_json(&opencode_config(home));
    let oc_cmd = oj["mcp"]["runai"]["command"]
        .as_array()
        .expect("opencode mcp.runai.command must be an array");
    assert_eq!(oc_cmd.len(), 2);
    assert!(Path::new(oc_cmd[0].as_str().unwrap()).is_absolute());
    assert_eq!(oc_cmd[1].as_str().unwrap(), "mcp-serve");
    assert!(oj["mcp"]["other"]["command"].is_array());

    // Idempotency — second run reports `(already registered)` for each CLI.
    let out2 = runai_cmd(home).arg("register").output().unwrap();
    dump(&out2, "register pass 2");
    assert!(out2.status.success(), "second register must succeed");
    let stdout2 = String::from_utf8_lossy(&out2.stdout);
    assert!(
        stdout2.contains("already registered"),
        "idempotent register must say 'already registered': {stdout2:?}"
    );
}

#[test]
fn register_creates_missing_dirs() {
    let home_t = scratch_home();
    let home = home_t.path();

    // No .gemini / .codex / .config dirs at all.
    assert!(!home.join(".gemini").exists());
    assert!(!home.join(".codex").exists());
    assert!(!home.join(".config").exists());

    let out = runai_cmd(home).arg("register").output().unwrap();
    dump(&out, "register from empty home");
    assert!(out.status.success(), "register must succeed");

    // All 4 config files must now exist.
    assert!(
        claude_config(home).is_file(),
        "register must create .claude.json"
    );
    assert!(
        gemini_config(home).is_file(),
        "register must create .gemini/settings.json"
    );
    assert!(
        codex_config(home).is_file(),
        "register must create .codex/config.toml"
    );
    assert!(
        opencode_config(home).is_file(),
        "register must create .config/opencode/opencode.json"
    );
}

#[test]
fn register_symmetric_across_targets() {
    let home_t = scratch_home();
    let home = home_t.path();

    let out = runai_cmd(home).arg("register").output().unwrap();
    dump(&out, "register symmetry");
    assert!(out.status.success());

    let claude_cmd = read_json(&claude_config(home))["mcpServers"]["runai"]["command"]
        .as_str()
        .unwrap()
        .to_string();
    let gemini_cmd = read_json(&gemini_config(home))["mcpServers"]["runai"]["command"]
        .as_str()
        .unwrap()
        .to_string();
    let codex_cmd = read_toml(&codex_config(home))["mcp_servers"]["runai"]["command"]
        .as_str()
        .unwrap()
        .to_string();
    let opencode_arr = read_json(&opencode_config(home))["mcp"]["runai"]["command"]
        .as_array()
        .unwrap()
        .clone();
    let opencode_cmd = opencode_arr[0].as_str().unwrap().to_string();

    // All four point at the same binary path.
    assert_eq!(claude_cmd, gemini_cmd, "claude vs gemini cmd drift");
    assert_eq!(claude_cmd, codex_cmd, "claude vs codex cmd drift");
    assert_eq!(claude_cmd, opencode_cmd, "claude vs opencode cmd drift");

    // All four agree on the `mcp-serve` arg shape.
    assert_eq!(
        read_json(&claude_config(home))["mcpServers"]["runai"]["args"][0]
            .as_str()
            .unwrap(),
        "mcp-serve"
    );
    assert_eq!(
        read_json(&gemini_config(home))["mcpServers"]["runai"]["args"][0]
            .as_str()
            .unwrap(),
        "mcp-serve"
    );
    assert_eq!(
        read_toml(&codex_config(home))["mcp_servers"]["runai"]["args"][0]
            .as_str()
            .unwrap(),
        "mcp-serve"
    );
    assert_eq!(opencode_arr[1].as_str().unwrap(), "mcp-serve");
}

#[test]
fn register_ignores_rune_data_dir() {
    let home_t = scratch_home();
    let home = home_t.path();
    let alt_data = scratch_home();

    // First register with default data dir under HOME.
    let out1 = runai_cmd(home).arg("register").output().unwrap();
    dump(&out1, "register default data dir");
    assert!(out1.status.success());
    let cmd_default = read_json(&claude_config(home))["mcpServers"]["runai"]["command"]
        .as_str()
        .unwrap()
        .to_string();

    // Re-register with a different RUNE_DATA_DIR; binary path must be unchanged
    // and the CLI configs still live under HOME, not under alt_data.
    let out2 = Command::new(RUNAI_BIN)
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", alt_data.path())
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .arg("register")
        .output()
        .unwrap();
    dump(&out2, "register alt RUNE_DATA_DIR");
    assert!(out2.status.success());

    let cmd_alt = read_json(&claude_config(home))["mcpServers"]["runai"]["command"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        cmd_default, cmd_alt,
        "binary path must not depend on RUNE_DATA_DIR"
    );

    // alt data dir must NOT contain CLI config files (register is HOME-rooted).
    assert!(
        !alt_data.path().join(".claude.json").exists(),
        "register must not write CLI configs into RUNE_DATA_DIR"
    );
    assert!(!alt_data.path().join(".gemini/settings.json").exists());
    assert!(!alt_data.path().join(".codex/config.toml").exists());
    assert!(
        !alt_data
            .path()
            .join(".config/opencode/opencode.json")
            .exists()
    );
}

// ---------------------------------------------------------------------------
// 1.29 unregister
// ---------------------------------------------------------------------------

#[test]
fn unregister_removes_runai_from_all_configs() {
    let home_t = scratch_home();
    let home = home_t.path();

    // Pre-create 4 CLI configs containing BOTH runai and a foreign MCP.
    std::fs::create_dir_all(home.join(".gemini")).unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
    std::fs::write(
        claude_config(home),
        r#"{"mcpServers":{
            "runai":{"command":"/tmp/runai","args":["mcp-serve"]},
            "other":{"command":"x","args":[]}
        }}"#,
    )
    .unwrap();
    std::fs::write(
        gemini_config(home),
        r#"{"general":{"k":"v"},"mcpServers":{
            "runai":{"command":"/tmp/runai","args":["mcp-serve"]},
            "other":{"command":"y","args":[]}
        }}"#,
    )
    .unwrap();
    std::fs::write(
        codex_config(home),
        "[mcp_servers.runai]\ncommand = \"/tmp/runai\"\nargs = [\"mcp-serve\"]\n\n[mcp_servers.other]\ncommand = \"z\"\nargs = []\n",
    )
    .unwrap();
    std::fs::write(
        opencode_config(home),
        r#"{"mcp":{
            "runai":{"command":["/tmp/runai","mcp-serve"],"enabled":true,"type":"local"},
            "other":{"command":["q","arg"],"enabled":true,"type":"local"}
        }}"#,
    )
    .unwrap();

    let out = runai_cmd(home).arg("unregister").output().unwrap();
    dump(&out, "unregister");
    assert!(out.status.success(), "unregister must succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Unregistered"),
        "unregister stdout must contain 'Unregistered': {stdout:?}"
    );

    // claude: runai removed, other preserved.
    let cj = read_json(&claude_config(home));
    assert!(
        cj["mcpServers"].get("runai").is_none(),
        ".claude.json must drop mcpServers.runai"
    );
    assert!(
        cj["mcpServers"]["other"]["command"].is_string(),
        ".claude.json must preserve mcpServers.other"
    );

    // gemini
    let gj = read_json(&gemini_config(home));
    assert!(gj["mcpServers"].get("runai").is_none());
    assert!(gj["mcpServers"]["other"]["command"].is_string());
    assert_eq!(gj["general"]["k"], serde_json::json!("v"));

    // codex
    let cx = read_toml(&codex_config(home));
    let codex_servers = cx["mcp_servers"]
        .as_table()
        .expect("mcp_servers table missing");
    assert!(
        !codex_servers.contains_key("runai"),
        "[mcp_servers.runai] must be removed: {:?}",
        codex_servers.keys().collect::<Vec<_>>()
    );
    assert!(
        codex_servers.contains_key("other"),
        "[mcp_servers.other] must survive"
    );

    // opencode
    let oj = read_json(&opencode_config(home));
    assert!(oj["mcp"].get("runai").is_none());
    assert!(oj["mcp"]["other"]["command"].is_array());
}

#[test]
fn unregister_symmetric_across_targets() {
    let home_t = scratch_home();
    let home = home_t.path();

    // register first, then unregister, then assert all 4 are runai-free.
    let reg = runai_cmd(home).arg("register").output().unwrap();
    dump(&reg, "symmetry register");
    assert!(reg.status.success());

    let unreg = runai_cmd(home).arg("unregister").output().unwrap();
    dump(&unreg, "symmetry unregister");
    assert!(unreg.status.success());

    // All 4 configs still exist (we did NOT delete files, only entries).
    assert!(claude_config(home).is_file());
    assert!(gemini_config(home).is_file());
    assert!(codex_config(home).is_file());
    assert!(opencode_config(home).is_file());

    // None of the 4 contain a runai MCP entry.
    let cj = read_json(&claude_config(home));
    let gj = read_json(&gemini_config(home));
    let cx = read_toml(&codex_config(home));
    let oj = read_json(&opencode_config(home));

    let claude_has = cj.get("mcpServers").and_then(|s| s.get("runai")).is_some();
    let gemini_has = gj.get("mcpServers").and_then(|s| s.get("runai")).is_some();
    let codex_has = cx.get("mcp_servers").and_then(|s| s.get("runai")).is_some();
    let opencode_has = oj.get("mcp").and_then(|s| s.get("runai")).is_some();

    assert!(
        !claude_has,
        ".claude.json must be runai-free after unregister"
    );
    assert!(
        !gemini_has,
        ".gemini/settings.json must be runai-free after unregister"
    );
    assert!(
        !codex_has,
        ".codex/config.toml must be runai-free after unregister"
    );
    assert!(
        !opencode_has,
        ".config/opencode/opencode.json must be runai-free after unregister"
    );
}

#[test]
fn unregister_idempotent_when_no_configs() {
    let home_t = scratch_home();
    let home = home_t.path();

    // No CLI configs exist at all.
    assert!(!claude_config(home).exists());
    assert!(!gemini_config(home).exists());
    assert!(!codex_config(home).exists());
    assert!(!opencode_config(home).exists());

    let out = runai_cmd(home).arg("unregister").output().unwrap();
    dump(&out, "unregister on empty home");
    assert!(
        out.status.success(),
        "unregister must succeed even with no configs (idempotent)"
    );

    // Unregister must not have CREATED config files for missing CLIs.
    // (The unregister code path is read-and-skip when the file doesn't exist.)
    assert!(
        !claude_config(home).exists(),
        "unregister must not create .claude.json"
    );
    assert!(
        !gemini_config(home).exists(),
        "unregister must not create .gemini/settings.json"
    );
    assert!(
        !codex_config(home).exists(),
        "unregister must not create .codex/config.toml"
    );
    assert!(
        !opencode_config(home).exists(),
        "unregister must not create .config/opencode/opencode.json"
    );

    // Running it again is also fine.
    let out2 = runai_cmd(home).arg("unregister").output().unwrap();
    dump(&out2, "unregister idempotent second run");
    assert!(out2.status.success());
}

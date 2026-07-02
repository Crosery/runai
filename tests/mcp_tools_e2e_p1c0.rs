//! P1 e2e regression tests for `sm_list` and `sm_status` MCP tools.
//!
//! Each test spawns the workspace-built runai binary in an isolated HOME / RUNE_DATA_DIR,
//! drives `runai mcp-serve` over stdio with framed JSON-RPC, and asserts on the
//! tool result text. The binary is `env!("CARGO_BIN_EXE_runai")` — the binary
//! produced by `cargo build`/`cargo test` from this worktree's source.
//!
//! These tests never touch the real `~/.runai/` or `~/.{claude,codex,gemini,opencode}/`.
//! Safety contract: every spawn carries explicit `HOME`, `RUNAI_NO_AUTOSPAWN=1`,
//! and `RUNE_DATA_DIR=<HOME>/.runai`.

#![cfg(not(target_os = "windows"))]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

// ────────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────────

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

/// Build an isolated HOME with `~/.runai/skills/`, `~/.runai/mcps/`, and a
/// pre-created `~/.claude/skills/` symlink directory. Returns the TempDir
/// (must stay alive across the test) plus the data dir path.
struct Env {
    home: TempDir,
}

impl Env {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
                .expect("pre-create CLI skills dir");
        }
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        std::fs::create_dir_all(home.path().join(".runai/mcps")).expect("pre-create mcps dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn data_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }

    /// Drop a managed skill at `~/.runai/skills/<name>/SKILL.md`.
    fn seed_skill(&self, name: &str, description: &str) {
        let dir = self.data_dir().join("skills").join(name);
        std::fs::create_dir_all(&dir).expect("create skill dir");
        let skill_md =
            format!("---\nname: {name}\ndescription: {description}\n---\n{name} body.\n");
        std::fs::write(dir.join("SKILL.md"), skill_md).expect("write SKILL.md");
    }

    /// Run a runai CLI command (not MCP) and return the trimmed stdout.
    fn cli(&self, args: &[&str]) -> String {
        let out = Command::new(RUNAI_BIN)
            .args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.data_dir())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .expect("spawn runai CLI");
        assert!(
            out.status.success(),
            "runai {args:?} failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Spawn `runai mcp-serve` with the env locked to this sandbox.
    fn spawn_mcp(&self) -> Child {
        Command::new(RUNAI_BIN)
            .arg("mcp-serve")
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.data_dir())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn runai mcp-serve")
    }
}

/// Send an MCP `tools/call` to a freshly spawned mcp-serve and return the
/// inner `result` string from the tool's `TextResult`. Handles
/// initialize → notifications/initialized → tools/call sequence.
///
/// The MCP server may interleave responses (each request runs in its own
/// task), so we read up to `expected_responses` lines and pick the one with
/// matching `id`.
fn call_tool(env: &Env, tool: &str, arguments: serde_json::Value) -> String {
    let mut child = env.spawn_mcp();
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "p1c0-test", "version": "1.0"}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init).unwrap()).unwrap();
    stdin.flush().unwrap();

    // Drain initialize response
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let init_resp: serde_json::Value =
        serde_json::from_str(line.trim()).expect("init response is JSON");
    assert_eq!(init_resp["jsonrpc"], "2.0");

    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    writeln!(stdin, "{}", serde_json::to_string(&notif).unwrap()).unwrap();
    stdin.flush().unwrap();

    // Small delay so the notification is processed before the call
    std::thread::sleep(Duration::from_millis(150));

    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}
    });
    writeln!(stdin, "{}", serde_json::to_string(&call).unwrap()).unwrap();
    stdin.flush().unwrap();

    // Read response — may take some time if scan/dedupe is slow
    let mut line2 = String::new();
    reader.read_line(&mut line2).unwrap();
    let resp: serde_json::Value =
        serde_json::from_str(line2.trim()).expect("tool response is JSON");
    assert_eq!(resp["jsonrpc"], "2.0", "response not JSON-RPC");
    assert_eq!(resp["id"], 2, "response id mismatch: got {resp}");

    // Cleanup
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();

    // The structured `result.result` is the raw string returned by the tool.
    resp["result"]["structuredContent"]["result"]
        .as_str()
        .unwrap_or_else(|| panic!("missing structured result in {resp}"))
        .to_string()
}

// ────────────────────────────────────────────────────────────────────────────
// sm_list tests (test plan §3.1)
// ────────────────────────────────────────────────────────────────────────────

/// list_shows_correct_status_icons_and_counts — 2 enabled + 1 disabled skill;
/// sm_list must surface "● skill <name>" for enabled, "○ skill <name>" for
/// disabled, and the header reports total + enabled count.
#[test]
fn list_shows_correct_status_icons_and_counts() {
    let env = Env::new();
    env.seed_skill("enabled-one", "first");
    env.seed_skill("enabled-two", "second");
    env.seed_skill("only-disabled", "third");
    let scan_out = env.cli(&["scan"]);
    assert!(
        scan_out.contains("3 adopted"),
        "expected 3 adopted, got: {scan_out}"
    );
    env.cli(&["enable", "enabled-one", "--target", "claude"]);
    env.cli(&["enable", "enabled-two", "--target", "claude"]);

    let body = call_tool(&env, "sm_list", serde_json::json!({}));

    // Header line: count of resources + enabled count for default target (claude)
    assert!(
        body.contains("3 resources"),
        "missing '3 resources' header: {body}"
    );
    assert!(
        body.contains("2 enabled for claude"),
        "missing '2 enabled for claude' header: {body}"
    );

    // Enabled lines use ● icon; disabled lines use ○ icon.
    assert!(
        body.contains("● skill enabled-one"),
        "enabled-one missing ● marker: {body}"
    );
    assert!(
        body.contains("● skill enabled-two"),
        "enabled-two missing ● marker: {body}"
    );
    assert!(
        body.contains("○ skill only-disabled"),
        "only-disabled missing ○ marker: {body}"
    );
    // No spurious ● on the disabled one
    assert!(
        !body.contains("● skill only-disabled"),
        "disabled skill incorrectly marked enabled: {body}"
    );
}

/// list_kind_filter_excludes_other_types — with kind='skill', the list must
/// contain only skill entries; MCP entries (if any) must be absent.
///
/// We cannot easily seed an MCP in this sandbox (sm_install is GitHub-based;
/// MCP rows live in DB + on-disk backups). So this test exercises the kind
/// filter on a pure-skills pool: every row must be marked as `skill` and the
/// filter must not introduce any spurious `mcp` rows.
#[test]
fn list_kind_filter_excludes_other_types() {
    let env = Env::new();
    env.seed_skill("kind-a", "a");
    env.seed_skill("kind-b", "b");
    env.cli(&["scan"]);

    let body = call_tool(&env, "sm_list", serde_json::json!({"kind": "skill"}));

    // Every body line that mentions a resource must be marked `skill`, never `mcp`.
    let resource_lines: Vec<&str> = body
        .lines()
        .filter(|l| l.starts_with("● ") || l.starts_with("○ "))
        .collect();
    assert_eq!(
        resource_lines.len(),
        2,
        "expected 2 skill rows, got: {body}"
    );
    for line in &resource_lines {
        assert!(
            line.contains("skill"),
            "row not marked skill: {line} (full body: {body})"
        );
        assert!(
            !line.contains("mcp"),
            "row marked mcp despite kind=skill filter: {line}"
        );
    }
    assert!(
        body.contains("2 resources"),
        "header missing/incorrect: {body}"
    );
}

/// list_group_filter_isolates_members — sm_list(group=<id>) returns exactly
/// the group's members, never other resources in the pool.
#[test]
fn list_group_filter_isolates_members() {
    let env = Env::new();
    env.seed_skill("member-one", "in-group");
    env.seed_skill("member-two", "in-group");
    env.seed_skill("non-member", "out-of-group");
    env.cli(&["scan"]);
    env.cli(&["group", "create", "test-group", "--name", "TestGroup"]);
    env.cli(&["group", "add", "test-group", "member-one"]);
    env.cli(&["group", "add", "test-group", "member-two"]);

    let body = call_tool(&env, "sm_list", serde_json::json!({"group": "test-group"}));

    // Header reports 2 resources (the two group members).
    assert!(
        body.contains("2 resources"),
        "expected 2 resources in group, got: {body}"
    );
    // Both members present.
    assert!(
        body.contains("member-one"),
        "member-one missing from group filter result: {body}"
    );
    assert!(
        body.contains("member-two"),
        "member-two missing from group filter result: {body}"
    );
    // Non-member must not leak in.
    assert!(
        !body.contains("non-member"),
        "non-member leaked into group filter result: {body}"
    );
}

// ────────────────────────────────────────────────────────────────────────────
// sm_status tests (test plan §3.2)
// ────────────────────────────────────────────────────────────────────────────

/// status_json_shape_complete — sm_status returns valid JSON with all
/// required fields: target, skills_enabled, skills_total, mcps_enabled,
/// mcps_total, runai_version.
#[test]
fn status_json_shape_complete() {
    let env = Env::new();
    env.seed_skill("stat-skill", "x");
    env.cli(&["scan"]);

    let body = call_tool(&env, "sm_status", serde_json::json!({}));

    let parsed: serde_json::Value =
        serde_json::from_str(&body).expect("sm_status body must be valid JSON");

    assert_eq!(
        parsed["target"], "claude",
        "default target should be claude"
    );
    assert!(
        parsed["skills_enabled"].is_i64() || parsed["skills_enabled"].is_u64(),
        "skills_enabled missing/non-int: {parsed}"
    );
    assert!(
        parsed["skills_total"].is_i64() || parsed["skills_total"].is_u64(),
        "skills_total missing/non-int: {parsed}"
    );
    assert!(
        parsed["mcps_enabled"].is_i64() || parsed["mcps_enabled"].is_u64(),
        "mcps_enabled missing/non-int: {parsed}"
    );
    assert!(
        parsed["mcps_total"].is_i64() || parsed["mcps_total"].is_u64(),
        "mcps_total missing/non-int: {parsed}"
    );
    let ver = parsed["runai_version"]
        .as_str()
        .expect("runai_version must be a string");
    // Looks like a semver-ish version (digits.digits.digits[-tail])
    assert!(
        ver.chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false),
        "runai_version doesn't look like a version: {ver}"
    );

    // skills_total should reflect the seeded skill (1)
    assert_eq!(
        parsed["skills_total"].as_u64().unwrap_or(0),
        1,
        "skills_total mismatch (expected 1 after seeding): {parsed}"
    );
}

/// status_target_param_respected — sm_status with target='codex' returns
/// codex in the target field.
#[test]
fn status_target_param_respected() {
    let env = Env::new();
    env.seed_skill("st-target-codex", "x");
    env.cli(&["scan"]);

    let body = call_tool(&env, "sm_status", serde_json::json!({"target": "codex"}));
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("body is JSON");
    assert_eq!(
        parsed["target"], "codex",
        "target should be codex: {parsed}"
    );

    // Also test gemini for full target-passthrough coverage
    let body2 = call_tool(&env, "sm_status", serde_json::json!({"target": "gemini"}));
    let parsed2: serde_json::Value = serde_json::from_str(&body2).expect("body2 is JSON");
    assert_eq!(
        parsed2["target"], "gemini",
        "target should be gemini: {parsed2}"
    );
}

/// status_surfaces_pending_updates — when `~/.runai/update-check.json` reports
/// a newer version, sm_status sets update_available=true and includes
/// update_latest + update_hint.
#[test]
fn status_surfaces_pending_updates() {
    let env = Env::new();
    // Seed a pending-update cache file at data_dir/update-check.json
    let cache = serde_json::json!({
        "latest_version": "99.99.99",
        "current_version": "0.0.1",
        "download_url": "https://example.invalid/dl",
        "checksum_url": "https://example.invalid/cs",
        "checked_at": "2026-06-13T00:00:00Z"
    });
    std::fs::write(
        env.data_dir().join("update-check.json"),
        serde_json::to_string_pretty(&cache).unwrap(),
    )
    .expect("write update-check.json");

    let body = call_tool(&env, "sm_status", serde_json::json!({}));
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("body is JSON");

    assert_eq!(
        parsed["update_available"], true,
        "update_available should be true when cache has newer version: {parsed}"
    );
    assert_eq!(
        parsed["update_latest"], "99.99.99",
        "update_latest should match cache: {parsed}"
    );
    let hint = parsed["update_hint"]
        .as_str()
        .expect("update_hint should be present");
    assert!(
        hint.to_lowercase().contains("runai update") || hint.to_lowercase().contains("newer"),
        "update_hint should mention runai update or newer version: {hint}"
    );
}

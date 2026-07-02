//! P2 integration coverage for the install / market MCP tools:
//! `sm_install`, `sm_install_github`, `sm_market`, `sm_market_install`.
//!
//! Each test spawns the real `runai mcp-serve` binary, sandboxes HOME so it
//! never touches the developer's `~/.runai/`, sends an `initialize` plus a
//! tool call, and asserts on the parsed JSON-RPC response.
//!
//! Per the safety contract (AGENTS.md), these tests honor:
//! - `HOME=$(mktemp -d)` so all data-dir reads/writes are sandboxed
//! - `RUNE_DATA_DIR=$HOME/.runai` so the binary's data dir resolution stays in sandbox
//! - `RUNAI_NO_AUTOSPAWN=1` so the TUI dashboard server never auto-spawns
//!
//! Skipped on Windows: `dirs::home_dir()` ignores the HOME override there,
//! so the spawned `mcp-serve` binary would touch the runner's real profile
//! instead of the sandbox — same HOME-mocking-is-unix-only class as the
//! other physical-e2e suites.
#![cfg(not(target_os = "windows"))]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const BINARY_PATH: &str = env!("CARGO_BIN_EXE_runai");

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl McpSession {
    fn new(scratch: &Path) -> Self {
        assert!(
            Path::new(BINARY_PATH).exists(),
            "runai binary not found at {BINARY_PATH}"
        );
        let data_dir = scratch.join(".runai");
        let mut child = Command::new(BINARY_PATH)
            .arg("mcp-serve")
            .env("HOME", scratch)
            .env("RUNE_DATA_DIR", &data_dir)
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn runai mcp-serve");
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout);

        // initialize
        let init = json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "p2c0-test", "version": "1.0"}
            }
        });
        writeln!(stdin, "{}", serde_json::to_string(&init).unwrap()).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("failed to read initialize response");
        let v: Value = serde_json::from_str(line.trim()).expect("init response is not JSON");
        assert_eq!(v["jsonrpc"], "2.0");
        assert!(
            v["result"]["serverInfo"].is_object(),
            "no serverInfo in initialize: {v}"
        );

        // initialized notification
        let notif = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        writeln!(stdin, "{}", serde_json::to_string(&notif).unwrap()).unwrap();
        stdin.flush().unwrap();
        std::thread::sleep(Duration::from_millis(150));

        McpSession {
            child,
            stdin,
            reader,
        }
    }

    /// Call a single MCP tool and return the inner `result` string from
    /// the `structuredContent` field of the tool response.
    fn call_tool(&mut self, id: i64, name: &str, args: Value) -> String {
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": args}
        });
        writeln!(self.stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        self.stdin.flush().unwrap();

        // Read response lines until we see one matching our id.
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if Instant::now() > deadline {
                panic!("timeout waiting for response id={id} (tool={name})");
            }
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .expect("read_line failed waiting for tool response");
            if n == 0 {
                panic!("EOF on MCP stdout before id={id} (tool={name})");
            }
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue, // skip stray log lines if any
            };
            if v["id"] == id {
                if v.get("error").is_some() {
                    panic!("MCP error for {name}: {v}");
                }
                // The tool returns a Json<TextResult> via rmcp, so the
                // structured payload exposes a single "result" string.
                let inner = &v["result"]["structuredContent"]["result"];
                let s = inner
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("expected structuredContent.result to be a string, got: {v}")
                    })
                    .to_string();
                return s;
            }
            // not our id — keep reading
        }
    }
}

impl Drop for McpSession {
    fn drop(&mut self) {
        // We don't have access to stdin after `call_tool` borrows it through &mut self,
        // so just kill the process to terminate the MCP loop.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn scratch() -> TempDir {
    tempfile::tempdir().expect("failed to create scratch tempdir")
}

// ─────────────────────────────────────────────────────────────────────
// sm_install (4 tests)
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sm_install_valid_repo_returns_command_string() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_install", json!({"repo": "anthropic/skills"}));
    assert!(
        out.contains("Run this command via Bash tool:"),
        "missing instruction header: {out}"
    );
    assert!(
        out.contains("rune install anthropic/skills"),
        "missing command: {out}"
    );
    // sm_install is an INSTRUCTION endpoint — it must not return the
    // output of an executed install (no "Installed" / "Downloaded" prose
    // suggesting it ran the command itself).
    assert!(
        !out.contains("Installed ") && !out.contains("Downloaded "),
        "sm_install must not execute the install: {out}"
    );
}

#[test]
fn sm_install_url_normalization() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(
        1,
        "sm_install",
        json!({"repo": "https://github.com/anthropic/skills/"}),
    );
    // URL prefix stripped, trailing slash removed, owner/repo normalized.
    assert!(
        out.contains("rune install anthropic/skills"),
        "URL should normalize to owner/repo: {out}"
    );
    assert!(
        !out.contains("https://github.com/"),
        "URL prefix should be stripped: {out}"
    );
    assert!(
        !out.contains("anthropic/skills/"),
        "trailing slash should be stripped: {out}"
    );
}

#[test]
fn sm_install_rejects_unsafe_shell_args() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_install", json!({"repo": "test; rm -rf /"}));
    assert!(
        out.contains("Invalid repo format"),
        "unsafe shell arg should be rejected: {out}"
    );
    // Must not return any rune install command for the unsafe payload.
    assert!(
        !out.contains("rune install test;") && !out.contains("rune install test"),
        "no executable command should be emitted for unsafe input: {out}"
    );
}

#[test]
fn sm_install_allows_at_symbol_for_branch() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_install", json!({"repo": "owner/repo@main"}));
    // @ must be in the safe-arg allowlist; command should be generated.
    assert!(
        out.contains("rune install owner/repo@main"),
        "@branch syntax should be accepted: {out}"
    );
    assert!(
        !out.contains("Invalid repo format"),
        "owner/repo@branch should not be rejected: {out}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// sm_market_install (5 tests)
// ─────────────────────────────────────────────────────────────────────

#[test]
fn sm_market_install_single_skill_returns_command() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_market_install", json!({"name": "my-skill"}));
    assert!(
        out.contains("Run this command via Bash tool:"),
        "missing instruction header: {out}"
    );
    assert!(
        out.contains("runai market-install my-skill"),
        "missing market-install command: {out}"
    );
    // Single skill must not get the multi-skill "Then run: runai scan" tail.
    assert!(
        !out.contains("Then run: runai scan"),
        "single-skill output should not include batch scan tail: {out}"
    );
    // And should not list multiple commands.
    let cmd_count = out.matches("runai market-install ").count();
    assert_eq!(
        cmd_count, 1,
        "single-skill output should contain exactly one command line: {out}"
    );
}

#[test]
fn sm_market_install_multiple_skills_returns_batch_commands() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(
        1,
        "sm_market_install",
        json!({"names": ["skill1", "skill2"]}),
    );
    assert!(
        out.contains("runai market-install skill1"),
        "missing first skill command: {out}"
    );
    assert!(
        out.contains("runai market-install skill2"),
        "missing second skill command: {out}"
    );
    assert!(
        out.contains("one by one or with &&"),
        "missing batch guidance: {out}"
    );
    assert!(
        out.contains("Then run: runai scan"),
        "missing scan tail: {out}"
    );
}

#[test]
fn sm_market_install_with_source_parameter() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(
        1,
        "sm_market_install",
        json!({"name": "skill1", "source": "anthropic/skills"}),
    );
    // Source must be single-quoted in the emitted shell command.
    assert!(
        out.contains("runai market-install skill1 --source 'anthropic/skills'"),
        "source argument missing or not single-quoted: {out}"
    );
}

#[test]
fn sm_market_install_rejects_unsafe_args() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_market_install", json!({"name": "skill; rm -rf /"}));
    assert!(
        out.contains("Invalid name"),
        "unsafe name not rejected: {out}"
    );
    assert!(
        !out.contains("runai market-install skill;"),
        "no command should be emitted for unsafe input: {out}"
    );
}

#[test]
fn sm_market_install_allows_slash_and_dash() {
    let s = scratch();
    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_market_install", json!({"name": "org/skill-name"}));
    assert!(
        out.contains("runai market-install org/skill-name"),
        "/ and - should be allowed in name: {out}"
    );
    assert!(
        !out.contains("Invalid name"),
        "org/skill-name should not be rejected: {out}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// sm_market (5 tests)
//
// These tests prime the market by writing a controlled `market-sources.json`
// (a single user-added source so we don't depend on the built-in list) and
// a matching `market-cache/<owner>_<repo>.json` payload. That keeps the
// tests hermetic — they never touch the network and never assume how many
// built-in sources ship with the binary.
// ─────────────────────────────────────────────────────────────────────

fn seed_market(data_dir: &Path) {
    std::fs::create_dir_all(data_dir).unwrap();

    // Single non-builtin source so we control the entire candidate set.
    // Field shape mirrors `SourceEntry` in src/core/market.rs.
    let sources = json!([
        {
            "owner": "testorg",
            "repo": "testskills",
            "branch": "main",
            "skill_prefix": "",
            "label": "testorg/testskills",
            "description": "test source",
            "builtin": false,
            "enabled": true
        }
    ]);
    std::fs::write(
        data_dir.join("market-sources.json"),
        serde_json::to_string_pretty(&sources).unwrap(),
    )
    .unwrap();

    // Cache file naming: "<owner>_<repo>.json" inside market-cache/.
    let cache_dir = data_dir.join("market-cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let cache = json!([
        {
            "name": "video-player",
            "repo_path": "skills/video-player",
            "source_label": "testorg/testskills",
            "source_repo": "testorg/testskills",
            "branch": "main"
        },
        {
            "name": "file-manager",
            "repo_path": "skills/file-manager",
            "source_label": "testorg/testskills",
            "source_repo": "testorg/testskills",
            "branch": "main"
        }
    ]);
    std::fs::write(
        cache_dir.join("testorg_testskills.json"),
        serde_json::to_string(&cache).unwrap(),
    )
    .unwrap();
}

/// Disable the default built-in sources so they don't pollute the candidate
/// set with their fetched (or empty) caches. The user-added source seeded by
/// `seed_market` stays enabled.
fn seed_market_disabling_builtins(data_dir: &Path) {
    std::fs::create_dir_all(data_dir).unwrap();
    // List enough built-in sources to disable them; load_sources merges by
    // (builtin, repo_id). Anything not listed retains its default enabled
    // state, so we list the ones load_sources is documented to ship.
    let mut sources: Vec<Value> = vec![json!({
        "owner": "testorg",
        "repo": "testskills",
        "branch": "main",
        "skill_prefix": "",
        "label": "testorg/testskills",
        "description": "test source",
        "builtin": false,
        "enabled": true
    })];
    for (owner, repo) in [
        ("anthropics", "claude-plugins-official"),
        ("affaan-m", "everything-claude-code"),
        ("TerminalSkills", "skills"),
        ("vercel-labs", "agent-skills"),
        ("anthropics", "skills"),
        ("ComposioHQ", "awesome-claude-skills"),
    ] {
        sources.push(json!({
            "owner": owner,
            "repo": repo,
            "branch": "main",
            "skill_prefix": "",
            "label": format!("{owner}/{repo}"),
            "description": "",
            "builtin": true,
            "enabled": false
        }));
    }
    std::fs::write(
        data_dir.join("market-sources.json"),
        serde_json::to_string_pretty(&sources).unwrap(),
    )
    .unwrap();

    let cache_dir = data_dir.join("market-cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let cache = json!([
        {
            "name": "video-player",
            "repo_path": "skills/video-player",
            "source_label": "testorg/testskills",
            "source_repo": "testorg/testskills",
            "branch": "main"
        },
        {
            "name": "file-manager",
            "repo_path": "skills/file-manager",
            "source_label": "testorg/testskills",
            "source_repo": "testorg/testskills",
            "branch": "main"
        }
    ]);
    std::fs::write(
        cache_dir.join("testorg_testskills.json"),
        serde_json::to_string(&cache).unwrap(),
    )
    .unwrap();

    // suppress "let _ = seed_market" dead-code path
    let _ = seed_market;
}

#[test]
fn sm_market_list_all_returns_valid_json() {
    let s = scratch();
    let data_dir = s.path().join(".runai");
    seed_market_disabling_builtins(&data_dir);

    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_market", json!({}));
    let parsed: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("sm_market did not return JSON ({e}): {out}"));
    let arr = parsed
        .as_array()
        .unwrap_or_else(|| panic!("sm_market output is not an array: {out}"));
    assert!(!arr.is_empty(), "expected seeded skills, got empty: {out}");
    for entry in arr {
        assert!(entry.get("name").is_some(), "missing 'name' in {entry}");
        assert!(entry.get("source").is_some(), "missing 'source' in {entry}");
        assert!(
            entry.get("installed").is_some(),
            "missing 'installed' in {entry}"
        );
        assert!(entry["name"].is_string(), "name not string in {entry}");
        assert!(entry["source"].is_string(), "source not string in {entry}");
        assert!(
            entry["installed"].is_boolean(),
            "installed not bool in {entry}"
        );
    }
    // Both seeded skills should appear.
    let names: Vec<&str> = arr.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(
        names.contains(&"video-player"),
        "missing video-player: {names:?}"
    );
    assert!(
        names.contains(&"file-manager"),
        "missing file-manager: {names:?}"
    );
}

#[test]
fn sm_market_search_keyword_fuzzy_match() {
    let s = scratch();
    let data_dir = s.path().join(".runai");
    seed_market_disabling_builtins(&data_dir);

    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_market", json!({"search": "video"}));
    let parsed: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("sm_market did not return JSON ({e}): {out}"));
    let arr = parsed.as_array().unwrap_or_else(|| {
        panic!("sm_market search output is not an array (no-results message?): {out}")
    });
    let names: Vec<&str> = arr.iter().filter_map(|e| e["name"].as_str()).collect();
    assert!(
        names.contains(&"video-player"),
        "fuzzy 'video' should match video-player; got {names:?}"
    );
    assert!(
        !names.contains(&"file-manager"),
        "file-manager should not match 'video'; got {names:?}"
    );
}

#[test]
fn sm_market_filter_by_source() {
    let s = scratch();
    let data_dir = s.path().join(".runai");
    seed_market_disabling_builtins(&data_dir);

    let mut mcp = McpSession::new(s.path());
    // Source filter is substring-matched against label and repo_id.
    let out = mcp.call_tool(1, "sm_market", json!({"source": "testorg"}));
    let parsed: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("sm_market did not return JSON ({e}): {out}"));
    let arr = parsed
        .as_array()
        .unwrap_or_else(|| panic!("sm_market source-filter output is not an array: {out}"));
    assert!(
        !arr.is_empty(),
        "filter by 'testorg' should yield results: {out}"
    );
    for entry in arr {
        let source = entry["source"].as_str().unwrap_or("");
        assert!(
            source.to_lowercase().contains("testorg"),
            "source filter did not narrow output: entry {entry}"
        );
    }
}

#[test]
fn sm_market_marks_installed_skills() {
    let s = scratch();
    let data_dir = s.path().join(".runai");
    // Pre-create a skill dir + SKILL.md so that `runai scan` adopts it into
    // the DB. Name matches one of the seeded market entries so the market
    // listing should mark it as installed.
    let skill_dir = data_dir.join("skills").join("video-player");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: video-player\ndescription: Local fake for installed-mark test\n---\nbody\n",
    )
    .unwrap();
    seed_market_disabling_builtins(&data_dir);

    // Run scan to adopt the skill into the DB.
    let scan = Command::new(BINARY_PATH)
        .arg("scan")
        .env("HOME", s.path())
        .env("RUNE_DATA_DIR", &data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .output()
        .expect("failed to run runai scan");
    assert!(
        scan.status.success(),
        "runai scan failed: {}",
        String::from_utf8_lossy(&scan.stderr)
    );

    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_market", json!({}));
    let parsed: Value = serde_json::from_str(&out)
        .unwrap_or_else(|e| panic!("sm_market did not return JSON ({e}): {out}"));
    let arr = parsed.as_array().expect("not array");
    let video = arr
        .iter()
        .find(|e| e["name"] == "video-player")
        .unwrap_or_else(|| panic!("video-player missing from market output: {out}"));
    assert_eq!(
        video["installed"], true,
        "video-player should be marked installed after scan adopted it: {video}"
    );
    let file_mgr = arr
        .iter()
        .find(|e| e["name"] == "file-manager")
        .unwrap_or_else(|| panic!("file-manager missing from market output: {out}"));
    assert_eq!(
        file_mgr["installed"], false,
        "file-manager should not be marked installed: {file_mgr}"
    );
}

#[test]
fn sm_market_no_results_helpful_message() {
    let s = scratch();
    let data_dir = s.path().join(".runai");
    seed_market_disabling_builtins(&data_dir);

    let mut mcp = McpSession::new(s.path());
    let out = mcp.call_tool(1, "sm_market", json!({"search": "xyznonexistent99999"}));
    // The response is a human-readable message, not JSON.
    assert!(
        out.contains("No skills matching"),
        "missing 'No skills matching' message: {out}"
    );
    assert!(
        out.contains("TUI Market tab"),
        "no-results message should point users to TUI Market tab: {out}"
    );
    // Must not return an empty array — that would mislead callers.
    assert!(
        out.trim() != "[]",
        "no-results path must not return empty array: {out}"
    );
}

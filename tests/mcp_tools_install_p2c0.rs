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

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;

const BINARY_PATH: &str = "/Users/crosery/.cargo/bin/runai";

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
                    .unwrap_or_else(|| panic!(
                        "expected structuredContent.result to be a string, got: {v}"
                    ))
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
    let out = mcp.call_tool(
        1,
        "sm_market_install",
        json!({"name": "skill; rm -rf /"}),
    );
    assert!(out.contains("Invalid name"), "unsafe name not rejected: {out}");
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

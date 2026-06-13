//! P1 e2e regression for `runai mcp-serve` (PLANNING §1.26).
//!
//! Verifies the stdio MCP server:
//!   1. `mcp_serve_initializes_and_waits` — accepts the JSON-RPC `initialize`
//!      handshake on stdin, returns a valid `serverInfo` response, then keeps
//!      the loop alive long enough to answer a follow-up `tools/list` (the
//!      driver must NOT exit after init).
//!   2. `mcp_serve_handles_tool_calls` — `tools/call` to `sm_list` returns a
//!      valid JSON-RPC result with the canonical "N resources (..." prefix
//!      that the SmServer renders. We do NOT pre-populate skills (that would
//!      need a populated DB + symlinks); a fresh HOME yields "0 resources"
//!      which is still a valid SmServer reply — what we are gating is the
//!      router wiring, not the count.
//!
//! Safety contract: every test runs in a fresh `tempfile::TempDir` with
//! `HOME` / `RUNE_DATA_DIR` / `RUNAI_NO_AUTOSPAWN` injected so the binary
//! NEVER touches the real `~/.runai/` or any of the 4 CLI config files.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

/// Spawn `runai mcp-serve` in a sandbox HOME. Returns the child plus a
/// retained TempDir so the caller controls cleanup ordering.
fn spawn_mcp_serve(scratch: &tempfile::TempDir) -> std::process::Child {
    let data_dir = scratch.path().join(".runai");
    Command::new(RUNAI_BIN)
        .arg("mcp-serve")
        .env("HOME", scratch.path())
        .env("RUNE_DATA_DIR", &data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn runai mcp-serve")
}

fn send_line<W: Write>(w: &mut W, v: &serde_json::Value) {
    writeln!(w, "{}", serde_json::to_string(v).unwrap()).unwrap();
    w.flush().unwrap();
}

fn read_jsonrpc_line<R: BufRead>(r: &mut R) -> serde_json::Value {
    let mut line = String::new();
    r.read_line(&mut line).expect("read stdout line");
    serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("stdout line is not valid JSON-RPC: {e}\nGot: {line:?}"))
}

#[test]
fn mcp_serve_initializes_and_waits() {
    let scratch = tempfile::tempdir().unwrap();
    let mut child = spawn_mcp_serve(&scratch);

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // Step 1: initialize handshake.
    let init_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "p1c0-init-test", "version": "1.0"}
        }
    });
    send_line(&mut stdin, &init_msg);

    let init_resp = read_jsonrpc_line(&mut reader);
    assert_eq!(
        init_resp["jsonrpc"], "2.0",
        "init response not JSON-RPC 2.0"
    );
    assert_eq!(init_resp["id"], 1, "init response id mismatch");
    assert!(
        init_resp["result"]["serverInfo"].is_object(),
        "init response missing serverInfo: {init_resp}"
    );
    assert!(
        init_resp["result"]["protocolVersion"].is_string(),
        "init response missing protocolVersion: {init_resp}"
    );

    // notifications/initialized — required by MCP handshake spec.
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    send_line(&mut stdin, &notif);
    std::thread::sleep(Duration::from_millis(300));

    // Step 2: server should STILL be alive — drive tools/list to prove it.
    let list_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    send_line(&mut stdin, &list_req);

    let list_resp = read_jsonrpc_line(&mut reader);
    assert_eq!(list_resp["jsonrpc"], "2.0");
    assert_eq!(list_resp["id"], 2);
    let tools = list_resp["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list missing tools array: {list_resp}"));
    assert!(
        tools.len() > 10,
        "expected >10 sm_* tools (SmServer publishes ~21), got {}",
        tools.len()
    );

    // SmServer must publish sm_list — the contract test for §1.26 #2 leans on it.
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(
        names.iter().any(|n| *n == "sm_list"),
        "tools/list missing 'sm_list'; got: {names:?}"
    );
    assert!(
        names.iter().any(|n| *n == "sm_status"),
        "tools/list missing 'sm_status'; got: {names:?}"
    );

    // Process must NOT have exited — the stdio loop is blocking on stdin.
    assert!(
        child.try_wait().unwrap().is_none(),
        "mcp-serve exited after init+tools/list; expected to stay in waiting() loop"
    );

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn mcp_serve_handles_tool_calls() {
    let scratch = tempfile::tempdir().unwrap();
    let mut child = spawn_mcp_serve(&scratch);

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // Init handshake.
    send_line(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "p1c0-call-test", "version": "1.0"}
            }
        }),
    );
    let init_resp = read_jsonrpc_line(&mut reader);
    assert_eq!(init_resp["jsonrpc"], "2.0");

    send_line(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );
    std::thread::sleep(Duration::from_millis(300));

    // tools/call sm_list — no skills registered, but the router still answers
    // with a valid JSON-RPC result containing the canonical "N resources" body.
    send_line(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": 42,
            "method": "tools/call",
            "params": {
                "name": "sm_list",
                "arguments": {}
            }
        }),
    );

    let call_resp = read_jsonrpc_line(&mut reader);
    assert_eq!(call_resp["jsonrpc"], "2.0");
    assert_eq!(call_resp["id"], 42, "tool call id mismatch: {call_resp}");
    assert!(
        call_resp["error"].is_null(),
        "tool call returned error: {call_resp}"
    );

    // Body — rmcp surfaces tool output under result.content[0].text. The
    // sm_list renderer always emits a "<N> resources (..." header line.
    let content = call_resp["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("tool call missing result.content: {call_resp}"));
    assert!(
        !content.is_empty(),
        "tool call returned empty content: {call_resp}"
    );

    // Each content block is { type: "text", text: "..." } per MCP spec.
    let text = content
        .iter()
        .find_map(|c| c["text"].as_str())
        .unwrap_or_else(|| panic!("tool call content has no text field: {call_resp}"));
    assert!(
        text.contains("resources ("),
        "sm_list reply does not look like the SmServer rendering. \
         Expected '<N> resources (... enabled for ...)' header, got: {text:?}"
    );

    // Process still alive after tool dispatch.
    assert!(
        child.try_wait().unwrap().is_none(),
        "mcp-serve exited after tool call; loop should stay alive"
    );

    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

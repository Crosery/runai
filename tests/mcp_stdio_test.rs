//! Integration test: MCP stdio transport must only write valid JSON-RPC to stdout.
//! Tracing/log output on stdout corrupts the protocol and breaks Codex CLI.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn runai_binary() -> String {
    // Use cargo-built binary. EXE_SUFFIX is "" on unix, ".exe" on Windows.
    let bin_name = format!("runai{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(&bin_name);
    if path.exists() {
        return path.to_string_lossy().to_string();
    }
    // Fallback to installed
    "/home/crosery/.local/bin/runai".to_string()
}

#[test]
fn mcp_stdout_only_contains_valid_jsonrpc() {
    let binary = runai_binary();
    let temp_home = tempfile::tempdir().unwrap();

    let mut child = Command::new(&binary)
        .arg("mcp-serve")
        .env("HOME", temp_home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn runai mcp-serve");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // Send initialize request
    let init_msg = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init_msg).unwrap()).unwrap();
    stdin.flush().unwrap();

    // Read initialize response — must be valid JSON
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let init_resp: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("stdout line 1 is not valid JSON: {e}\nGot: {line}"));
    assert_eq!(init_resp["jsonrpc"], "2.0");
    assert!(init_resp["result"]["serverInfo"].is_object());

    // Send initialized notification (triggers tracing log in rmcp)
    let notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    writeln!(stdin, "{}", serde_json::to_string(&notif).unwrap()).unwrap();
    stdin.flush().unwrap();

    // Small delay for notification processing
    std::thread::sleep(Duration::from_millis(300));

    // Send tools/list request
    let tools_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    writeln!(stdin, "{}", serde_json::to_string(&tools_req).unwrap()).unwrap();
    stdin.flush().unwrap();

    // Read tools/list response — must be valid JSON, not a tracing log line
    let mut line2 = String::new();
    reader.read_line(&mut line2).unwrap();
    let tools_resp: serde_json::Value = serde_json::from_str(line2.trim())
        .unwrap_or_else(|e| {
            panic!(
                "stdout line 2 is not valid JSON (likely tracing log leaked to stdout): {e}\nGot: {line2}"
            )
        });
    assert_eq!(tools_resp["jsonrpc"], "2.0");
    assert!(
        tools_resp["result"]["tools"].is_array(),
        "expected tools array, got: {tools_resp}"
    );

    // Verify tools are present
    let tools = tools_resp["result"]["tools"].as_array().unwrap();
    assert!(tools.len() > 10, "expected >10 tools, got {}", tools.len());

    // Clean up
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
}

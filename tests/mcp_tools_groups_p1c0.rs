//! Physical e2e for `sm_groups` (PLANNING test-plan §3.10 P1/P2).
//!
//! `sm_groups` is the MCP tool that lists custom groups with member counts and
//! a 200-char description preview. Behavior covered here:
//!
//!  1. `groups_empty_message` — empty state returns the canonical
//!     "No groups. Use sm_create_group to create one." string.
//!  2. `groups_lists_with_metadata` — header "N groups:" + per-group
//!     "id (name) — N members" + truncated description preview at 200 chars
//!     with `…` ellipsis when over.
//!  3. `groups_no_empty_preview` — groups whose description is empty do NOT
//!     emit a 6-space-indented blank preview line.
//!  4. `groups_member_count_accurate` — member count column reflects DB state
//!     (add/remove members surface in subsequent `sm_groups` calls).
//!
//! We drive group state through the real `runai` binary CLI (`runai group
//! create / add / remove`) — the same `SkillManager` code path the MCP layer
//! uses. We then invoke `sm_groups` via `runai mcp-serve` over stdio so we
//! assert on the EXACT MCP-tool output (the CLI's `runai group list` uses a
//! different format).
//!
//! Safety contract: each test runs in an isolated `HOME=$(mktemp -d)` with
//! `RUNE_DATA_DIR=$HOME/.runai` and `RUNAI_NO_AUTOSPAWN=1`. We never touch the
//! real `~/.runai/`.
//!
//! Skipped on Windows: HOME mocking + symlinks unix-only (consistent with the
//! existing `safety_e2e` / `mcp_tools_trash` suites).
#![cfg(not(target_os = "windows"))]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_runai"))
}

/// Run runai with isolated HOME + RUNE_DATA_DIR=$HOME/.runai.
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

fn fresh_home() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(tmp.path().join(format!(".{cli}/skills")))
            .expect("pre-create CLI skills dir");
    }
    std::fs::create_dir_all(tmp.path().join(".runai/skills"))
        .expect("pre-create managed skills dir");
    std::fs::create_dir_all(tmp.path().join(".runai/groups"))
        .expect("pre-create managed groups dir");
    tmp
}

fn make_skill(skills_dir: &Path, name: &str, body: &str) -> PathBuf {
    let dir = skills_dir.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
    )
    .unwrap();
    dir
}

fn dump(out: &(String, String, std::process::ExitStatus), label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.2, out.0, out.1
    );
}

/// Spawn `runai mcp-serve` over stdio in an isolated HOME, send tools/call,
/// return the response value. We use this for the `sm_groups` invocation
/// itself because the CLI's `runai group list` uses a different output format
/// than the MCP tool — the plan asserts on the MCP-tool format.
fn mcp_call(
    home: &Path,
    tool: &str,
    arguments: serde_json::Value,
) -> (serde_json::Value, std::process::Child) {
    let data_dir = home.join(".runai");
    let mut child = Command::new(runai_binary())
        .arg("mcp-serve")
        .env("HOME", home)
        .env("RUNE_DATA_DIR", &data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn runai mcp-serve");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // initialize
    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "1.0"}
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&init).unwrap()).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();

    // initialized notification
    let notif = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    writeln!(stdin, "{}", serde_json::to_string(&notif).unwrap()).unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(Duration::from_millis(200));

    // tools/call
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": tool,
            "arguments": arguments
        }
    });
    writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
    stdin.flush().unwrap();

    let mut resp = serde_json::Value::Null;
    for _ in 0..10 {
        let mut buf = String::new();
        if reader.read_line(&mut buf).unwrap_or(0) == 0 {
            break;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(buf.trim())
            && v.get("id") == Some(&serde_json::json!(2))
        {
            resp = v;
            break;
        }
    }
    drop(stdin);
    (resp, child)
}

/// Extract the text payload from an MCP tools/call response.
fn mcp_text(resp: &serde_json::Value) -> String {
    resp.get("result")
        .and_then(|r| r.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("text"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("(no text in response: {resp})"))
}

/// Convenience: call `sm_groups` (no args) and return the inner `result`
/// string (the human-readable payload). rmcp serializes `Json<TextResult>` so
/// the text we get back is `{"result":"<actual-output>"}`. We unwrap to the
/// actual output (with real newlines) so callers can use `.lines()`.
fn call_sm_groups(home: &Path) -> String {
    let (resp, mut child) = mcp_call(home, "sm_groups", serde_json::json!({}));
    let _ = child.kill();
    let _ = child.wait();
    let raw = mcp_text(&resp);
    eprintln!("sm_groups raw response text:\n{raw}");
    // Parse the inner JSON to extract the human-readable string. If it fails
    // to parse, fall back to the raw text so the test's diagnostic survives.
    let inner = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("result").and_then(|r| r.as_str()).map(String::from))
        .unwrap_or(raw);
    eprintln!("sm_groups inner result:\n{inner}");
    inner
}

// ─── sm_groups tests (PLANNING test-plan §3.10) ─────────────────────────────

/// Test #1: `groups_empty_message` — sm_groups returns the canonical
/// "No groups." message when no groups exist.
#[test]
fn groups_empty_message() {
    let home_guard = fresh_home();
    let home = home_guard.path();

    // Warm DB / dirs by issuing a trivial command so the schema migration runs.
    let _ = run_runai(home, &["status"]);

    let text = call_sm_groups(home);
    // call_sm_groups unwraps the JSON envelope; we get the bare human-readable
    // payload. The empty-state message must match exactly (no trailing list).
    assert_eq!(
        text.trim(),
        "No groups. Use sm_create_group to create one.",
        "sm_groups with no groups must return the canonical empty-state \
         message exactly; got: {text:?}"
    );
}

/// Test #2: `groups_lists_with_metadata` — sm_groups lists each group with
/// id, (display name), member count, and a description preview (truncated to
/// 200 chars with `…` when over).
#[test]
fn groups_lists_with_metadata() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let _ = run_runai(home, &["status"]);

    // Group A: short description (well under 200 chars).
    let short_desc = "Short description for group A.";
    let g_a = run_runai(
        home,
        &[
            "group",
            "create",
            "group-a",
            "--name",
            "Group A",
            "--description",
            short_desc,
        ],
    );
    dump(&g_a, "group create group-a (short desc)");
    assert!(g_a.2.success(), "create group-a failed");

    // Group B: description well over 200 chars (use a 250-char ASCII string).
    let long_desc: String = "L".repeat(250);
    let g_b = run_runai(
        home,
        &[
            "group",
            "create",
            "group-b",
            "--name",
            "Group B",
            "--description",
            &long_desc,
        ],
    );
    dump(&g_b, "group create group-b (long desc)");
    assert!(g_b.2.success(), "create group-b failed");

    let text = call_sm_groups(home);

    // Header: "2 groups:"
    assert!(
        text.contains("2 groups:"),
        "header missing '2 groups:'; got:\n{text}"
    );

    // Each group line: id, "(display_name)", "N members"
    assert!(
        text.contains("group-a (Group A)") && text.contains(" members"),
        "group-a line must show 'group-a (Group A) — N members'; got:\n{text}"
    );
    assert!(
        text.contains("group-b (Group B)") && text.contains(" members"),
        "group-b line must show 'group-b (Group B) — N members'; got:\n{text}"
    );

    // Short description shown fully (no ellipsis on this line).
    assert!(
        text.contains(short_desc),
        "short description '{short_desc}' must appear in full; got:\n{text}"
    );

    // Long description truncated at 200 chars + `…` ellipsis.
    let expected_preview: String = long_desc.chars().take(200).collect();
    assert!(
        text.contains(&expected_preview),
        "long description must be truncated to 200 chars; got:\n{text}"
    );
    assert!(
        text.contains('…'),
        "long description preview must end with `…` ellipsis; got:\n{text}"
    );
    // The full long description must NOT appear (proves truncation happened).
    assert!(
        !text.contains(&long_desc),
        "full 250-char description leaked through truncation; got:\n{text}"
    );
}

/// Test #3: `groups_no_empty_preview` — when a group's description is empty,
/// sm_groups must NOT emit an indented blank preview line (this would render
/// as a 6-space empty line between rows, which the CLI/MCP UX disallows).
#[test]
fn groups_no_empty_preview() {
    let home_guard = fresh_home();
    let home = home_guard.path();
    let _ = run_runai(home, &["status"]);

    // Create one group with NO description.
    let g = run_runai(
        home,
        &["group", "create", "empty-desc", "--name", "Empty Desc"],
    );
    dump(&g, "group create empty-desc (no --description flag)");
    assert!(g.2.success(), "create empty-desc failed");

    let text = call_sm_groups(home);
    let lines: Vec<&str> = text.lines().collect();

    let idx = lines
        .iter()
        .position(|l| l.contains("empty-desc"))
        .unwrap_or_else(|| panic!("'empty-desc' line should exist; full text:\n{text}"));

    // If there is a next line at all, it must NOT be a 6-space-indented blank
    // / whitespace-only preview line.
    if idx + 1 < lines.len() {
        let next = lines[idx + 1];
        assert!(
            !(next.starts_with("      ") && next.trim().is_empty()),
            "REGRESSION: empty-description group emitted a 6-space-indented \
             blank preview line; next line was {next:?}; full output:\n{text}"
        );
    }

    // Spot-check the group is present and was not silently dropped.
    assert!(
        text.contains("empty-desc (Empty Desc)"),
        "empty-desc group line missing; got:\n{text}"
    );
}

/// Test #4: `groups_member_count_accurate` — adding/removing members updates
/// the `N members` figure surfaced by subsequent `sm_groups` calls.
#[test]
fn groups_member_count_accurate() {
    let home_guard = fresh_home();
    let home = home_guard.path();

    // Seed two skills + scan them into the DB so `runai group add` resolves
    // their resource_id (the CLI requires existence of the named resource).
    let managed = home.join(".runai/skills");
    make_skill(&managed, "skill-one", "one body");
    make_skill(&managed, "skill-two", "two body");
    let scan = run_runai(home, &["scan"]);
    dump(&scan, "scan to register seed skills");
    assert!(scan.2.success(), "scan failed");

    // Create the group.
    let g = run_runai(
        home,
        &[
            "group",
            "create",
            "mc-group",
            "--name",
            "MC Group",
            "--description",
            "Member counting",
        ],
    );
    dump(&g, "group create mc-group");
    assert!(g.2.success(), "create mc-group failed");

    // Add two members.
    let add1 = run_runai(home, &["group", "add", "mc-group", "skill-one"]);
    dump(&add1, "group add skill-one");
    assert!(add1.2.success(), "group add skill-one failed");
    let add2 = run_runai(home, &["group", "add", "mc-group", "skill-two"]);
    dump(&add2, "group add skill-two");
    assert!(add2.2.success(), "group add skill-two failed");

    // First sm_groups call: should show "2 members" on the mc-group line.
    let text2 = call_sm_groups(home);
    let line2 = text2
        .lines()
        .find(|l| l.contains("mc-group"))
        .unwrap_or_else(|| panic!("mc-group line missing in:\n{text2}"));
    assert!(
        line2.contains("2 members"),
        "expected '2 members' in mc-group line, got: {line2:?}; full:\n{text2}"
    );

    // Remove one.
    let rm = run_runai(home, &["group", "remove", "mc-group", "skill-one"]);
    dump(&rm, "group remove skill-one");
    assert!(rm.2.success(), "group remove failed");

    // Second sm_groups call: should now show "1 members".
    let text1 = call_sm_groups(home);
    let line1 = text1
        .lines()
        .find(|l| l.contains("mc-group"))
        .unwrap_or_else(|| panic!("mc-group line missing after remove:\n{text1}"));
    assert!(
        line1.contains("1 members"),
        "expected '1 members' in mc-group line after remove, got: {line1:?}; \
         full:\n{text1}"
    );
    // Defensive: must not still claim 2 members.
    assert!(
        !line1.contains("2 members"),
        "stale member count after remove: {line1:?}; full:\n{text1}"
    );
}

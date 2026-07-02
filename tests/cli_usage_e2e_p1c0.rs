//! P1 e2e regression tests for `runai usage` (PLANNING test plan §1.30).
//!
//! Cloud HEAD wires `runai usage` to `SkillManager::usage_stats()`, which
//! sources truth from Claude Code transcripts (`~/.claude/projects/**/*.jsonl`)
//! — the `RUNAI_TRANSCRIPTS_DIR` env var lets us redirect that root to a
//! sandboxed tempdir so the binary is exercised end-to-end without touching
//! the real user transcripts.
//!
//! Each test:
//!   - allocates an isolated `HOME=$(mktemp -d)`
//!   - sets `RUNE_DATA_DIR=$HOME/.runai` (so the DB / cache stays inside the
//!     sandbox — AGENTS.md safety contract rule 1)
//!   - sets `RUNAI_NO_AUTOSPAWN=1` (no dashboard spawn during CLI tests)
//!   - sets `RUNAI_TRANSCRIPTS_DIR=<sandboxed jsonl tree>`
//!   - spawns the workspace-built binary (`env!("CARGO_BIN_EXE_runai")`,
//!     produced by `cargo build`/`cargo test`)
//!
//! Skipped on Windows: the `dirs` crate ignores HOME/USERPROFILE env vars and
//! the parent integration suites are gated the same way (see
//! `manager::tests` and `safety_e2e`).
#![cfg(not(target_os = "windows"))]

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

/// Build a sandboxed `runai usage` invocation.
fn runai_usage(home: &Path, transcripts: &Path, extra_args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(RUNAI_BIN);
    cmd.arg("usage")
        .args(extra_args)
        .env("HOME", home)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", home.join(".runai"))
        .env("RUNAI_TRANSCRIPTS_DIR", transcripts)
        .env_remove("SKILL_MANAGER_DATA_DIR");
    cmd.output().expect("spawn runai usage")
}

fn write_jsonl(path: &Path, lines: &[&str]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create transcripts project dir");
    }
    let body = lines.join("\n") + "\n";
    std::fs::write(path, body).expect("write transcript jsonl");
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// §1.30 test 1 — `usage_aggregates_transcripts`
///
/// Plant a sandboxed transcript directory with skill A (5 hits), skill B (2
/// hits) and an MCP tool call (1 hit). Run `runai usage` and assert:
///   - the column header is present (`uses`, `last`, `type`, `name`)
///   - the rows are sorted by count desc (A → B → C)
///   - the `type` column shows `skill` vs `mcp` correctly
///   - the `last` column is in `<n>{m,h,d} ago` shape (the transcripts use a
///     2026-04 timestamp, so we expect `… ago`)
///   - the exit code is 0
#[test]
fn usage_aggregates_transcripts() {
    let home = TempDir::new().expect("tempdir HOME");
    let transcripts = TempDir::new().expect("tempdir transcripts");

    // Plant a multi-tool transcript. `transcript_stats::scan` reads every
    // `*.jsonl` under each direct subdirectory of the root, looking for
    // `tool_use` entries whose name is `Skill` (input.skill) or
    // `mcp__<server>__*` (the MCP server is the count target).
    let proj = transcripts.path().join("project-a");
    let log = proj.join("session-1.jsonl");

    // 5 hits on skill "alpha"
    let alpha = r#"{"type":"assistant","timestamp":"2026-04-17T01:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Skill","input":{"skill":"alpha"}}]}}"#;
    // 2 hits on skill "beta"
    let beta = r#"{"type":"assistant","timestamp":"2026-04-17T02:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Skill","input":{"skill":"beta"}}]}}"#;
    // 1 hit on mcp "runai"
    let mcp = r#"{"type":"assistant","timestamp":"2026-04-17T03:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__runai__sm_list","input":{}}]}}"#;

    write_jsonl(&log, &[alpha, alpha, alpha, alpha, alpha, beta, beta, mcp]);

    let out = runai_usage(home.path(), transcripts.path(), &[]);
    dump(&out, "usage_aggregates_transcripts");
    assert!(
        out.status.success(),
        "usage exited non-zero; status={}",
        out.status
    );

    let stdout = String::from_utf8(out.stdout).expect("usage stdout utf8");

    // Header columns. Order matters for the sort assertion below.
    assert!(stdout.contains("uses"), "missing 'uses' column header");
    assert!(stdout.contains("last"), "missing 'last' column header");
    assert!(stdout.contains("type"), "missing 'type' column header");
    assert!(stdout.contains("name"), "missing 'name' column header");

    // Rows for our three transcripts.
    assert!(
        stdout.contains("alpha"),
        "missing skill alpha row: {stdout}"
    );
    assert!(stdout.contains("beta"), "missing skill beta row: {stdout}");
    assert!(stdout.contains("runai"), "missing mcp runai row: {stdout}");

    // type column distinguishes skill vs mcp.
    assert!(stdout.contains("skill"), "missing 'skill' type label");
    assert!(stdout.contains("mcp"), "missing 'mcp' type label");

    // sort assertion: alpha (5x) must precede beta (2x) which must precede
    // mcp runai (1x).
    let alpha_idx = stdout.find("alpha").expect("alpha row position");
    let beta_idx = stdout.find("beta").expect("beta row position");
    let mcp_idx = stdout.find("runai").expect("mcp runai row position");
    assert!(
        alpha_idx < beta_idx,
        "alpha (5x) should be sorted before beta (2x); stdout was:\n{stdout}"
    );
    assert!(
        beta_idx < mcp_idx,
        "beta (2x) should be sorted before mcp runai (1x); stdout was:\n{stdout}"
    );

    // last column shape: format_time_ago renders `<n>{m,h,d} ago` for old
    // timestamps. The 2026-04-17 fixture is months in the past, so we expect
    // `d ago` somewhere.
    assert!(
        stdout.contains("ago"),
        "missing time-ago text in last column; stdout was:\n{stdout}"
    );
}

/// §1.30 test 2 — `usage_top_limit_caps_output`
///
/// Plant 5 distinct skills, run `runai usage --top 3`, assert only 3 data
/// rows are emitted (4 lines total = header + 3 rows). This is the e2e
/// surface of the `--top N` flag — the unit-level mock-the-manager case
/// listed in the plan is rejected (we don't mock at the binary boundary).
#[test]
fn usage_top_limit_caps_output() {
    let home = TempDir::new().expect("tempdir HOME");
    let transcripts = TempDir::new().expect("tempdir transcripts");

    let proj = transcripts.path().join("project-top");
    let log = proj.join("session-1.jsonl");

    let mut lines: Vec<String> = Vec::new();
    for i in 0..5 {
        // The skill name embeds the index so each is distinct; one row each
        // ⇒ stats has 5 entries.
        lines.push(format!(
            r#"{{"type":"assistant","timestamp":"2026-04-17T01:00:0{i}Z","message":{{"role":"assistant","content":[{{"type":"tool_use","name":"Skill","input":{{"skill":"sk_{i}"}}}}]}}}}"#,
        ));
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    write_jsonl(&log, &refs);

    let out = runai_usage(home.path(), transcripts.path(), &["--top", "3"]);
    dump(&out, "usage_top_limit_caps_output");
    assert!(
        out.status.success(),
        "usage --top exited non-zero; status={}",
        out.status
    );

    let stdout = String::from_utf8(out.stdout).expect("usage stdout utf8");
    let non_empty: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

    // header line + 3 data rows = 4
    assert_eq!(
        non_empty.len(),
        4,
        "expected 1 header + 3 rows = 4 lines, got {} lines; stdout:\n{stdout}",
        non_empty.len()
    );

    // At least 3 distinct sk_* identifiers appear (the 3 selected). Counting
    // how many distinct names show up — must be exactly 3 of the 5 planted.
    let present_count = (0..5)
        .filter(|i| stdout.contains(&format!("sk_{i}")))
        .count();
    assert_eq!(
        present_count, 3,
        "--top 3 should emit exactly 3 of the 5 skills; saw {present_count} in:\n{stdout}"
    );
}

/// §1.30 test 3 — `usage_empty_outputs_message`
///
/// With an empty transcripts root, `runai usage` must print exactly
/// `No usage data yet.` and exit 0. No header, no rows.
#[test]
fn usage_empty_outputs_message() {
    let home = TempDir::new().expect("tempdir HOME");
    let transcripts = TempDir::new().expect("tempdir transcripts");
    // Note: we deliberately do NOT plant any *.jsonl under the transcripts
    // root, so scan_default → empty TranscriptStats → empty Vec<UsageStat>
    // → the "No usage data yet." branch fires.

    let out = runai_usage(home.path(), transcripts.path(), &[]);
    dump(&out, "usage_empty_outputs_message");
    assert!(
        out.status.success(),
        "usage (empty) exited non-zero; status={}",
        out.status
    );

    let stdout = String::from_utf8(out.stdout).expect("usage stdout utf8");
    let trimmed = stdout.trim();
    assert_eq!(
        trimmed, "No usage data yet.",
        "empty usage should print exactly 'No usage data yet.', got:\n{stdout}"
    );

    // sanity: no header / no rows
    assert!(
        !stdout.contains("uses"),
        "empty usage must not print header: {stdout}"
    );
    assert!(
        !stdout.contains("skill"),
        "empty usage must not print 'skill' type label: {stdout}"
    );
}

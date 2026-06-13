//! End-to-end regression for `runai recommend hook-snippet` (P1).
//!
//! Plan ref: PLANNING/test-plan §1.35 recommend hook-snippet.
//! Entry point: `src/cli/mod.rs` `(Some(RecommendCommands::HookSnippet), _)`.
//!
//! The handler prints a hook-snippet block to stdout — meant to be pasted
//! into `~/.claude/settings.json`. These tests guard:
//!   1. the embedded JSON sub-block parses cleanly as JSON, has the right
//!      `hooks.UserPromptSubmit[].hooks[].command` shape, and the command
//!      string is exactly `runai recommend`;
//!   2. the surrounding help text mentions both `install-hook` and
//!      `uninstall-hook` so users discover the automated alternative.
//!
//! Sandbox: an isolated `HOME=<tempdir>` with `RUNE_DATA_DIR` pointing under
//! that tempdir; we never touch the real `~/.runai/` per AGENTS.md safety
//! contract. `hook-snippet` itself is read-only stdout, but `runai` may
//! initialise `~/.runai/` on first run, so we still keep it sandboxed.
#![cfg(not(target_os = "windows"))]

use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Build an isolated HOME and run `runai recommend hook-snippet`.
fn run_hook_snippet() -> (TempDir, std::process::Output) {
    let home = tempfile::tempdir().expect("create tmp HOME");
    let rune_data = home.path().join(".runai");
    std::fs::create_dir_all(&rune_data).unwrap();
    let out = runai()
        .args(["recommend", "hook-snippet"])
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", &rune_data)
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .expect("spawn runai recommend hook-snippet");
    (home, out)
}

/// Extract the JSON object embedded in `hook-snippet` output.
///
/// The handler prints a `{ ... }` block inline with surrounding prose; we
/// locate the outermost balanced brace pair so the test does not depend on
/// the exact wording around it.
fn extract_json_block(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth: i64 = 0;
    let bytes = s.as_bytes();
    for (i, c) in bytes.iter().enumerate().skip(start) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

// ─── tests ─────────────────────────────────────────────────────────────────

/// `recommend_hook_snippet_valid_json` — output must contain a JSON object
/// that parses cleanly, exposes `hooks.UserPromptSubmit[]`, and whose
/// embedded command is exactly `runai recommend` (the wire string that
/// Claude Code will invoke).
#[test]
fn recommend_hook_snippet_valid_json() {
    let (_home, out) = run_hook_snippet();
    assert!(
        out.status.success(),
        "hook-snippet exited non-zero. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let json_block = extract_json_block(&stdout)
        .unwrap_or_else(|| panic!("could not locate JSON block in hook-snippet output:\n{stdout}"));

    let parsed: serde_json::Value = serde_json::from_str(json_block).unwrap_or_else(|e| {
        panic!("hook-snippet JSON block did not parse: {e}\nblock was:\n{json_block}")
    });

    let hooks = parsed
        .get("hooks")
        .unwrap_or_else(|| panic!("missing top-level `hooks` key in: {parsed}"));
    let ups = hooks
        .get("UserPromptSubmit")
        .unwrap_or_else(|| panic!("missing `UserPromptSubmit` under hooks in: {hooks}"));
    let ups_arr = ups.as_array().unwrap_or_else(|| {
        panic!("`UserPromptSubmit` should be a JSON array, got: {ups}");
    });
    assert!(
        !ups_arr.is_empty(),
        "`UserPromptSubmit` array should not be empty"
    );

    // Walk the standard Claude Code shape:
    //   UserPromptSubmit: [ { hooks: [ { type, command } ] } ]
    // and locate the command string.
    let mut found_command: Option<String> = None;
    for entry in ups_arr {
        if let Some(inner) = entry.get("hooks").and_then(|v| v.as_array()) {
            for h in inner {
                if let Some(cmd) = h.get("command").and_then(|v| v.as_str()) {
                    found_command = Some(cmd.to_string());
                    break;
                }
            }
        }
        if found_command.is_some() {
            break;
        }
    }
    let cmd = found_command
        .unwrap_or_else(|| panic!("no `command` field inside UserPromptSubmit.hooks[] of {ups}"));
    assert_eq!(
        cmd, "runai recommend",
        "snippet command must be exactly `runai recommend` so Claude Code invokes the router; got `{cmd}`"
    );
}

/// `recommend_hook_snippet_mentions_install` — the explanatory prose must
/// point users at the automated `install-hook` / `uninstall-hook` flow.
#[test]
fn recommend_hook_snippet_mentions_install() {
    let (_home, out) = run_hook_snippet();
    assert!(
        out.status.success(),
        "hook-snippet exited non-zero. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();

    assert!(
        stdout.contains("runai recommend install-hook"),
        "snippet help must mention `runai recommend install-hook` so users discover the automated wiring; got:\n{stdout}"
    );
    assert!(
        stdout.contains("runai recommend uninstall-hook"),
        "snippet help must mention `runai recommend uninstall-hook` so users can roll back; got:\n{stdout}"
    );
}

// P2 e2e tests for `runai recommend hook-snippet`.
//
// Covers test plan §1.35:
//   1. recommend_hook_snippet_valid_json — output contains a valid JSON
//      block with the UserPromptSubmit structure and "runai recommend"
//      command (so users can copy-paste into ~/.claude/settings.json).
//   2. recommend_hook_snippet_mentions_install — output mentions the
//      install-hook and uninstall-hook automation commands.
//
// The handler is pure stdout (no filesystem mutation), but we still
// sandbox HOME / RUNE_DATA_DIR per the safety contract so the test
// cannot perturb the developer's real ~/.runai/ even if a future
// refactor adds incidental I/O.

use std::process::Command;
use tempfile::TempDir;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

fn run_hook_snippet() -> (bool, String, String) {
    let scratch = TempDir::new().expect("tempdir");
    let data_dir = scratch.path().join(".runai");
    let output = Command::new(RUNAI_BIN)
        .args(["recommend", "hook-snippet"])
        .env("HOME", scratch.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", &data_dir)
        .output()
        .expect("failed to execute runai recommend hook-snippet");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status.success(), stdout, stderr)
}

/// Extract the JSON block from the stdout. The handler prints prose
/// surrounding a `{ ... }` block; we scan for the first balanced
/// brace pair so the test does not depend on exact line counts.
fn extract_json_block(stdout: &str) -> String {
    let start = stdout
        .find('{')
        .expect("hook-snippet output should contain a '{'");
    let bytes = stdout.as_bytes();
    let mut depth = 0i32;
    let mut end = start;
    for (i, b) in bytes.iter().enumerate().skip(start) {
        match *b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(end > start, "did not find closing brace for JSON block");
    stdout[start..end].to_string()
}

#[test]
fn recommend_hook_snippet_valid_json() {
    let (ok, stdout, stderr) = run_hook_snippet();
    assert!(
        ok,
        "`runai recommend hook-snippet` should exit 0 — stderr={stderr}"
    );

    let json_block = extract_json_block(&stdout);

    // 1. Must parse as valid JSON (no trailing commas / dangling braces).
    let parsed: serde_json::Value = serde_json::from_str(&json_block).unwrap_or_else(|e| {
        panic!("hook-snippet JSON block is invalid: {e}\nblock:\n{json_block}")
    });

    // 2. Must carry a hooks.UserPromptSubmit array.
    let user_prompt_submit = parsed
        .get("hooks")
        .and_then(|h| h.get("UserPromptSubmit"))
        .and_then(|v| v.as_array())
        .expect("hooks.UserPromptSubmit must be an array");
    assert!(
        !user_prompt_submit.is_empty(),
        "UserPromptSubmit array must contain at least one entry"
    );

    // 3. The inner command must be exactly "runai recommend".
    let inner = &user_prompt_submit[0];
    let inner_hooks = inner
        .get("hooks")
        .and_then(|v| v.as_array())
        .expect("inner .hooks must be an array");
    let cmd = inner_hooks
        .iter()
        .find_map(|h| h.get("command").and_then(|c| c.as_str()))
        .expect("expected a command field on the inner hook");
    assert_eq!(
        cmd, "runai recommend",
        "the wired command must be exactly 'runai recommend' (copy-paste contract)"
    );

    // The hook entries themselves declare `"type": "command"`.
    let kind = inner_hooks
        .iter()
        .find_map(|h| h.get("type").and_then(|c| c.as_str()))
        .expect("expected a type field on the inner hook");
    assert_eq!(kind, "command");
}

#[test]
fn recommend_hook_snippet_mentions_install() {
    let (ok, stdout, stderr) = run_hook_snippet();
    assert!(
        ok,
        "`runai recommend hook-snippet` should exit 0 — stderr={stderr}"
    );

    assert!(
        stdout.contains("runai recommend install-hook"),
        "hook-snippet output must mention `runai recommend install-hook`; stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("runai recommend uninstall-hook"),
        "hook-snippet output must mention `runai recommend uninstall-hook`; stdout:\n{stdout}"
    );

    // Sanity: also tells users where to paste manually. This is the
    // user-facing instruction layer.
    assert!(
        stdout.contains("~/.claude/settings.json"),
        "expected mention of ~/.claude/settings.json; stdout:\n{stdout}"
    );
}

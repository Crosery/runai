//! Physical e2e for the TUI Hooks Toggle feature
//! (PLANNING §1.5 — `src/tui/app/hook_panel.rs::toggle_selected_hook`).
//!
//! Toggling a row in the Hooks tab shells out to
//! `crate::core::recommend::{install_claude_hook, uninstall_claude_hook}` —
//! the same code paths the `runai recommend install-hook` /
//! `uninstall-hook` CLI subcommands use. These tests pin down the
//! contract those two functions must satisfy:
//!
//!   * Install plants a canonical-shape entry in
//!     `~/.claude/settings.json`:
//!       `hooks.UserPromptSubmit[].hooks[] = {type:"command", command:"runai recommend"}`
//!   * Uninstall removes only that entry; the rest of the file (sibling
//!     hooks, unrelated keys, other `UserPromptSubmit` groups) survives.
//!   * Install is idempotent: a second call returns `AlreadyPresent` and
//!     never duplicates the entry inside the array.
//!   * Install creates the `.claude` directory tree when missing.
//!   * Install writes exclusively to `HOME/.claude/settings.json` — a
//!     custom `RUNE_DATA_DIR` is NOT touched (hook config is CLI-scoped,
//!     not runai-data-scoped).
//!
//! Each test runs in its own `tempfile::TempDir` HOME so they never
//! touch the real `~/.claude/` (per AGENTS.md safety contract). HOME
//! mocking + the underlying symlink tests on this branch are unix-only,
//! mirroring `tests/tui_hook_panel_test.rs` and `tests/safety_e2e.rs`.
#![cfg(not(target_os = "windows"))]

use runai::core::recommend::{HookInstallStatus, install_claude_hook, uninstall_claude_hook};

/// The exact command-string the install path writes into the
/// `UserPromptSubmit` entry. Pinned here to lock the wire shape — if
/// the CLI changes this constant the TUI toggle (which goes through the
/// same install fn) must change with it, and the regression test
/// catches the drift.
const RUNAI_HOOK_COMMAND: &str = "runai recommend";

/// Count how many `UserPromptSubmit` groups in `settings.json` carry a
/// hook entry whose `command` field equals `RUNAI_HOOK_COMMAND`. Used
/// to verify idempotence: must be 1 after one or many installs.
fn count_runai_hook_entries(value: &serde_json::Value) -> usize {
    let ups = match value
        .get("hooks")
        .and_then(|h| h.get("UserPromptSubmit"))
        .and_then(|a| a.as_array())
    {
        Some(a) => a,
        None => return 0,
    };
    ups.iter()
        .map(|group| {
            group
                .get("hooks")
                .and_then(|h| h.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter(|h| {
                            h.get("command").and_then(|c| c.as_str()) == Some(RUNAI_HOOK_COMMAND)
                        })
                        .count()
                })
                .unwrap_or(0)
        })
        .sum()
}

fn read_settings_json(home: &std::path::Path) -> serde_json::Value {
    let path = home.join(".claude").join("settings.json");
    let txt =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&txt).unwrap_or_else(|e| panic!("parse {} as JSON: {e}", path.display()))
}

/// **[physical_e2e]** Test scenario 1 from plan §2.18.
///
/// After `install_claude_hook(&home)`:
///   * `<home>/.claude/settings.json` exists
///   * top-level `hooks.UserPromptSubmit` is a non-empty array
///   * the first group contains a `{type:"command", command:"runai recommend"}`
///     hook entry
///
/// This is the canonical shape `runai recommend install-hook` writes,
/// and the only shape the TUI toggle is allowed to write — otherwise
/// the CLI and the TUI would step on each other's settings.json.
#[test]
fn hooks_toggle_install_writes_canonical_settings() {
    let home = tempfile::tempdir().expect("tmp HOME");
    let status =
        install_claude_hook(home.path()).expect("install_claude_hook succeeds in fresh HOME");
    assert_eq!(status, HookInstallStatus::Installed);

    let settings_path = home.path().join(".claude").join("settings.json");
    assert!(
        settings_path.exists(),
        "install_claude_hook must create {}",
        settings_path.display()
    );

    let value = read_settings_json(home.path());
    let ups = value
        .get("hooks")
        .and_then(|h| h.get("UserPromptSubmit"))
        .and_then(|a| a.as_array())
        .expect("hooks.UserPromptSubmit must be an array");
    assert!(!ups.is_empty(), "UserPromptSubmit array must be non-empty");

    let inner = ups[0]
        .get("hooks")
        .and_then(|h| h.as_array())
        .expect("first group must carry a `hooks` array");
    assert!(!inner.is_empty(), "first group's hooks must be non-empty");
    let hook = &inner[0];
    assert_eq!(
        hook.get("type").and_then(|t| t.as_str()),
        Some("command"),
        "hook type must be 'command'"
    );
    assert_eq!(
        hook.get("command").and_then(|c| c.as_str()),
        Some(RUNAI_HOOK_COMMAND),
        "hook command must be exactly '{RUNAI_HOOK_COMMAND}'"
    );

    // Exactly one runai entry — install does not duplicate on fresh HOME.
    assert_eq!(
        count_runai_hook_entries(&value),
        1,
        "fresh install must yield exactly one runai hook entry"
    );
}

/// **[physical_e2e]** Test scenario 2 from plan §2.18.
///
/// Plant a settings.json that holds (a) an unrelated user hook in
/// `UserPromptSubmit`, (b) the runai hook, and (c) an unrelated
/// top-level key. `uninstall_claude_hook` must drop only (b) and leave
/// (a) + (c) untouched. The JSON must still parse, and no empty
/// `UserPromptSubmit` array shape must be left behind for the other
/// hook.
#[test]
fn hooks_toggle_uninstall_preserves_other_hooks() {
    let home = tempfile::tempdir().expect("tmp HOME");
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let settings_path = home.path().join(".claude").join("settings.json");

    // Pre-existing user config: their own hook + a top-level theme key
    // that has nothing to do with runai. Both must survive uninstall.
    let pre = serde_json::json!({
        "theme": "dark",
        "hooks": {
            "UserPromptSubmit": [
                {"hooks": [{"type": "command", "command": "my-other-hook --arg"}]},
                {"hooks": [{"type": "command", "command": "runai recommend"}]}
            ]
        }
    });
    std::fs::write(&settings_path, serde_json::to_string_pretty(&pre).unwrap()).unwrap();

    let status = uninstall_claude_hook(home.path()).expect("uninstall returns Ok");
    assert_eq!(status, HookInstallStatus::Removed);

    let after = read_settings_json(home.path());
    // Top-level user key must survive.
    assert_eq!(
        after.get("theme").and_then(|v| v.as_str()),
        Some("dark"),
        "unrelated top-level keys must be preserved"
    );

    // The user's own hook must remain.
    let ups = after
        .get("hooks")
        .and_then(|h| h.get("UserPromptSubmit"))
        .and_then(|a| a.as_array())
        .expect("UserPromptSubmit array must still exist");
    assert!(
        ups.iter().any(|group| {
            group
                .get("hooks")
                .and_then(|h| h.as_array())
                .is_some_and(|arr| {
                    arr.iter().any(|h| {
                        h.get("command").and_then(|c| c.as_str()) == Some("my-other-hook --arg")
                    })
                })
        }),
        "the unrelated user hook must survive runai uninstall"
    );

    // Runai entry must be gone.
    assert_eq!(
        count_runai_hook_entries(&after),
        0,
        "uninstall must remove every runai hook entry"
    );
}

/// **[physical_e2e]** Test scenario 3 from plan §2.18.
///
/// Two consecutive `install_claude_hook` calls. Second one must return
/// `AlreadyPresent` and the settings.json must still carry exactly ONE
/// runai entry — never a duplicated array element. This is what makes
/// "toggle twice" or "CLI + TUI both touched it" safe.
#[test]
fn hooks_toggle_install_idempotent() {
    let home = tempfile::tempdir().expect("tmp HOME");

    let first = install_claude_hook(home.path()).expect("first install_claude_hook succeeds");
    assert_eq!(first, HookInstallStatus::Installed, "first call installs");

    let second = install_claude_hook(home.path()).expect("second install_claude_hook succeeds");
    assert_eq!(
        second,
        HookInstallStatus::AlreadyPresent,
        "second call must report AlreadyPresent"
    );

    let value = read_settings_json(home.path());
    assert_eq!(
        count_runai_hook_entries(&value),
        1,
        "two installs must not duplicate the runai hook entry"
    );
}

/// **[physical_e2e]** Test scenario 4 from plan §2.18.
///
/// HOME starts with no `.claude/` directory at all. `install_claude_hook`
/// must create the directory tree AND the settings.json — first-time
/// install on a brand-new Claude Code install must Just Work, not
/// surface a "no such directory" error to the user.
#[test]
fn hooks_toggle_creates_missing_settings() {
    let home = tempfile::tempdir().expect("tmp HOME");
    let claude_dir = home.path().join(".claude");
    let settings_path = claude_dir.join("settings.json");

    // Belt-and-suspenders precondition: the dir really is missing.
    assert!(
        !claude_dir.exists(),
        "precondition: ~/.claude must not exist yet"
    );

    let status = install_claude_hook(home.path()).expect("install_claude_hook creates the tree");
    assert_eq!(status, HookInstallStatus::Installed);

    assert!(
        claude_dir.is_dir(),
        "{} must be created as a real directory",
        claude_dir.display()
    );
    assert!(
        settings_path.exists(),
        "{} must be created",
        settings_path.display()
    );

    // And the content must be the canonical-shape settings the other
    // tests pin — first-time install ≠ malformed install.
    let value = read_settings_json(home.path());
    assert_eq!(
        count_runai_hook_entries(&value),
        1,
        "first-time install must plant exactly one runai hook"
    );
}

/// **[physical_e2e]** Test scenario 5 from plan §2.18.
///
/// Hook config is CLI-scoped (it lives in `~/.claude/settings.json`),
/// not runai-data-scoped. Even when a custom `RUNE_DATA_DIR` is set,
/// `install_claude_hook(&home)` must only ever write under
/// `<home>/.claude/`, never under the data dir.
///
/// `install_claude_hook` takes `home` directly so it does not consult
/// `RUNE_DATA_DIR` at all — this test enforces that by pointing
/// `RUNE_DATA_DIR` at a tempdir, calling install, and asserting the
/// data dir is left empty while HOME got the canonical settings.json.
/// If a future refactor accidentally routes part of the hook payload
/// through `paths::data_dir()`, this test fails.
#[test]
fn hooks_toggle_ignores_custom_data_dir() {
    let home = tempfile::tempdir().expect("tmp HOME");
    let custom_data = tempfile::tempdir().expect("tmp RUNE_DATA_DIR");

    // The install fn doesn't read env vars on its own, but a future
    // regression could; mimic the CLI's invocation surface by setting
    // these around the call and tearing them down right after.
    //
    // SAFETY: env::set_var/remove_var require unsafe on edition 2024 in
    // a multi-threaded test runner. Using `--test-threads=1` (CI gate)
    // serializes us; the helper restores state before returning so a
    // future parallel test doesn't observe a sticky env.
    unsafe {
        std::env::set_var("RUNE_DATA_DIR", custom_data.path());
    }
    let install_result = install_claude_hook(home.path());
    unsafe {
        std::env::remove_var("RUNE_DATA_DIR");
    }
    let status = install_result.expect("install_claude_hook succeeds under RUNE_DATA_DIR");
    assert_eq!(status, HookInstallStatus::Installed);

    // HOME got the canonical settings.json.
    let value = read_settings_json(home.path());
    assert_eq!(
        count_runai_hook_entries(&value),
        1,
        "HOME must receive the canonical runai hook entry"
    );

    // RUNE_DATA_DIR must not have grown any hook-related artifacts.
    // The directory itself may stay empty (install doesn't touch it);
    // we assert that no `settings.json` / `.claude` / runai hook
    // payload landed there.
    let stray = custom_data.path().join(".claude");
    assert!(
        !stray.exists(),
        "RUNE_DATA_DIR must not receive a .claude tree: {}",
        stray.display()
    );
    let stray_settings = custom_data.path().join("settings.json");
    assert!(
        !stray_settings.exists(),
        "RUNE_DATA_DIR must not receive a settings.json: {}",
        stray_settings.display()
    );

    // Spot-check: any direct child file/dir inside the custom data dir
    // that mentions "claude" or "settings" is a smoking gun.
    if let Ok(rd) = std::fs::read_dir(custom_data.path()) {
        for entry in rd.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            assert!(
                !s.contains("claude") && !s.contains("settings"),
                "stray hook artifact landed under RUNE_DATA_DIR: {}",
                entry.path().display()
            );
        }
    }
}

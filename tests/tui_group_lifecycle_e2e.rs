//! Physical end-to-end coverage for the TUI "Create Group (2-step)" flow.
//!
//! Each test drives the public `App::handle_key` state machine in-process with
//! a sandboxed `SkillManager::with_base(tmp/data)` and an isolated HOME so the
//! `reload()` call (which scans the four CLI MCP configs via `dirs::home_dir()`)
//! never touches the real user data dir.
//!
//! The TUI module's inline unit tests already cover other key flows under the
//! `HOME_LOCK` from `crate::test_support` — that helper is `#[cfg(test)]` only
//! and not reachable from integration tests, so this file declares its own
//! process-wide HOME mutex and uses `SkillManager::with_base` to point the
//! managed data dir at a tempdir directly.
//!
//! Skipped on Windows: `dirs::home_dir()` ignores HOME on Windows (see
//! `AGENTS.md` Key constraints), so mocking via env var cannot isolate the
//! test environment from the user's real config.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::group::{Group, GroupKind};
use runai::core::manager::SkillManager;
use runai::tui::app::{App, InputMode, Tab};

// ─── harness ────────────────────────────────────────────────────────────────

/// Process-wide HOME lock. Tests in this file run on a `--test-threads=1`
/// gate (the CI default), but using a real mutex makes the harness robust
/// against future parallel test runs and matches the contract documented in
/// `src/test_support.rs`.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Run `f` with `HOME` set to `tmp` and `RUNE_DATA_DIR` set to
/// `tmp/.runai`, restoring previous values afterwards. The default-home
/// integration tests use the same pattern as the inline TUI tests.
fn with_isolated_home<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original_home = std::env::var("HOME").ok();
    let original_rune = std::env::var("RUNE_DATA_DIR").ok();
    let original_skill = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
    unsafe {
        std::env::set_var("HOME", tmp);
        std::env::set_var("RUNE_DATA_DIR", tmp.join(".runai"));
        // Make sure the legacy override doesn't leak from the host env.
        std::env::remove_var("SKILL_MANAGER_DATA_DIR");
    }
    // Pre-create CLI dirs so `read_mcp_status_from_configs` finds them empty
    // (no panic on missing parent) and the managed data root.
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let _ = std::fs::create_dir_all(tmp.join(format!(".{cli}")).join("skills"));
    }
    let _ = std::fs::create_dir_all(tmp.join(".runai").join("skills"));
    let _ = std::fs::create_dir_all(tmp.join(".runai").join("groups"));

    f();

    unsafe {
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match original_rune {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match original_skill {
            Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
            None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
        }
    }
}

/// Build a fresh `App` rooted at `<tmp>/.runai` with the Groups tab selected.
/// Use within `with_isolated_home` so reload() / list_groups() stay confined.
fn fresh_app_on_groups_tab(tmp: &Path) -> App {
    let data = tmp.join(".runai");
    let mgr = SkillManager::with_base(data).expect("SkillManager::with_base on tmp data dir");
    let mut app = App::new(mgr);
    app.tab = Tab::Groups;
    app.mode = InputMode::Normal;
    app
}

/// `paths::groups_dir()` resolved through `with_base(tmp.join(".runai"))`.
fn expected_groups_dir(tmp: &Path) -> PathBuf {
    tmp.join(".runai").join("groups")
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[test]
fn create_group_step_0_name_input() {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    with_isolated_home(tmp.path(), || {
        let mut app = fresh_app_on_groups_tab(tmp.path());

        // Pre-load the input buffer with stale content to assert the 'c'
        // handler clears it (the create_name buffer too).
        app.input_buf.push_str("stale-input");
        app.create_name.push_str("stale-name");

        // Press 'c' from Normal mode on the Groups tab.
        app.handle_key(key(KeyCode::Char('c')));
        assert!(
            matches!(app.mode, InputMode::CreateGroup(0)),
            "mode after 'c' must be CreateGroup(0)"
        );
        assert!(
            app.input_buf.is_empty(),
            "input_buf must be cleared on entering CreateGroup(0)"
        );
        assert!(
            app.create_name.is_empty(),
            "create_name must be cleared on entering CreateGroup(0)"
        );

        // Type "my-group-name" one char at a time and assert each accumulation.
        for c in "my-group-name".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            app.input_buf, "my-group-name",
            "chars accumulate into input_buf"
        );

        // Backspace removes the last char.
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(
            app.input_buf, "my-group-nam",
            "Backspace removes last char from input_buf"
        );

        // Escape cancels and returns to Normal — no file written.
        app.handle_key(key(KeyCode::Esc));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "Esc returns to Normal"
        );
        assert!(
            app.input_buf.is_empty(),
            "Esc must also clear input_buf to avoid leaking state into the next prompt"
        );
        let written = std::fs::read_dir(expected_groups_dir(tmp.path()))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(written, 0, "no .toml file should be written on Esc cancel");
    });
}

#[test]
fn create_group_step_1_description_input() {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    with_isolated_home(tmp.path(), || {
        let mut app = fresh_app_on_groups_tab(tmp.path());

        // Step 0: enter "My Cool Group" then press Enter to advance.
        app.handle_key(key(KeyCode::Char('c')));
        for c in "My Cool Group".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));

        assert!(
            matches!(app.mode, InputMode::CreateGroup(1)),
            "Enter on a non-empty name advances to step 1"
        );
        assert_eq!(
            app.create_name, "My Cool Group",
            "create_name captures the trimmed step-0 input"
        );
        assert!(
            app.input_buf.is_empty(),
            "input_buf must clear between steps so the description starts fresh"
        );

        // Step 1: type description.
        for c in "A group for cool stuff".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            app.input_buf, "A group for cool stuff",
            "description characters accumulate independent of create_name"
        );
        // Backspace works the same way.
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input_buf, "A group for cool stuf");

        // Esc cancels back to Normal without writing.
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, InputMode::Normal));
        let toml_count = std::fs::read_dir(expected_groups_dir(tmp.path()))
            .map(|d| {
                d.filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("toml"))
                    .count()
            })
            .unwrap_or(0);
        assert_eq!(
            toml_count, 0,
            "Esc on step 1 must not persist any TOML file"
        );
    });
}

#[test]
fn create_group_generates_id_from_name() {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    with_isolated_home(tmp.path(), || {
        let mut app = fresh_app_on_groups_tab(tmp.path());

        // Press 'c' → enter step 0.
        app.handle_key(key(KeyCode::Char('c')));
        // Type a name with mixed case, spaces, digits, and special chars.
        for c in "My Cool Group 2025!".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        // Advance to step 1.
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, InputMode::CreateGroup(1)));
        // Confirm step 1 with an empty description.
        app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "Enter on step 1 returns to Normal"
        );

        // The generated id collapses runs of non-alphanumerics into single
        // dashes, lowercases, and strips leading/trailing dashes.
        let expected_id = "my-cool-group-2025";
        let toml_path = expected_groups_dir(tmp.path()).join(format!("{expected_id}.toml"));
        assert!(
            toml_path.exists(),
            "expected generated id '{expected_id}' to land at {}",
            toml_path.display()
        );

        // Cross-check via the manager API (canonical reader).
        let mgr = SkillManager::with_base(tmp.path().join(".runai")).unwrap();
        let groups = mgr.list_groups().expect("list_groups");
        assert!(
            groups.iter().any(|(id, _)| id == expected_id),
            "list_groups should surface the generated id"
        );
    });
}

#[test]
fn create_group_writes_toml_file() {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    with_isolated_home(tmp.path(), || {
        // Before: groups dir empty.
        let gdir = expected_groups_dir(tmp.path());
        assert!(gdir.exists());
        let before: Vec<_> = std::fs::read_dir(&gdir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            before.is_empty(),
            "groups dir should be empty before create"
        );

        let mut app = fresh_app_on_groups_tab(tmp.path());
        // Drive: c → "TestGroup" → Enter → "Test Desc" → Enter.
        app.handle_key(key(KeyCode::Char('c')));
        for c in "TestGroup".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        for c in "Test Desc".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));

        // After: TOML file present at expected path.
        let id = "testgroup";
        let toml_path = gdir.join(format!("{id}.toml"));
        assert!(
            toml_path.exists(),
            "TOML file must be created at {}",
            toml_path.display()
        );

        // Content: valid TOML with [group] section, name = "TestGroup",
        // description = "Test Desc", members empty.
        let content = std::fs::read_to_string(&toml_path).expect("read toml");
        assert!(
            content.contains("[group]"),
            "TOML must contain [group] section, got:\n{content}"
        );
        let parsed = Group::from_toml(&content).expect("parse generated TOML");
        assert_eq!(parsed.name, "TestGroup", "name field round-trips");
        assert_eq!(
            parsed.description, "Test Desc",
            "description field round-trips"
        );
        assert!(
            parsed.members.is_empty(),
            "newly-created group must have empty members list"
        );
        // Mode returns to Normal and success message is set.
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(
            app.message
                .as_deref()
                .map(|m| m.contains(id))
                .unwrap_or(false),
            "success message should mention the new group id, got: {:?}",
            app.message
        );
    });
}

#[test]
fn create_group_updates_list_and_reloads() {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    with_isolated_home(tmp.path(), || {
        // Pre-seed two groups via the manager.
        let mgr_seed = SkillManager::with_base(tmp.path().join(".runai")).unwrap();
        mgr_seed
            .create_group(
                "alpha",
                &Group {
                    name: "Alpha".into(),
                    description: "first".into(),
                    kind: GroupKind::Custom,
                    auto_enable: false,
                    members: vec![],
                },
            )
            .expect("create alpha");
        mgr_seed
            .create_group(
                "beta",
                &Group {
                    name: "Beta".into(),
                    description: "second".into(),
                    kind: GroupKind::Custom,
                    auto_enable: false,
                    members: vec![],
                },
            )
            .expect("create beta");
        drop(mgr_seed);

        let mut app = fresh_app_on_groups_tab(tmp.path());
        // Switch off Groups to prove the create flow snaps back to it on success.
        app.tab = Tab::Skills;
        app.reload();
        assert_eq!(
            app.groups.len(),
            2,
            "two seeded groups visible after reload"
        );

        // Drive: c → "Gamma" → Enter → "third" → Enter.
        app.handle_key(key(KeyCode::Char('c')));
        for c in "Gamma".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));
        for c in "third".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        app.handle_key(key(KeyCode::Enter));

        // Tab snaps back to Groups after creation.
        assert!(
            matches!(app.tab, Tab::Groups),
            "tab switches to Groups after create"
        );
        // groups list is refreshed in-place — now 3 entries.
        assert_eq!(
            app.groups.len(),
            3,
            "groups list should now show 3 entries after reload"
        );
        // The new group has 0 members and 0 enabled.
        let new = app
            .groups
            .iter()
            .find(|(id, _, _, _, _)| id == "gamma")
            .expect("new group present");
        assert_eq!(new.2, 0, "new group has 0 members");
        assert_eq!(new.3, 0, "new group has 0 enabled");
        // Success message references the new id.
        assert!(
            app.message
                .as_deref()
                .map(|m| m.contains("gamma"))
                .unwrap_or(false),
            "message should reference 'gamma', got: {:?}",
            app.message
        );
    });
}

#[test]
fn create_group_rejects_empty_name() {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    with_isolated_home(tmp.path(), || {
        let mut app = fresh_app_on_groups_tab(tmp.path());

        // Press 'c' → step 0. Type only spaces.
        app.handle_key(key(KeyCode::Char('c')));
        for c in "   ".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input_buf, "   ");
        // Enter on whitespace-only input must reject and return to Normal
        // without advancing to step 1.
        app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "Enter on whitespace-only name returns to Normal"
        );

        // No file written to groups/ — directory still empty.
        let gdir = expected_groups_dir(tmp.path());
        let toml_count = std::fs::read_dir(&gdir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("toml"))
            .count();
        assert_eq!(
            toml_count, 0,
            "no TOML file may be written for an empty name"
        );
    });
}

#[test]
fn create_group_respects_rune_data_dir() {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    let alt_data = tempfile::tempdir().expect("alt data dir");
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let original_home = std::env::var("HOME").ok();
    let original_rune = std::env::var("RUNE_DATA_DIR").ok();
    let original_skill = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
    unsafe {
        std::env::set_var("HOME", tmp.path());
        std::env::set_var("RUNE_DATA_DIR", alt_data.path());
        std::env::remove_var("SKILL_MANAGER_DATA_DIR");
    }
    // Pre-create CLI dirs in HOME so reload's MCP scan is benign.
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let _ = std::fs::create_dir_all(tmp.path().join(format!(".{cli}")).join("skills"));
    }

    // Build the manager with explicit base = alt_data — exactly what
    // `paths::with_base(RUNE_DATA_DIR)` would resolve to from the CLI.
    let mgr = SkillManager::with_base(alt_data.path().to_path_buf())
        .expect("manager rooted at alt data dir");
    let mut app = App::new(mgr);
    app.tab = Tab::Groups;
    app.mode = InputMode::Normal;

    // Drive create flow.
    app.handle_key(key(KeyCode::Char('c')));
    for c in "Custom".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));
    for c in "test".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    app.handle_key(key(KeyCode::Enter));

    // File landed in alt_data/groups/, NOT in tmp/.runai/groups/.
    let alt_toml = alt_data.path().join("groups").join("custom.toml");
    assert!(
        alt_toml.exists(),
        "TOML must be written into RUNE_DATA_DIR's groups dir: {}",
        alt_toml.display()
    );
    let default_toml = tmp.path().join(".runai").join("groups").join("custom.toml");
    assert!(
        !default_toml.exists(),
        "TOML must NOT leak into the default HOME-rooted groups dir: {}",
        default_toml.display()
    );

    // list_groups against the alt data dir surfaces it.
    let mgr2 = SkillManager::with_base(alt_data.path().to_path_buf()).unwrap();
    let groups = mgr2.list_groups().unwrap();
    assert!(
        groups.iter().any(|(id, _)| id == "custom"),
        "list_groups on alt data dir should see the new group"
    );

    // Switching to the default-home managed data dir does not see it.
    let mgr_default =
        SkillManager::with_base(tmp.path().join(".runai")).expect("manager on default data dir");
    let default_groups = mgr_default.list_groups().unwrap();
    assert!(
        !default_groups.iter().any(|(id, _)| id == "custom"),
        "default-home managed data dir must not see the alt-dir group"
    );

    unsafe {
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match original_rune {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match original_skill {
            Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
            None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
        }
    }
}

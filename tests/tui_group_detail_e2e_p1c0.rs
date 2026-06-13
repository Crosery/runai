//! P1 e2e regression for "Rename Group" feature in the TUI Groups tab.
//!
//! Covers `handle_rename_group_key` (src/tui/app.rs:1085+) and its entry-point
//! 'r' key handler in `handle_normal_key` (src/tui/app.rs:726-732).
//!
//! The feature mutates `~/.runai/groups/<id>.toml` on disk via
//! `SkillManager::rename_group`, so the physical_e2e test is sandboxed with
//! `HOME=tempdir` + `HOME_LOCK` to keep the real `~/.runai/` untouched.
#![cfg(not(target_os = "windows"))]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::group::{Group, GroupKind};
use runai::core::manager::SkillManager;
use runai::tui::app::{App, InputMode, Tab};
use std::sync::Mutex;

/// Integration-test-local HOME lock. Mirrors `runai::test_support::HOME_LOCK`
/// (gated behind `#[cfg(test)]` in the lib crate, so not reachable from
/// integration tests). Tests within this file run with `--test-threads=1` per
/// the CI gate, but we hold the lock anyway so concurrent runs across files
/// can't tread on each other's HOME env var.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn with_home<F: FnOnce()>(tmp: &std::path::Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned: std::sync::PoisonError<std::sync::MutexGuard<'_, ()>>| {
            poisoned.into_inner()
        });
    let original = std::env::var("HOME").ok();
    unsafe {
        std::env::set_var("HOME", tmp);
    }
    f();
    unsafe {
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// Build an App with a single group registered on disk under `<tmp>/data/groups/`.
/// Returns the App pre-loaded on the Groups tab with selected=0.
fn app_with_group(tmp: &std::path::Path, id: &str, name: &str) -> App {
    let mgr = SkillManager::with_base(tmp.join("data")).unwrap();
    let group = Group {
        name: name.into(),
        description: "test description".into(),
        kind: GroupKind::Custom,
        auto_enable: false,
        members: vec![],
    };
    mgr.create_group(id, &group).expect("create group");

    let mut app = App::new(mgr);
    // App::new defaults to FirstLaunch when the DB has zero skills/mcps; the
    // rename-group flow runs in Normal mode, so force-reset before the test.
    app.mode = InputMode::Normal;
    app.reload();
    app.tab = Tab::Groups;
    app.selected = 0;
    app
}

// ─── Test 1: rename_group_prefills_current_name ─────────────────────────────
// [unit_in_module] Press 'r' on Groups tab — enters RenameGroup with input_buf
// pre-filled with the current group name. Typing and Backspace edit input_buf
// without committing.
#[test]
fn rename_group_prefills_current_name() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mut app = app_with_group(tmp.path(), "test-group", "OldName");

        // Sanity precondition.
        assert_eq!(app.visible_groups().len(), 1, "exactly one group visible");
        assert!(matches!(app.mode, InputMode::Normal));

        // Press 'r' on Groups tab → enter rename mode, input_buf seeded with current name.
        app.handle_key(key(KeyCode::Char('r')));

        assert!(
            matches!(app.mode, InputMode::RenameGroup),
            "expected RenameGroup mode after 'r'"
        );
        assert_eq!(
            app.input_buf, "OldName",
            "input_buf must be pre-filled with current group name"
        );

        // User can append characters to extend the name.
        app.handle_key(key(KeyCode::Char('X')));
        assert_eq!(app.input_buf, "OldNameX");

        // Backspace removes one char from input_buf.
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input_buf, "OldName");

        // No commit yet — DB / file should still say OldName.
        let groups = app.mgr.list_groups().expect("list_groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1.name, "OldName");
    });
}

// ─── Test 2: rename_group_updates_toml_and_reloads ──────────────────────────
// [physical_e2e] Enter RenameGroup, clear input via repeated Backspace, type a
// new name, press Enter → ~/.runai/groups/<id>.toml is rewritten with the new
// name, reload() refreshes visible_groups(), id stays unchanged, message
// reports the new name.
#[test]
fn rename_group_updates_toml_and_reloads() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mut app = app_with_group(tmp.path(), "test-group", "OldName");

        let toml_path = app
            .mgr
            .paths()
            .groups_dir()
            .join("test-group.toml");
        assert!(
            toml_path.exists(),
            "precondition: group TOML must exist on disk"
        );
        let before = std::fs::read_to_string(&toml_path).unwrap();
        assert!(
            before.contains("name = \"OldName\""),
            "TOML file must initially carry the old name; got:\n{before}"
        );

        // Press 'r' to enter rename.
        app.handle_key(key(KeyCode::Char('r')));
        assert!(matches!(app.mode, InputMode::RenameGroup));
        assert_eq!(app.input_buf, "OldName");

        // Clear input via backspaces, then type the new name.
        for _ in 0..app.input_buf.len() {
            app.handle_key(key(KeyCode::Backspace));
        }
        assert_eq!(app.input_buf, "");

        for c in "NewName".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input_buf, "NewName");

        // Enter commits — should rewrite TOML, reload, and return to Normal.
        app.handle_key(key(KeyCode::Enter));

        assert!(
            matches!(app.mode, InputMode::Normal),
            "mode returns to Normal after Enter commit"
        );
        assert_eq!(app.input_buf, "", "input_buf is cleared after commit");

        // Physical assertion — file on disk has the new name.
        let after = std::fs::read_to_string(&toml_path)
            .expect("TOML file must still exist at the same path (id unchanged)");
        assert!(
            after.contains("name = \"NewName\""),
            "TOML file must carry the new name after rename; got:\n{after}"
        );
        assert!(
            !after.contains("name = \"OldName\""),
            "TOML file must no longer carry the old name; got:\n{after}"
        );

        // visible_groups() reflects the new name; id unchanged.
        let visible = app.visible_groups();
        assert_eq!(visible.len(), 1, "still exactly one group after rename");
        assert_eq!(visible[0].0, "test-group", "id must be unchanged");
        assert_eq!(visible[0].1, "NewName", "name must be the new value");

        // Status message reports the new name.
        let msg = app.message.clone().unwrap_or_default();
        assert!(
            msg.contains("NewName"),
            "message must reference the new name; got: {msg:?}"
        );
    });
}

// ─── Test 3: rename_group_rejects_empty_name ────────────────────────────────
// [unit_in_module] Backspacing the entire input then pressing Enter must NOT
// update the TOML. Mode returns to Normal, name on disk stays unchanged.
#[test]
fn rename_group_rejects_empty_name() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mut app = app_with_group(tmp.path(), "test-group", "OldName");

        let toml_path = app
            .mgr
            .paths()
            .groups_dir()
            .join("test-group.toml");
        let before = std::fs::read_to_string(&toml_path).unwrap();

        // Enter rename, clear input via backspaces.
        app.handle_key(key(KeyCode::Char('r')));
        assert!(matches!(app.mode, InputMode::RenameGroup));
        assert_eq!(app.input_buf, "OldName");
        for _ in 0..app.input_buf.len() {
            app.handle_key(key(KeyCode::Backspace));
        }
        assert!(
            app.input_buf.trim().is_empty(),
            "input_buf must be empty after clearing"
        );

        // Press Enter on empty input — must NOT commit.
        app.handle_key(key(KeyCode::Enter));

        assert!(
            matches!(app.mode, InputMode::Normal),
            "empty Enter returns to Normal without commit"
        );

        // File on disk unchanged (still OldName).
        let after = std::fs::read_to_string(&toml_path).unwrap();
        assert_eq!(
            before, after,
            "TOML file must be byte-identical when rename was rejected"
        );

        // Visible list still has the old name.
        let visible = app.visible_groups();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].1, "OldName");
    });
}

// ─── Test 4: cancel_rename_discards_changes ─────────────────────────────────
// [unit_in_module] User typed a new name into input_buf; pressing Esc must
// abort: mode returns to Normal, input_buf clears, on-disk TOML unchanged.
#[test]
fn cancel_rename_discards_changes() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mut app = app_with_group(tmp.path(), "test-group", "OldName");

        let toml_path = app
            .mgr
            .paths()
            .groups_dir()
            .join("test-group.toml");
        let before = std::fs::read_to_string(&toml_path).unwrap();

        // Enter rename, replace name with "NewName".
        app.handle_key(key(KeyCode::Char('r')));
        for _ in 0..app.input_buf.len() {
            app.handle_key(key(KeyCode::Backspace));
        }
        for c in "NewName".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input_buf, "NewName");
        assert!(matches!(app.mode, InputMode::RenameGroup));

        // Press Esc — must discard everything.
        app.handle_key(key(KeyCode::Esc));

        assert!(
            matches!(app.mode, InputMode::Normal),
            "Esc returns to Normal mode"
        );
        assert_eq!(app.input_buf, "", "Esc clears input_buf");

        // On-disk TOML must be byte-identical (no write happened).
        let after = std::fs::read_to_string(&toml_path).unwrap();
        assert_eq!(
            before, after,
            "TOML must be untouched after Esc cancel"
        );

        // Visible list still has the old name.
        let visible = app.visible_groups();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].1, "OldName");
    });
}

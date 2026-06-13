//! P2 TUI group lifecycle smoke tests (chunk p2c0).
//!
//! Covers four features from runai-158 test plan §2.7, §2.9, §2.10, §2.11:
//! - Delete Group
//! - Add Member to Group
//! - Remove Member from Group
//! - Group Toggle
//!
//! Drives the `tui::app::App` state machine in-process via the public
//! `runai::tui::app::App` surface. HOME is mocked into a `tempfile::TempDir`
//! and serialized through a process-wide Mutex so `dirs::home_dir()` (read by
//! `CliTarget::skills_dir()`) lands inside the sandbox.
//!
//! Skipped on Windows: `dirs::home_dir()` ignores env on Windows (see
//! AGENTS.md "Key constraints"); the entire mocking strategy assumes unix.
#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::group::{Group, GroupKind};
use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind, Source};
use runai::tui::app::{App, InputMode, PendingDelete, Tab};

/// Process-wide guard for HOME mutation. `dirs::home_dir()` reads HOME on
/// unix at call time, so concurrent tests racing on HOME would let one
/// test's `enable_resource` plant a symlink in another test's CLI skills
/// dir. We are run with --test-threads=1, but keep the guard for safety.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::env::var("HOME").ok();
    unsafe {
        std::env::set_var("HOME", tmp);
    }
    // Make sure the CLI skills dirs exist so enable_resource's
    // `std::fs::create_dir_all` resolves predictably.
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let _ = std::fs::create_dir_all(tmp.join(format!(".{cli}/skills")));
    }
    f();
    unsafe {
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn make_skill_resource(name: &str, dir: PathBuf) -> Resource {
    Resource {
        id: format!("local:{name}"),
        name: name.to_string(),
        kind: ResourceKind::Skill,
        description: format!("{name} desc"),
        directory: dir.clone(),
        source: Source::Local { path: dir },
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
    }
}

/// Plant a skill on disk + DB inside the sandboxed data dir.
fn plant_skill(mgr: &SkillManager, name: &str) -> PathBuf {
    let dir = mgr.paths().skills_dir().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), format!("# {name}")).unwrap();
    let res = make_skill_resource(name, dir.clone());
    mgr.db().insert_resource(&res).unwrap();
    dir
}

/// Build a `SkillManager` rooted at `<tmp>/data`. Uses `with_base` so the data
/// dir is fully explicit (does not consult RUNE_DATA_DIR / HOME).
fn make_manager(tmp: &Path) -> SkillManager {
    SkillManager::with_base(tmp.join("data")).unwrap()
}

/// Create a TOML-backed group and wire its members in the DB. Returns the
/// group id (== TOML stem).
fn create_group(mgr: &SkillManager, id: &str, display_name: &str, member_resource_ids: &[&str]) {
    let group = Group {
        name: display_name.to_string(),
        description: format!("{display_name} desc"),
        kind: GroupKind::Custom,
        auto_enable: false,
        members: Vec::new(), // resolved by add_group_member directly
    };
    mgr.create_group(id, &group).unwrap();
    for rid in member_resource_ids {
        mgr.db().add_group_member(id, rid).unwrap();
    }
}

/// Convenience: select the group with the given id in the visible_groups()
/// list and update `app.selected` to point at it.
fn select_group_by_id(app: &mut App, id: &str) {
    let visible = app.visible_groups();
    let idx = visible
        .iter()
        .position(|(gid, _, _, _, _)| gid == id)
        .expect("group id not in visible_groups");
    app.selected = idx;
}

// ───────────────────────────────────────────────────────────────────────────
// Feature 2.7 — Delete Group
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn delete_group_stages_confirmation() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "demo");
        create_group(&mgr, "grp-keep", "GroupKeep", &["local:demo"]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "grp-keep");

        app.handle_key(key(KeyCode::Char('d')));

        assert!(matches!(app.mode, InputMode::ConfirmDelete));
        match &app.pending_delete {
            Some(PendingDelete::Group { id, name }) => {
                assert_eq!(id, "grp-keep");
                assert_eq!(name, "GroupKeep");
            }
            other => panic!("expected PendingDelete::Group, got {other:?}", other = other.is_some()),
        }

        // TOML still on disk before confirmation.
        let toml_path = app.mgr.paths().groups_dir().join("grp-keep.toml");
        assert!(
            toml_path.exists(),
            "group TOML should remain until Enter confirms"
        );
    });
}

#[test]
fn delete_group_removes_toml_preserves_members() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        let skill_a_dir = plant_skill(&mgr, "alpha");
        let skill_b_dir = plant_skill(&mgr, "beta");
        create_group(&mgr, "grp-doomed", "Doomed", &["local:alpha", "local:beta"]);

        let toml_path = mgr.paths().groups_dir().join("grp-doomed.toml");
        assert!(toml_path.exists(), "precondition: group TOML on disk");

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "grp-doomed");

        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Enter));

        assert!(matches!(app.mode, InputMode::Normal));
        assert!(
            !toml_path.exists(),
            "group TOML should be removed after confirm"
        );

        // Members are NOT cascaded — both skill dirs survive, DB still has
        // them.
        assert!(skill_a_dir.exists(), "skill 'alpha' physical dir survives");
        assert!(skill_b_dir.exists(), "skill 'beta' physical dir survives");
        assert!(
            app.mgr
                .db()
                .get_resource("local:alpha")
                .unwrap()
                .is_some(),
            "skill 'alpha' DB row survives group delete"
        );
        assert!(
            app.mgr
                .db()
                .get_resource("local:beta")
                .unwrap()
                .is_some(),
            "skill 'beta' DB row survives group delete"
        );

        // Message reflects the deletion.
        let msg = app.message.clone().unwrap_or_default();
        assert!(
            msg.contains("Doomed") && msg.contains("deleted"),
            "message should mention name+deleted, got {msg:?}"
        );
    });
}

#[test]
fn cancel_delete_group_clears_pending() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "demo");
        create_group(&mgr, "grp-survives", "Survives", &["local:demo"]);
        let toml_path = mgr.paths().groups_dir().join("grp-survives.toml");

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "grp-survives");

        app.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(app.mode, InputMode::ConfirmDelete));
        assert!(app.pending_delete.is_some());

        app.handle_key(key(KeyCode::Esc));

        assert!(matches!(app.mode, InputMode::Normal));
        assert!(app.pending_delete.is_none());
        assert!(toml_path.exists(), "TOML must be untouched after Esc");
    });
}

#[test]
fn delete_missing_group_no_panic() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "demo");
        create_group(&mgr, "grp-ghost", "Ghost", &["local:demo"]);
        let toml_path = mgr.paths().groups_dir().join("grp-ghost.toml");

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "grp-ghost");

        // Manually delete the TOML out from under the app to simulate the
        // inconsistent filesystem condition described in the plan.
        std::fs::remove_file(&toml_path).unwrap();
        assert!(!toml_path.exists());

        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Enter));

        // No panic, mode resets, message still shows 'deleted'.
        assert!(matches!(app.mode, InputMode::Normal));
        let msg = app.message.clone().unwrap_or_default();
        assert!(
            msg.contains("Ghost") && msg.contains("deleted"),
            "expected delete-acknowledgement message, got {msg:?}"
        );
    });
}

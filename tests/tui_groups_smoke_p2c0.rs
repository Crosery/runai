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

// ───────────────────────────────────────────────────────────────────────────
// Feature 2.9 — Add Member to Group
// ───────────────────────────────────────────────────────────────────────────

/// Plant a disabled MCP by writing a canonical JSON backup file into the
/// managed `mcps/` dir. `SkillManager::list_resources(Some(Mcp), _)` reads
/// MCPs from the active CLI configs first, then sweeps this dir for any
/// that the user has disabled — exactly the "available but disabled"
/// shape we need for the pick-list test. Returns the synthetic resource
/// id (`mcp:<name>`).
fn plant_mcp(mgr: &SkillManager, name: &str) -> String {
    let mcps_dir = mgr.paths().mcps_dir();
    std::fs::create_dir_all(&mcps_dir).unwrap();
    let json = serde_json::json!({
        "command": "echo",
        "args": ["hello"],
    });
    std::fs::write(
        mcps_dir.join(format!("{name}.json")),
        serde_json::to_string_pretty(&json).unwrap(),
    )
    .unwrap();
    format!("mcp:{name}")
}

/// Drive `app` from Normal/Groups → GroupDetail by pressing Enter on the
/// currently-selected group row.
fn open_group_detail(app: &mut App) {
    app.handle_key(key(KeyCode::Enter));
    assert!(matches!(app.mode, InputMode::GroupDetail));
}

#[test]
fn add_member_lists_available_skills() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "skill1");
        plant_skill(&mgr, "skill2");
        plant_skill(&mgr, "skill3");
        create_group(&mgr, "g1", "G1", &["local:skill1"]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);

        app.handle_key(key(KeyCode::Char('a')));

        assert!(matches!(app.mode, InputMode::PickSkillForGroup));
        let names: Vec<&str> = app.pick_items.iter().map(|r| r.name.as_str()).collect();
        assert!(
            !names.contains(&"skill1"),
            "skill1 is already a member, must be filtered out"
        );
        assert!(names.contains(&"skill2"));
        assert!(names.contains(&"skill3"));
        assert_eq!(app.pick_idx, 0, "cursor resets to top of list");
    });
}

#[test]
fn pick_toggle_skill_mcp_with_tab() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "skill-a");
        plant_skill(&mgr, "skill-b");
        plant_mcp(&mgr, "mcp-x");
        plant_mcp(&mgr, "mcp-y");
        create_group(&mgr, "g1", "G1", &[]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);

        // Default: Skills view.
        app.handle_key(key(KeyCode::Char('a')));
        assert!(!app.pick_show_mcp);
        assert!(app.pick_items.iter().all(|r| r.kind == ResourceKind::Skill));
        let skill_kinds: usize = app.pick_items.len();
        assert!(skill_kinds >= 2, "should see both skills");

        // Tab toggles to MCPs.
        app.handle_key(key(KeyCode::Tab));
        assert!(app.pick_show_mcp);
        assert!(app.pick_items.iter().all(|r| r.kind == ResourceKind::Mcp));
        assert!(app.pick_items.len() >= 2, "should see both mcps");

        // Tab again toggles back.
        app.handle_key(key(KeyCode::Tab));
        assert!(!app.pick_show_mcp);
        assert!(app.pick_items.iter().all(|r| r.kind == ResourceKind::Skill));
    });
}

#[test]
fn confirm_add_member() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &[]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);
        app.handle_key(key(KeyCode::Char('a')));

        // Pick the second item.
        let visible_before: Vec<String> = app
            .visible_pick_items()
            .iter()
            .map(|r| r.id.clone())
            .collect();
        assert!(visible_before.len() >= 2);
        let target_id = visible_before[1].clone();
        app.pick_idx = 1;

        app.handle_key(key(KeyCode::Enter));

        // DB now lists the picked id as a member.
        let member_ids = app
            .mgr
            .db()
            .get_group_member_ids("g1")
            .unwrap_or_default();
        assert!(
            member_ids.contains(&target_id),
            "expected DB to record member {target_id}, got {member_ids:?}"
        );

        // pick_items no longer contains the added one.
        assert!(
            app.pick_items.iter().all(|r| r.id != target_id),
            "picked item should be removed from pick_items"
        );

        // detail_members was reloaded.
        assert!(
            app.detail_members.iter().any(|r| r.id == target_id),
            "detail_members should reflect the newly added member"
        );

        // Message acknowledges add.
        let msg = app.message.clone().unwrap_or_default();
        assert!(
            msg.to_lowercase().contains("added") || msg.contains("Added"),
            "expected an 'Added' message, got {msg:?}"
        );
    });
}

#[test]
fn add_member_persists_in_db() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &[]);

        // Use the App to add 'alpha' (or whichever lands at idx 0).
        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);
        app.handle_key(key(KeyCode::Char('a')));

        let added_id = app
            .visible_pick_items()
            .first()
            .map(|r| r.id.clone())
            .expect("at least one pick item");
        app.pick_idx = 0;
        app.handle_key(key(KeyCode::Enter));

        // Drop the App, build a brand-new SkillManager on the same data dir.
        // This proves the add isn't memory-only.
        drop(app);
        let mgr2 = make_manager(tmp.path());
        let member_ids = mgr2.db().get_group_member_ids("g1").unwrap_or_default();
        assert!(
            member_ids.contains(&added_id),
            "membership must survive a manager reload, got {member_ids:?}"
        );

        // A fresh App reads the same row through get_group_members.
        let mut app2 = App::new(mgr2);
        app2.tab = Tab::Groups;
        app2.reload();
        select_group_by_id(&mut app2, "g1");
        open_group_detail(&mut app2);
        assert!(
            app2.detail_members.iter().any(|r| r.id == added_id),
            "fresh App should also see member {added_id}"
        );
    });
}

#[test]
fn cancel_add_member() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &["local:alpha"]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);

        let detail_members_before: Vec<String> =
            app.detail_members.iter().map(|r| r.id.clone()).collect();

        app.handle_key(key(KeyCode::Char('a')));
        assert!(matches!(app.mode, InputMode::PickSkillForGroup));

        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, InputMode::GroupDetail));

        let detail_members_after: Vec<String> =
            app.detail_members.iter().map(|r| r.id.clone()).collect();
        assert_eq!(
            detail_members_before, detail_members_after,
            "detail_members must be unchanged after cancelling add"
        );
    });
}

#[test]
fn pick_search_filters_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        plant_skill(&mgr, "gamma");
        create_group(&mgr, "g1", "G1", &[]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);
        app.handle_key(key(KeyCode::Char('a')));

        // Initially all three skills are visible.
        let initial: Vec<String> = app
            .visible_pick_items()
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert!(
            initial.contains(&"alpha".to_string())
                && initial.contains(&"beta".to_string())
                && initial.contains(&"gamma".to_string())
        );

        // Typing 'b' narrows to beta.
        app.handle_key(key(KeyCode::Char('b')));
        let just_b: Vec<String> = app
            .visible_pick_items()
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(just_b, vec!["beta"], "after 'b' only beta visible");

        // 'B' (uppercase) on top of 'b' shouldn't matter — filter is
        // case-insensitive. Pop the lowercase 'b', try 'B'.
        app.handle_key(key(KeyCode::Backspace));
        assert!(app.pick_search.is_empty());
        app.handle_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        let just_caps_b: Vec<String> = app
            .visible_pick_items()
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(
            just_caps_b,
            vec!["beta"],
            "case-insensitive: 'B' also matches beta"
        );

        // Backspace clears, all three return.
        app.handle_key(key(KeyCode::Backspace));
        let after_clear: Vec<String> = app
            .visible_pick_items()
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(
            after_clear.len(),
            3,
            "all three skills visible after backspace clears search"
        );
    });
}

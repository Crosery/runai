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
use runai::core::cli_target::CliTarget;
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
            other => panic!(
                "expected PendingDelete::Group, got {other:?}",
                other = other.is_some()
            ),
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
            app.mgr.db().get_resource("local:alpha").unwrap().is_some(),
            "skill 'alpha' DB row survives group delete"
        );
        assert!(
            app.mgr.db().get_resource("local:beta").unwrap().is_some(),
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
        let member_ids = app.mgr.db().get_group_member_ids("g1").unwrap_or_default();
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

// ───────────────────────────────────────────────────────────────────────────
// Feature 2.10 — Remove Member from Group
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn remove_member_stages_confirmation() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &["local:alpha", "local:beta"]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);

        // detail_idx = 0 → some member is selected.
        assert!(
            !app.detail_members.is_empty(),
            "precondition: group has members"
        );
        let target = app.detail_members[0].clone();

        app.handle_key(key(KeyCode::Char('d')));

        assert!(matches!(app.mode, InputMode::ConfirmDelete));
        match &app.pending_delete {
            Some(PendingDelete::GroupMember {
                group_id,
                group_name,
                resource_id,
                resource_name,
            }) => {
                assert_eq!(group_id, "g1");
                assert_eq!(group_name, "G1");
                assert_eq!(resource_id, &target.id);
                assert_eq!(resource_name, &target.name);
            }
            other => panic!(
                "expected PendingDelete::GroupMember, got present={}",
                other.is_some()
            ),
        }

        // DB still has the member (nothing committed yet).
        let still_there = app.mgr.db().get_group_member_ids("g1").unwrap_or_default();
        assert!(still_there.contains(&target.id));
    });
}

#[test]
fn remove_member_deletes_db_row_preserves_resource() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        let alpha_dir = plant_skill(&mgr, "alpha");
        let beta_dir = plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &["local:alpha", "local:beta"]);

        let before = mgr.db().get_group_member_ids("g1").unwrap_or_default();
        assert_eq!(before.len(), 2, "precondition: 2 members in g1");

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);

        // Remove the first member (whichever ordering get_group_members
        // returns — we only need to know which one to assert on).
        let removed = app.detail_members[0].clone();
        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Enter));

        // DB no longer carries the removed member.
        let after = app.mgr.db().get_group_member_ids("g1").unwrap_or_default();
        assert!(
            !after.contains(&removed.id),
            "removed member {0} should not be in g1 anymore, after={after:?}",
            removed.id
        );
        assert_eq!(after.len(), 1, "g1 should now have exactly 1 member");

        // detail_members reloaded — exactly 1 entry remains.
        assert_eq!(
            app.detail_members.len(),
            1,
            "detail_members should reload to the remaining 1 member"
        );

        // Message reflects removal.
        let msg = app.message.clone().unwrap_or_default();
        assert!(
            msg.contains(&removed.name) && msg.to_lowercase().contains("remov"),
            "message should mention removed name, got {msg:?}"
        );

        // The skill resources themselves survive — neither dir nor DB row
        // was touched.
        assert!(alpha_dir.exists(), "alpha skill dir preserved");
        assert!(beta_dir.exists(), "beta skill dir preserved");
        assert!(
            app.mgr.db().get_resource("local:alpha").unwrap().is_some(),
            "alpha resource DB row preserved"
        );
        assert!(
            app.mgr.db().get_resource("local:beta").unwrap().is_some(),
            "beta resource DB row preserved"
        );
    });
}

#[test]
fn cancel_remove_member() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &["local:alpha", "local:beta"]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);

        let before_ids: Vec<String> = app.detail_members.iter().map(|r| r.id.clone()).collect();
        assert_eq!(before_ids.len(), 2);

        app.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(app.mode, InputMode::ConfirmDelete));

        app.handle_key(key(KeyCode::Esc));

        // Per `PendingDelete::return_mode`, cancelling a GroupMember
        // confirmation must return to the GroupDetail overlay, not Normal.
        assert!(matches!(app.mode, InputMode::GroupDetail));
        assert!(app.pending_delete.is_none());

        // detail_members untouched: 2 members still listed in the same
        // order.
        let after_ids: Vec<String> = app.detail_members.iter().map(|r| r.id.clone()).collect();
        assert_eq!(before_ids, after_ids);

        // DB still has both.
        let db_ids = app.mgr.db().get_group_member_ids("g1").unwrap();
        assert_eq!(db_ids.len(), 2);
    });
}

#[test]
fn remove_member_clamps_cursor() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &["local:alpha", "local:beta"]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);

        // Move cursor to the LAST member (idx=1).
        assert_eq!(app.detail_members.len(), 2);
        app.detail_idx = 1;
        assert_eq!(app.detail_idx, 1);

        // Remove it.
        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Enter));

        // After remove: list shrinks to 1, cursor was at 1 (= len of new
        // list) and must be clamped to 0 (= len - 1) so it does not point
        // off the end.
        assert_eq!(app.detail_members.len(), 1);
        assert_eq!(
            app.detail_idx, 0,
            "cursor should clamp to detail_members.len() - 1 after removal"
        );

        // Sanity: subsequent navigation does not panic.
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.detail_idx, 0);
    });
}

// ───────────────────────────────────────────────────────────────────────────
// Feature 2.11 — Group Toggle
// ───────────────────────────────────────────────────────────────────────────

fn claude_link_path(home: &Path, skill_name: &str) -> PathBuf {
    home.join(".claude").join("skills").join(skill_name)
}

fn codex_link_path(home: &Path, skill_name: &str) -> PathBuf {
    home.join(".codex").join("skills").join(skill_name)
}

/// True if a symlink (possibly dangling) exists at `path` — matches
/// `Linker::is_symlink`'s semantics (symlink_metadata).
fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

#[test]
fn toggle_group_enables_all_if_partial() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &["local:alpha", "local:beta"]);

        // Pre-state: enable 'alpha' only on Claude (partial coverage).
        mgr.enable_resource("local:alpha", CliTarget::Claude, None)
            .unwrap();
        assert!(is_symlink(&claude_link_path(tmp.path(), "alpha")));
        assert!(!is_symlink(&claude_link_path(tmp.path(), "beta")));

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();
        select_group_by_id(&mut app, "g1");

        // visible group row reports 1/2 enabled.
        let row = app.visible_groups()[app.selected].clone();
        assert_eq!(row.2, 2, "total members = 2");
        assert_eq!(row.3, 1, "enabled count = 1 (partial)");

        // Toggle. On the Groups tab, `Enter` opens the detail overlay, so
        // Space is the toggle binding (see normal_key handler).
        app.handle_key(key(KeyCode::Char(' ')));

        // Both symlinks now exist.
        assert!(
            is_symlink(&claude_link_path(tmp.path(), "alpha")),
            "alpha symlink must exist after toggle"
        );
        assert!(
            is_symlink(&claude_link_path(tmp.path(), "beta")),
            "beta symlink must exist after toggle (partial → full)"
        );

        // reload() ran inside toggle; row now reports 2/2.
        let after = app.visible_groups()[app.selected].clone();
        assert_eq!(after.3, 2, "enabled count = 2 after partial→full toggle");
    });
}

#[test]
fn toggle_group_disables_all_if_full() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g1", "G1", &["local:alpha", "local:beta"]);

        // Pre-state: both members enabled.
        mgr.enable_resource("local:alpha", CliTarget::Claude, None)
            .unwrap();
        mgr.enable_resource("local:beta", CliTarget::Claude, None)
            .unwrap();

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();
        select_group_by_id(&mut app, "g1");
        let row = app.visible_groups()[app.selected].clone();
        assert_eq!(row.3, 2, "precondition: full coverage 2/2");

        // Toggle → disable all. Space is the toggle binding on Groups tab.
        app.handle_key(key(KeyCode::Char(' ')));

        assert!(
            !is_symlink(&claude_link_path(tmp.path(), "alpha")),
            "alpha symlink should be removed"
        );
        assert!(
            !is_symlink(&claude_link_path(tmp.path(), "beta")),
            "beta symlink should be removed"
        );
        let after = app.visible_groups()[app.selected].clone();
        assert_eq!(after.3, 0, "enabled count = 0 after full→empty toggle");

        // Idempotent toggle round-trip: pressing Space again must
        // re-enable both (0/2 → partial branch enables) — confirms the
        // state machine is not stuck.
        app.handle_key(key(KeyCode::Char(' ')));
        let after2 = app.visible_groups()[app.selected].clone();
        assert_eq!(after2.3, 2, "second toggle restores 2/2");
    });
}

#[test]
fn toggle_group_per_target() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        create_group(&mgr, "g1", "G1", &["local:alpha"]);

        // Enable for Claude only.
        mgr.enable_resource("local:alpha", CliTarget::Claude, None)
            .unwrap();
        assert!(is_symlink(&claude_link_path(tmp.path(), "alpha")));
        assert!(!is_symlink(&codex_link_path(tmp.path(), "alpha")));

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();
        select_group_by_id(&mut app, "g1");

        // Switch to Codex. group toggle should now operate on Codex only.
        app.handle_key(key(KeyCode::Char('2')));
        assert_eq!(app.active_target, CliTarget::Codex);
        // Codex view: 0/1 enabled → partial branch → enable. Space binds
        // to toggle on the Groups tab (Enter would open the detail view).
        app.handle_key(key(KeyCode::Char(' ')));

        // Codex now has a symlink, Claude unchanged.
        assert!(
            is_symlink(&codex_link_path(tmp.path(), "alpha")),
            "alpha must be linked into .codex/skills after toggle on Codex"
        );
        assert!(
            is_symlink(&claude_link_path(tmp.path(), "alpha")),
            "alpha must still be linked into .claude/skills (unaffected)"
        );
    });
}

#[test]
fn toggle_member_in_detail_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        create_group(&mgr, "g1", "G1", &["local:alpha"]);

        // Enable alpha for Claude.
        mgr.enable_resource("local:alpha", CliTarget::Claude, None)
            .unwrap();
        assert!(is_symlink(&claude_link_path(tmp.path(), "alpha")));

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();
        select_group_by_id(&mut app, "g1");
        open_group_detail(&mut app);

        assert_eq!(app.detail_members.len(), 1);
        assert!(
            app.detail_members[0].is_enabled_for(CliTarget::Claude),
            "precondition: member enabled for Claude"
        );

        // Press Enter to toggle member → should DISABLE.
        app.handle_key(key(KeyCode::Enter));
        assert!(
            !is_symlink(&claude_link_path(tmp.path(), "alpha")),
            "symlink should be removed after toggle"
        );
        assert!(
            !app.detail_members[0].is_enabled_for(CliTarget::Claude),
            "detail_members must reflect post-toggle state (disabled)"
        );

        // Toggle again → re-enable.
        app.handle_key(key(KeyCode::Enter));
        assert!(
            is_symlink(&claude_link_path(tmp.path(), "alpha")),
            "symlink should be recreated after second toggle"
        );
        assert!(
            app.detail_members[0].is_enabled_for(CliTarget::Claude),
            "detail_members must reflect re-enable"
        );
    });
}

#[test]
fn toggle_group_mixed_kinds() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        // 1 skill + 1 MCP (registered via canonical backup).
        plant_skill(&mgr, "alpha");
        let mcp_id = plant_mcp(&mgr, "echo-mcp");
        // Wire the group: skill ID + the synthetic 'mcp:echo-mcp' id (the
        // App resolves MCP membership through `mcp:` prefix at toggle
        // time via `restore_mcp` / `remove_mcp`).
        create_group(&mgr, "g1", "G1", &[]);
        mgr.db().add_group_member("g1", "local:alpha").unwrap();
        mgr.db().add_group_member("g1", &mcp_id).unwrap();

        // Pre-state: skill enabled for Claude; MCP not in any CLI config
        // (its backup is in mcps/ → disabled state).
        mgr.enable_resource("local:alpha", CliTarget::Claude, None)
            .unwrap();
        assert!(is_symlink(&claude_link_path(tmp.path(), "alpha")));

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();
        select_group_by_id(&mut app, "g1");

        let row = app.visible_groups()[app.selected].clone();
        assert_eq!(row.2, 2, "group has 2 members (1 skill + 1 mcp)");
        // Partial coverage (skill enabled, mcp disabled) → toggle should
        // ENABLE all.
        assert!(
            row.3 < row.2,
            "partial coverage triggers full-enable branch"
        );

        // Space, not Enter — Enter opens detail; Space toggles on Groups.
        app.handle_key(key(KeyCode::Char(' ')));

        // Skill symlink still there (was already enabled, toggle re-enables
        // it via `create_link_force` → idempotent).
        assert!(
            is_symlink(&claude_link_path(tmp.path(), "alpha")),
            "skill symlink survives toggle"
        );

        // No panic; group toggle handled both kinds without erroring out.
        // We can't easily assert the MCP entry landed in ~/.claude.json
        // without spinning up the full mcp_register pipeline, but the
        // critical regression-target here is "toggle does not crash on
        // mixed kinds", which we just exercised.
    });
}

#[test]
fn toggle_group_respects_rune_data_dir() {
    // Cross-data-dir dual-run: each `SkillManager::with_base(tmp.join("dataN"))`
    // owns its own DB and groups TOML, and `with_home` mocks HOME so the
    // CLI link path is sandboxed. We assert that toggling a group in
    // data_dir_1 lands its symlink **only** in HOME_1/.claude/skills/, and
    // never crosses into HOME_2's filesystem space.
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();

    // --- Run 1: data_dir 1 + HOME 1 ---
    with_home(tmp1.path(), || {
        let mgr = SkillManager::with_base(tmp1.path().join("data1")).unwrap();
        plant_skill(&mgr, "alpha");
        create_group(&mgr, "g1", "G1", &["local:alpha"]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();
        select_group_by_id(&mut app, "g1");
        // Space, not Enter — Enter opens detail; Space toggles on Groups.
        app.handle_key(key(KeyCode::Char(' ')));

        assert!(
            is_symlink(&claude_link_path(tmp1.path(), "alpha")),
            "run1: symlink must land inside tmp1's HOME"
        );
    });

    // --- Run 2: data_dir 2 + HOME 2 ---
    with_home(tmp2.path(), || {
        let mgr = SkillManager::with_base(tmp2.path().join("data2")).unwrap();
        plant_skill(&mgr, "beta");
        create_group(&mgr, "g2", "G2", &["local:beta"]);

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();
        select_group_by_id(&mut app, "g2");
        app.handle_key(key(KeyCode::Char(' ')));

        assert!(
            is_symlink(&claude_link_path(tmp2.path(), "beta")),
            "run2: symlink must land inside tmp2's HOME"
        );

        // Run-1's HOME must NOT have grown a 'beta' symlink because of
        // run-2 — i.e. no cross-data-dir bleed.
        assert!(
            !is_symlink(&claude_link_path(tmp1.path(), "beta")),
            "run2's toggle must not pollute run1's HOME"
        );
        assert!(
            !is_symlink(&claude_link_path(tmp2.path(), "alpha")),
            "run1's skill must not appear in run2's HOME"
        );
    });

    // Confirm post-condition outside both with_home guards: run1's symlink
    // for alpha still lives only in tmp1.
    assert!(
        is_symlink(&claude_link_path(tmp1.path(), "alpha")),
        "tmp1's alpha symlink must still exist after run2"
    );
    assert!(
        !is_symlink(&claude_link_path(tmp2.path(), "alpha")),
        "tmp2 must never have grown alpha"
    );
}

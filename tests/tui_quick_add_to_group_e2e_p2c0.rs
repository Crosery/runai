//! P2 regression tests for TUI quick "Add to Group" flow (plan §2.33).
//!
//! Drives `runai::tui::app::App` directly through `handle_key` to exercise
//! the `'a'` keybinding on Skills tab: opens AddToGroup mode, navigates with
//! j/k, commits with Enter (writing `group_members` row), cancels with Esc.
//!
//! All tests sandbox via `HOME=<tempdir>` + `RUNE_DATA_DIR=<tempdir>/.runai`
//! so they never touch the real `~/.runai/` tree (safety contract §1).
//!
//! Skipped on Windows: the same constraint as `manager::tests` — symlink
//! creation and `dirs::home_dir()` env honoring are unix-only here.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::group::{Group, GroupKind};
use runai::core::manager::SkillManager;
use runai::tui::app::{App, InputMode, Tab};
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

/// Process-wide HOME serialization lock. Integration tests cannot import
/// `crate::test_support::HOME_LOCK` (cfg(test) only), so we keep a local
/// equivalent here.
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with HOME pointed at `tmp`. Restores prior value on drop.
fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let prior_home = std::env::var("HOME").ok();
    let prior_data = std::env::var("RUNE_DATA_DIR").ok();
    // Ensure `dirs::home_dir()` and any HOME-rooted resolver see the temp.
    unsafe {
        std::env::set_var("HOME", tmp);
        // RUNE_DATA_DIR also pointed at tmp/.runai so any code path that
        // bypasses `with_base` still lands inside the sandbox.
        std::env::set_var("RUNE_DATA_DIR", tmp.join(".runai"));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match prior_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match prior_data {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
    }
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Pre-create the four CLI skills directories so any incidental scan path
/// has somewhere to look at (even though these tests don't trigger scan).
fn seed_cli_dirs(home: &Path) {
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(home.join(format!(".{cli}/skills"))).ok();
    }
    std::fs::create_dir_all(home.join(".runai/skills")).ok();
}

/// Build a `SkillManager` against `<home>/.runai`.
fn make_manager(home: &Path) -> SkillManager {
    let base = home.join(".runai");
    SkillManager::with_base(base).expect("manager init")
}

/// Plant a local skill on disk under `<sm_data>/skills/<name>/SKILL.md` and
/// register it through the manager. Returns the resource id.
fn plant_skill(mgr: &SkillManager, name: &str) -> String {
    let skill_dir: PathBuf = mgr.paths().data_dir().join("skills").join(name);
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(skill_dir.join("SKILL.md"), format!("# {name}\n")).expect("write SKILL.md");
    mgr.register_local_skill(name).expect("register local skill");
    mgr.find_resource_id(name).expect("resource id present")
}

fn fresh_group(name: &str, description: &str) -> Group {
    Group {
        name: name.to_string(),
        description: description.to_string(),
        kind: GroupKind::Custom,
        auto_enable: false,
        members: vec![],
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

// ─── test 1: physical_e2e — Enter writes the group_members row + reload ──────

#[test]
fn add_to_group_updates_db_and_reloads() {
    let tmp = TempDir::new().expect("tempdir");
    with_home(tmp.path(), || {
        seed_cli_dirs(tmp.path());
        let mgr = make_manager(tmp.path());

        // Seed two groups (alphabetical order: group-a, group-b) so that
        // pressing 'j' once advances from group-a to group-b deterministically.
        mgr.create_group("group-a", &fresh_group("group-a", "alpha"))
            .expect("create group-a");
        mgr.create_group("group-b", &fresh_group("group-b", "beta"))
            .expect("create group-b");

        // Plant one local skill and let the App reload from disk.
        let rid = plant_skill(&mgr, "skill-1");

        let mut app = App::new(mgr);
        app.tab = Tab::Skills;
        app.reload();

        // Sanity: groups list populated and skill-1 visible.
        assert_eq!(app.groups.len(), 2, "two groups should be loaded");
        assert!(
            app.visible_count() > 0,
            "skill-1 must be visible in Skills tab"
        );

        // Skills are sorted by usage_count desc then by name. Find the index
        // of skill-1 in the visible_items() projection and select it.
        let target_idx = app
            .visible_items()
            .iter()
            .position(|r| r.name == "skill-1")
            .expect("skill-1 must appear in visible items");
        app.selected = target_idx;

        // Press 'a' → AddToGroup mode, group_pick_idx initialized to 0.
        app.handle_key(key(KeyCode::Char('a')));
        assert!(
            matches!(app.mode, InputMode::AddToGroup),
            "'a' on Skills tab with groups + visible item enters AddToGroup mode"
        );
        assert_eq!(app.group_pick_idx, 0, "picker resets to first group");

        // Capture the chosen group id at index 0 (likely group-a) and the
        // expected destination after one 'j' (likely group-b). The handler
        // sorts by list_groups() — we just read the actual id at index 1 to
        // stay robust against ordering changes.
        let initial_gid = app.groups[0].0.clone();
        let after_j_gid = app.groups[1].0.clone();
        assert_ne!(
            initial_gid, after_j_gid,
            "two distinct groups must be addressable"
        );

        // Press 'j' → group_pick_idx advances to 1.
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.group_pick_idx, 1, "'j' advances picker to next group");

        // Press Enter → add_group_member(after_j_gid, rid) gets called,
        // reload() runs, mode returns to Normal.
        app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "Enter commits and returns to Normal mode"
        );

        // DB assertion: the resource is now a member of the chosen group.
        let groups_for_skill = app
            .mgr
            .db()
            .get_groups_for_resource(&rid)
            .expect("get_groups_for_resource");
        assert!(
            groups_for_skill.contains(&after_j_gid),
            "resource {rid} should be a member of {after_j_gid} after Enter (got {groups_for_skill:?})"
        );
        assert!(
            !groups_for_skill.contains(&initial_gid),
            "we navigated past {initial_gid}; it should not be a member"
        );

        // group_members table reflects the new row.
        let members = app
            .mgr
            .db()
            .get_group_members(&after_j_gid)
            .expect("get_group_members");
        assert_eq!(members.len(), 1, "exactly one member in the chosen group");
        assert_eq!(members[0].name, "skill-1");

        // reload() ran: the App's groups list shows the new member count = 1
        // for the chosen group.
        let after_row = app
            .groups
            .iter()
            .find(|(id, _, _, _, _)| id == &after_j_gid)
            .expect("chosen group still present in app.groups");
        assert_eq!(
            after_row.2, 1,
            "post-reload member count for chosen group should be 1"
        );
    });
}

// ─── test 2: unit_in_module — no groups → hint, mode stays Normal ────────────

#[test]
fn add_to_group_no_groups_shows_hint() {
    let tmp = TempDir::new().expect("tempdir");
    with_home(tmp.path(), || {
        seed_cli_dirs(tmp.path());
        let mgr = make_manager(tmp.path());

        // Plant a skill so visible_count() > 0 — that way we isolate the
        // "no groups" branch from the "no visible items" branch.
        let _rid = plant_skill(&mgr, "skill-1");

        let mut app = App::new(mgr);
        app.tab = Tab::Skills;
        app.reload();

        assert!(app.groups.is_empty(), "precondition: zero groups");
        assert!(
            app.visible_count() > 0,
            "precondition: at least one visible skill"
        );

        app.message = None;
        app.handle_key(key(KeyCode::Char('a')));

        assert!(
            matches!(app.mode, InputMode::Normal),
            "no groups: mode must remain Normal (not enter AddToGroup)"
        );
        let msg = app
            .message
            .as_deref()
            .expect("hint message must be set when groups list is empty");
        assert!(
            msg.contains("No groups yet") && msg.contains('c'),
            "message should hint pressing 'c' to create a group, got {msg:?}"
        );

        // DB sanity: no group_members rows materialized.
        let count_via_groups: Vec<String> = app
            .mgr
            .db()
            .get_groups_for_resource(&app.visible_items()[0].id.clone())
            .expect("get_groups_for_resource");
        assert!(
            count_via_groups.is_empty(),
            "no group memberships should exist when no groups exist"
        );
    });
}

// ─── test 3: unit_in_module — Esc cancels with no side effect ────────────────

#[test]
fn add_to_group_cancel_via_esc() {
    let tmp = TempDir::new().expect("tempdir");
    with_home(tmp.path(), || {
        seed_cli_dirs(tmp.path());
        let mgr = make_manager(tmp.path());

        mgr.create_group("group-a", &fresh_group("group-a", "alpha"))
            .expect("create group-a");

        let rid = plant_skill(&mgr, "skill-1");

        let mut app = App::new(mgr);
        app.tab = Tab::Skills;
        app.reload();
        // Find and select skill-1.
        let idx = app
            .visible_items()
            .iter()
            .position(|r| r.name == "skill-1")
            .expect("skill-1 must appear in visible items");
        app.selected = idx;

        // Snapshot of group membership BEFORE entering AddToGroup.
        let before = app
            .mgr
            .db()
            .get_groups_for_resource(&rid)
            .expect("get_groups_for_resource before");
        assert!(
            before.is_empty(),
            "skill-1 must not be in any group before test"
        );

        // 'a' → AddToGroup, then Esc → back to Normal, no DB change.
        app.handle_key(key(KeyCode::Char('a')));
        assert!(
            matches!(app.mode, InputMode::AddToGroup),
            "'a' should enter AddToGroup with groups present"
        );

        app.handle_key(key(KeyCode::Esc));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "Esc must return mode to Normal"
        );

        // DB unchanged.
        let after = app
            .mgr
            .db()
            .get_groups_for_resource(&rid)
            .expect("get_groups_for_resource after");
        assert!(
            after.is_empty(),
            "Esc must NOT add the resource to any group (got {after:?})"
        );

        // And group-a still has zero members.
        let members = app
            .mgr
            .db()
            .get_group_members("group-a")
            .expect("get_group_members");
        assert!(
            members.is_empty(),
            "group-a must remain empty after Esc cancel"
        );
    });
}

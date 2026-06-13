//! TUI smoke E2E tests for PLANNING test plan §2.1, §2.4, §2.8
//! (Chunk 0: Skills List & Filter, MCPs List & Filter, Groups List & Search,
//! Group Detail Overlay).
//!
//! These run as integration tests against the runai library API
//! (`crate::core::manager::SkillManager` + `crate::tui::app::App`). Each test
//! constructs an isolated TempDir HOME + data_dir so production
//! `~/.runai/` / `~/.{claude,codex,gemini,opencode}/` are never touched.
//!
//! HOME mutation is global to the process, so all tests in this file
//! serialize on `home_lock()`. Windows is skipped because `dirs::home_dir()`
//! ignores HOME there (see `AGENTS.md` Key constraints).

#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

use runai::core::cli_target::CliTarget;
use runai::core::group::{Group, GroupKind};
use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind, Source};
use runai::tui::app::{App, FilterMode, InputMode, Tab};

// ───────────────────────────── Shared helpers ─────────────────────────────

fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Run `f` with `HOME` (and `RUNE_DATA_DIR`) pointing at `tmp`. Process-global
/// HOME state is serialized via `home_lock()`.
fn with_home<R>(tmp: &Path, f: impl FnOnce() -> R) -> R {
    let guard = home_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let orig_home = std::env::var("HOME").ok();
    let orig_rune = std::env::var("RUNE_DATA_DIR").ok();
    // SAFETY: home_lock serializes env mutation across tests in this file.
    unsafe {
        std::env::set_var("HOME", tmp);
        std::env::set_var("RUNE_DATA_DIR", tmp.join(".runai"));
    }
    let result = f();
    unsafe {
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match orig_rune {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
    }
    drop(guard);
    result
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Build a `SkillManager` whose data dir lives entirely inside `tmp` (no
/// reads/writes to the real home).
fn make_manager(tmp: &Path) -> SkillManager {
    SkillManager::with_base(tmp.join(".runai")).expect("create SkillManager with isolated data dir")
}

/// Plant a managed skill: create dir + insert DB row pointing at it.
/// Returns the directory path.
fn plant_skill(mgr: &SkillManager, name: &str) -> PathBuf {
    let dir = mgr.paths().skills_dir().join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), format!("# {name}\nbody\n")).expect("write SKILL.md");
    let resource = Resource {
        id: format!("local:{name}"),
        name: name.to_string(),
        kind: ResourceKind::Skill,
        description: format!("{name}-description"),
        directory: dir.clone(),
        source: Source::Local { path: dir.clone() },
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
    };
    mgr.db().insert_resource(&resource).expect("insert skill row");
    dir
}

/// Plant a "disabled-by-SM" MCP backup so `list_resources(Mcp)` surfaces it.
/// Returns the backup file path.
fn plant_mcp_backup(mgr: &SkillManager, name: &str) -> PathBuf {
    let mcps_dir = mgr.paths().mcps_dir();
    std::fs::create_dir_all(&mcps_dir).expect("create mcps dir");
    let path = mcps_dir.join(format!("{name}.json"));
    // Canonical MCP backup shape (command:string + args:array).
    let body = serde_json::json!({
        "command": "/bin/echo",
        "args": ["hello"],
    });
    std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap()).expect("write mcp backup");
    path
}

fn plant_group(mgr: &SkillManager, id: &str, name: &str, description: &str) {
    let group = Group {
        name: name.to_string(),
        description: description.to_string(),
        kind: GroupKind::Custom,
        auto_enable: false,
        members: Vec::new(),
    };
    mgr.create_group(id, &group).expect("create group");
}

// ───────────────────── §2.1 Skills List & Filter ──────────────────────────

#[test]
fn skill_list_displays_all_items_with_correct_count() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "beta");
        plant_skill(&mgr, "gamma");

        let mut app = App::new(mgr);
        app.tab = Tab::Skills;
        app.reload();

        assert_eq!(app.items.len(), 3, "all 3 planted skills should be listed");
        assert_eq!(
            app.visible_items().len(),
            3,
            "all 3 skills should be visible with no search/filter"
        );

        let names: Vec<String> = app.visible_items().iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"alpha".into()));
        assert!(names.contains(&"beta".into()));
        assert!(names.contains(&"gamma".into()));
    });
}

#[test]
fn filter_toggle_cycles_all_enabled_disabled() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "one");
        plant_skill(&mgr, "two");
        plant_skill(&mgr, "three");

        // Enable "one" and "two" for Claude by creating the symlink. The
        // skill .runai/skills/<name> exists; `check_skill_symlinks` calls
        // `target.skills_dir().join(name).symlink_metadata().is_ok()`, which
        // means a real directory inside ~/.claude/skills/<name> also counts
        // as enabled (the symlink form is just the default representation).
        let claude_skills = CliTarget::Claude.skills_dir();
        std::fs::create_dir_all(&claude_skills).expect("create claude skills dir");
        std::os::unix::fs::symlink(
            mgr.paths().skills_dir().join("one"),
            claude_skills.join("one"),
        )
        .expect("symlink one");
        std::os::unix::fs::symlink(
            mgr.paths().skills_dir().join("two"),
            claude_skills.join("two"),
        )
        .expect("symlink two");

        let mut app = App::new(mgr);
        app.tab = Tab::Skills;
        app.active_target = CliTarget::Claude;
        app.filter_mode = FilterMode::All;
        app.reload();

        assert_eq!(app.items.len(), 3);
        assert_eq!(
            app.visible_items().len(),
            3,
            "FilterMode::All shows all skills"
        );

        // Press 'f' -> Enabled
        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::Enabled));
        assert_eq!(
            app.visible_items().len(),
            2,
            "FilterMode::Enabled shows only the 2 enabled skills"
        );

        // Press 'f' -> Disabled
        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::Disabled));
        assert_eq!(
            app.visible_items().len(),
            1,
            "FilterMode::Disabled shows only the 1 disabled skill"
        );

        // Press 'f' -> back to All
        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::All));
        assert_eq!(app.visible_items().len(), 3);
    });
}

#[test]
fn search_filters_case_insensitive_resets_index() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "alpha");
        plant_skill(&mgr, "BETA");
        plant_skill(&mgr, "gamma");

        let mut app = App::new(mgr);
        app.tab = Tab::Skills;
        app.reload();
        // Move selection off zero so we can prove '/'-then-typing resets it.
        app.selected = 2;

        // Press '/' to enter search mode.
        app.handle_key(key(KeyCode::Char('/')));
        assert!(matches!(app.mode, InputMode::Search));
        assert!(app.search.is_empty(), "entering search clears query");

        // Type lowercase 'beta' — should match BETA (case-insensitive).
        for c in "beta".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.search, "beta");
        assert_eq!(
            app.selected, 0,
            "typing characters in search resets selected index to 0"
        );
        assert_eq!(
            app.visible_items().len(),
            1,
            "case-insensitive match returns only BETA"
        );
        assert_eq!(app.visible_items()[0].name, "BETA");

        // Erase & retype uppercase — also matches (case-insensitive).
        for _ in 0..4 {
            app.handle_key(key(KeyCode::Backspace));
        }
        for c in "BETA".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.visible_items().len(), 1);
        assert_eq!(app.visible_items()[0].name, "BETA");
    });
}

#[test]
fn mcps_list_and_filter_identical_to_skills() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        // 2 MCPs (backed up = disabled-by-SM, no CLI config exists in this
        // tempdir so they surface from the backup dir only).
        plant_mcp_backup(&mgr, "mcp-alpha");
        plant_mcp_backup(&mgr, "mcp-beta");
        // Plant a skill too — proves Tab::Mcps only surfaces MCP rows.
        plant_skill(&mgr, "should-not-appear");

        let mut app = App::new(mgr);
        app.tab = Tab::Mcps;
        app.reload();

        let kinds: Vec<ResourceKind> = app.visible_items().iter().map(|r| r.kind).collect();
        assert!(
            kinds.iter().all(|k| matches!(k, ResourceKind::Mcp)),
            "Tab::Mcps must only show MCP resources, got {kinds:?}"
        );
        assert_eq!(
            app.visible_items().len(),
            2,
            "both planted MCPs should be visible"
        );

        // Filter toggle works on Mcps tab (same code path as Skills).
        assert!(matches!(app.filter_mode, FilterMode::All));
        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::Enabled));
        // No MCP is enabled (no CLI configs in tempdir), so Enabled = 0.
        assert_eq!(app.visible_items().len(), 0);

        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::Disabled));
        assert_eq!(
            app.visible_items().len(),
            2,
            "all 2 MCPs are disabled-by-SM"
        );

        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::All));

        // Search filters MCP names case-insensitively (shared code path).
        app.handle_key(key(KeyCode::Char('/')));
        for c in "ALPHA".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.visible_items().len(), 1);
        assert_eq!(app.visible_items()[0].name, "mcp-alpha");
    });
}

// ───────────────────── §2.4 Groups List & Search ──────────────────────────

#[test]
fn group_list_displays_all_with_counts() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());

        // Plant 3 skills first so groups have something to point at.
        plant_skill(&mgr, "s1");
        plant_skill(&mgr, "s2");
        plant_skill(&mgr, "s3");

        // Enable s1 for Claude so group1's enabled count is 1.
        let claude_dir = CliTarget::Claude.skills_dir();
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::os::unix::fs::symlink(mgr.paths().skills_dir().join("s1"), claude_dir.join("s1"))
            .unwrap();

        plant_group(&mgr, "group1", "GroupOne", "first group");
        plant_group(&mgr, "group2", "GroupTwo", "second group");

        // group1: 3 members (s1, s2, s3), 1 enabled (s1 only)
        // group2: 2 members (s2, s3), 2 enabled... but s2/s3 not symlinked, so
        // group2: 2 members, 0 enabled.
        mgr.db()
            .add_group_member("group1", "local:s1")
            .expect("add s1 to group1");
        mgr.db()
            .add_group_member("group1", "local:s2")
            .expect("add s2 to group1");
        mgr.db()
            .add_group_member("group1", "local:s3")
            .expect("add s3 to group1");
        mgr.db()
            .add_group_member("group2", "local:s2")
            .expect("add s2 to group2");
        mgr.db()
            .add_group_member("group2", "local:s3")
            .expect("add s3 to group2");
        // Enable s2 + s3 for Claude so group2 has 2 enabled.
        std::os::unix::fs::symlink(mgr.paths().skills_dir().join("s2"), claude_dir.join("s2"))
            .unwrap();
        std::os::unix::fs::symlink(mgr.paths().skills_dir().join("s3"), claude_dir.join("s3"))
            .unwrap();

        let mut app = App::new(mgr);
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();

        assert_eq!(app.groups.len(), 2, "both groups visible");
        assert_eq!(app.visible_groups().len(), 2);

        // groups are sorted by name alphabetically in `list_groups()`.
        let by_id: std::collections::HashMap<String, (String, usize, usize, String)> = app
            .groups
            .iter()
            .map(|(id, name, total, enabled, desc)| {
                (
                    id.clone(),
                    (name.clone(), *total, *enabled, desc.clone()),
                )
            })
            .collect();

        let (n1, total1, enabled1, desc1) = by_id.get("group1").expect("group1 present").clone();
        assert_eq!(n1, "GroupOne");
        assert_eq!(total1, 3, "group1 has 3 members");
        assert_eq!(enabled1, 3, "group1 has 3 enabled (s1,s2,s3 all symlinked)");
        assert_eq!(desc1, "first group");

        let (n2, total2, enabled2, desc2) = by_id.get("group2").expect("group2 present").clone();
        assert_eq!(n2, "GroupTwo");
        assert_eq!(total2, 2, "group2 has 2 members");
        assert_eq!(enabled2, 2, "group2 has 2 enabled (s2,s3 symlinked)");
        assert_eq!(desc2, "second group");
    });
}

#[test]
fn group_search_case_insensitive() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        // Plant a sentinel skill so App doesn't boot into FirstLaunch mode.
        plant_skill(&mgr, "sentinel-skill");
        plant_group(&mgr, "g-my", "MyGroup", "");
        plant_group(&mgr, "g-test", "TestGroup", "");
        plant_group(&mgr, "g-other", "another", "");

        let mut app = App::new(mgr);
        app.mode = InputMode::Normal; // belt + suspenders against FirstLaunch
        app.tab = Tab::Groups;
        app.reload();
        assert_eq!(app.visible_groups().len(), 3);

        // 'my' matches MyGroup (case-insensitive)
        app.search = "my".to_string();
        let names: Vec<String> = app
            .visible_groups()
            .iter()
            .map(|(_, n, _, _, _)| n.clone())
            .collect();
        assert_eq!(names, vec!["MyGroup".to_string()]);

        // Uppercase 'MY' also matches
        app.search = "MY".to_string();
        let names: Vec<String> = app
            .visible_groups()
            .iter()
            .map(|(_, n, _, _, _)| n.clone())
            .collect();
        assert_eq!(names, vec!["MyGroup".to_string()]);

        // 'test' returns TestGroup
        app.search = "test".to_string();
        let names: Vec<String> = app
            .visible_groups()
            .iter()
            .map(|(_, n, _, _, _)| n.clone())
            .collect();
        assert_eq!(names, vec!["TestGroup".to_string()]);

        // 'xyz' returns empty
        app.search = "xyz".to_string();
        assert_eq!(app.visible_groups().len(), 0);
    });
}

#[test]
fn group_search_by_id() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "sentinel-skill");
        plant_group(&mgr, "my-group", "MyGroup", "");
        plant_group(&mgr, "other-id", "Other", "");

        let mut app = App::new(mgr);
        app.mode = InputMode::Normal;
        app.tab = Tab::Groups;
        app.reload();

        // Full id match
        app.search = "my-group".to_string();
        assert_eq!(app.visible_groups().len(), 1);
        assert_eq!(app.visible_groups()[0].0, "my-group");

        // Uppercase id match (case-insensitive)
        app.search = "MY-GROUP".to_string();
        assert_eq!(app.visible_groups().len(), 1);
        assert_eq!(app.visible_groups()[0].0, "my-group");

        // Substring '-group' should match by id (both 'my-group' has it).
        app.search = "-group".to_string();
        let ids: Vec<String> = app
            .visible_groups()
            .iter()
            .map(|(id, _, _, _, _)| id.clone())
            .collect();
        assert!(ids.contains(&"my-group".into()));
    });
}

#[test]
fn search_clears_selected_index() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "sentinel-skill");
        plant_group(&mgr, "g-a", "Alpha", "");
        plant_group(&mgr, "g-b", "Beta", "");
        plant_group(&mgr, "g-c", "Gamma", "");

        let mut app = App::new(mgr);
        app.mode = InputMode::Normal;
        app.tab = Tab::Groups;
        app.reload();
        app.selected = 2;

        // Press '/' to enter search.
        app.handle_key(key(KeyCode::Char('/')));
        assert!(matches!(app.mode, InputMode::Search));
        // search.clear() ran on '/' but the index is NOT reset until the
        // first character is typed. Type one char to filter.
        app.handle_key(key(KeyCode::Char('a')));
        assert_eq!(
            app.selected, 0,
            "typing a char in search resets selected index to 0"
        );

        // ESC clears search and returns to Normal mode.
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(app.search.is_empty(), "Esc clears search query");
        assert_eq!(app.selected, 0);
    });
}

// ───────────────────── §2.8 Group Detail Overlay ──────────────────────────

#[test]
fn open_group_detail_loads_members() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "skill1");
        plant_skill(&mgr, "skill2");
        plant_group(&mgr, "grp", "MyGroup", "");
        mgr.db().add_group_member("grp", "local:skill1").unwrap();
        mgr.db().add_group_member("grp", "local:skill2").unwrap();

        let mut app = App::new(mgr);
        app.mode = InputMode::Normal;
        app.tab = Tab::Groups;
        app.reload();
        app.selected = 0;

        // Enter opens the group detail overlay.
        app.handle_key(key(KeyCode::Enter));

        assert!(matches!(app.mode, InputMode::GroupDetail));
        assert_eq!(app.detail_group_id, "grp");
        assert_eq!(app.detail_group_name, "MyGroup");
        assert_eq!(
            app.detail_members.len(),
            2,
            "detail_members loaded from get_group_members()"
        );
        assert_eq!(app.detail_idx, 0, "cursor starts at first member");
        let member_names: Vec<String> = app.detail_members.iter().map(|r| r.name.clone()).collect();
        assert!(member_names.contains(&"skill1".into()));
        assert!(member_names.contains(&"skill2".into()));
    });
}

#[test]
fn group_detail_navigation_clamps() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "a");
        plant_skill(&mgr, "b");
        plant_skill(&mgr, "c");
        plant_group(&mgr, "grp", "G", "");
        mgr.db().add_group_member("grp", "local:a").unwrap();
        mgr.db().add_group_member("grp", "local:b").unwrap();
        mgr.db().add_group_member("grp", "local:c").unwrap();

        let mut app = App::new(mgr);
        app.mode = InputMode::Normal;
        app.tab = Tab::Groups;
        app.reload();
        app.selected = 0;
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, InputMode::GroupDetail));
        assert_eq!(app.detail_members.len(), 3);
        assert_eq!(app.detail_idx, 0);

        // j moves down through 0 -> 1 -> 2 -> clamps at 2.
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.detail_idx, 1);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.detail_idx, 2);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.detail_idx, 2, "j at last clamps");

        // k moves back up; clamps at 0.
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.detail_idx, 1);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.detail_idx, 0);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.detail_idx, 0, "k at 0 clamps");
    });
}

#[test]
fn escape_from_group_detail() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "s");
        plant_group(&mgr, "g", "Gr", "");
        mgr.db().add_group_member("g", "local:s").unwrap();

        let mut app = App::new(mgr);
        app.mode = InputMode::Normal;
        app.tab = Tab::Groups;
        app.reload();
        app.selected = 0;
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, InputMode::GroupDetail));

        // Esc returns to Normal and stays on Groups tab.
        app.handle_key(key(KeyCode::Esc));
        assert!(matches!(app.mode, InputMode::Normal));
        assert!(matches!(app.tab, Tab::Groups));
    });
}

#[test]
fn group_detail_target_switch_reloads() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_manager(tmp.path());
        plant_skill(&mgr, "member");
        plant_group(&mgr, "g", "Gr", "");
        mgr.db().add_group_member("g", "local:member").unwrap();

        // Enable for Claude only — pre-create the symlink so the skill is
        // "enabled for Claude" by definition (`check_skill_symlinks` truths).
        let claude_dir = CliTarget::Claude.skills_dir();
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::os::unix::fs::symlink(
            mgr.paths().skills_dir().join("member"),
            claude_dir.join("member"),
        )
        .unwrap();

        let mut app = App::new(mgr);
        app.mode = InputMode::Normal;
        app.tab = Tab::Groups;
        app.active_target = CliTarget::Claude;
        app.reload();
        app.selected = 0;
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.mode, InputMode::GroupDetail));
        assert!(
            app.detail_members[0].is_enabled_for(CliTarget::Claude),
            "member enabled for Claude before switch"
        );

        // Press '2' inside detail overlay -> switch to Codex + reload members.
        app.handle_key(key(KeyCode::Char('2')));
        assert!(matches!(app.active_target, CliTarget::Codex));
        // After reload, the member is NOT enabled for Codex (no codex symlink).
        assert!(
            !app.detail_members[0].is_enabled_for(CliTarget::Codex),
            "member should be disabled for Codex after target switch"
        );
        // Mode stays in GroupDetail.
        assert!(matches!(app.mode, InputMode::GroupDetail));
    });
}

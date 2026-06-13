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

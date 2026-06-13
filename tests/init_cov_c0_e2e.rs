//! Integration coverage for `src/tui/app.rs` init + reload APIs.
//!
//! NOTE: the audit chunk asked for `src/tui/app/init.rs` and the four symbols
//! `App::new() -> Self`, `App::with_data_dir(PathBuf) -> Self`,
//! `App::reload(&mut self) -> Result<()>`, `App::reload_groups(&mut self) -> Result<()>`.
//! Grep over the real HEAD shows:
//!   * `src/tui/app.rs:303` `pub fn new(mgr: SkillManager) -> Self`  (takes a manager, no Result)
//!   * `src/tui/app.rs:390` `pub fn reload(&mut self)`               (no Result)
//!   * `App::with_data_dir` and `App::reload_groups` do **not exist** in HEAD.
//!
//! So we cover the two real entry points (App::new + App::reload) at full
//! depth, and surface the other two as skipped (signature does not exist).
//!
//! All tests sandbox HOME to a tempdir and pin RUNE_DATA_DIR / RUNAI_NO_AUTOSPAWN
//! per the safety contract; never touch the user's real `~/.runai/`.

#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use runai::core::cli_target::CliTarget;
use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind, Source};
use runai::tui::app::{App, FilterMode, InputMode, Tab};
use runai::tui::i18n::Lang;
use runai::tui::theme::ThemeMode;

/// Serialize HOME mutation across tests in this file. `--test-threads=1` is the
/// CI default but the lock keeps the file safe under default `cargo test` too.
fn home_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = home_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original_home = std::env::var("HOME").ok();
    let original_data = std::env::var("RUNE_DATA_DIR").ok();
    let original_autospawn = std::env::var("RUNAI_NO_AUTOSPAWN").ok();
    unsafe {
        std::env::set_var("HOME", tmp);
        std::env::set_var("RUNE_DATA_DIR", tmp.join(".runai"));
        std::env::set_var("RUNAI_NO_AUTOSPAWN", "1");
    }
    f();
    unsafe {
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match original_data {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match original_autospawn {
            Some(v) => std::env::set_var("RUNAI_NO_AUTOSPAWN", v),
            None => std::env::remove_var("RUNAI_NO_AUTOSPAWN"),
        }
    }
}

fn make_mgr(tmp: &Path) -> SkillManager {
    SkillManager::with_base(tmp.join("data")).unwrap()
}

fn make_skill_resource(name: &str, dir: &Path) -> Resource {
    Resource {
        id: format!("local:{name}"),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: format!("desc-{name}"),
        directory: dir.to_path_buf(),
        source: Source::Local {
            path: dir.to_path_buf(),
        },
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
    }
}

// ----------------------------------------------------------------------------
// Feature 1: App::new(mgr) -> Self
// ----------------------------------------------------------------------------

#[test]
fn app_new_initializes_default_tab() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        let app = App::new(mgr);
        assert!(
            app.tab == Tab::Skills,
            "App::new should land on Tab::Skills first"
        );
    });
}

#[test]
fn app_new_starts_with_skills_tab_and_first_launch_mode_on_empty_data() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        // brand-new data dir => mgr.is_first_launch() == true => mode FirstLaunch(0)
        let app = App::new(mgr);
        assert!(app.tab == Tab::Skills);
        assert!(
            matches!(app.mode, InputMode::FirstLaunch(_)),
            "new App on empty data should boot into FirstLaunch onboarding, got non-FirstLaunch mode"
        );
    });
}

#[test]
fn app_new_initializes_empty_filter_and_search() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        let app = App::new(mgr);
        assert!(app.search.is_empty(), "search buf must start empty");
        assert!(app.input_buf.is_empty(), "input_buf must start empty");
        assert!(app.create_name.is_empty(), "create_name must start empty");
        assert!(
            matches!(app.filter_mode, FilterMode::All),
            "default filter is All"
        );
        assert_eq!(app.selected, 0, "selection must start at 0");
        assert_eq!(app.status, (0, 0, 0, 0), "status counters must zero out");
        assert_eq!(app.items.len(), 0, "items must be empty pre-reload");
        assert_eq!(app.groups.len(), 0, "groups must be empty pre-reload");
        assert!(app.message.is_none(), "no message yet");
        assert!(app.pending_delete.is_none(), "no pending delete yet");
    });
}

#[test]
fn app_new_initializes_theme_lang_and_target_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        let app = App::new(mgr);
        // Defaults asserted by src/tui/app.rs:303-347.
        assert!(matches!(app.theme_mode, ThemeMode::Dark), "default Dark");
        assert!(matches!(app.lang, Lang::Zh), "default Zh");
        assert!(
            matches!(app.active_target, CliTarget::Claude),
            "default Claude target"
        );
        // Sources should be loaded (builtin skills-hub sentinel at minimum).
        assert!(
            !app.sources.is_empty(),
            "App::new must load at least the builtin sources"
        );
    });
}

// ----------------------------------------------------------------------------
// Feature 2: App::reload(&mut self)
// ----------------------------------------------------------------------------

#[test]
fn reload_refreshes_resources_from_db() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        // Pre-seed a skill row + on-disk dir.
        let skill_dir = mgr.paths().skills_dir().join("alpha-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let res = make_skill_resource("alpha-skill", &skill_dir);
        mgr.db().insert_resource(&res).unwrap();

        let mut app = App::new(mgr);
        assert_eq!(app.items.len(), 0, "pre-reload items must be empty");
        app.reload();
        assert!(
            app.items.iter().any(|r| r.name == "alpha-skill"),
            "post-reload items must include alpha-skill, got {:?}",
            app.items.iter().map(|r| &r.name).collect::<Vec<_>>()
        );
    });
}

#[test]
fn reload_updates_status_counters_from_db() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        let dir = mgr.paths().skills_dir().join("beta-skill");
        std::fs::create_dir_all(&dir).unwrap();
        mgr.db()
            .insert_resource(&make_skill_resource("beta-skill", &dir))
            .unwrap();

        let mut app = App::new(mgr);
        app.reload();
        // status = (enabled_skills, total_skills, enabled_mcps, total_mcps).
        // We didn't symlink so enabled is 0, but total skills must reflect 1.
        assert_eq!(
            app.status.1, 1,
            "total skills counter must equal inserted rows, got {:?}",
            app.status
        );
    });
}

#[test]
fn reload_kind_filter_excludes_mcps_when_on_skills_tab() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        // Insert one skill + one MCP. Reload on Skills tab must only see the
        // skill row in `app.items`.
        let skill_dir = mgr.paths().skills_dir().join("only-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        mgr.db()
            .insert_resource(&make_skill_resource("only-skill", &skill_dir))
            .unwrap();

        let mcp_dir = mgr.paths().data_dir().join("mcps").join("only-mcp");
        std::fs::create_dir_all(&mcp_dir).unwrap();
        let mcp = Resource {
            id: "local:only-mcp".into(),
            name: "only-mcp".into(),
            kind: ResourceKind::Mcp,
            description: "an mcp".into(),
            directory: mcp_dir.clone(),
            source: Source::Local { path: mcp_dir },
            installed_at: 0,
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
        };
        mgr.db().insert_resource(&mcp).unwrap();

        let mut app = App::new(mgr);
        app.tab = Tab::Skills;
        app.reload();

        assert!(
            app.items
                .iter()
                .all(|r| matches!(r.kind, ResourceKind::Skill)),
            "Skills tab reload must only carry skill rows, got: {:?}",
            app.items
                .iter()
                .map(|r| (&r.name, r.kind))
                .collect::<Vec<_>>()
        );
        assert!(
            app.items.iter().any(|r| r.name == "only-skill"),
            "should have only-skill"
        );
        assert!(
            app.items.iter().all(|r| r.name != "only-mcp"),
            "must not leak only-mcp into Skills tab"
        );
    });
}

#[test]
fn reload_clamps_selection_when_visible_count_shrinks() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        let dir = mgr.paths().skills_dir().join("solo");
        std::fs::create_dir_all(&dir).unwrap();
        mgr.db()
            .insert_resource(&make_skill_resource("solo", &dir))
            .unwrap();

        let mut app = App::new(mgr);
        // Force selection above the visible count so reload's tail clamp fires
        // (src/tui/app.rs:443-445).
        app.selected = 999;
        app.tab = Tab::Skills;
        app.reload();
        assert_eq!(app.items.len(), 1, "exactly one skill row");
        assert_eq!(
            app.selected, 0,
            "reload must clamp selected to visible_count-1 when over",
        );
    });
}

#[test]
fn reload_loads_groups_into_app_groups_field() {
    use runai::core::group::{Group, GroupKind};
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = make_mgr(tmp.path());
        // Create a group on disk via the manager API path.
        let group = Group {
            name: "my-grp".into(),
            description: "test group".into(),
            kind: GroupKind::Custom,
            auto_enable: false,
            members: Vec::new(),
        };
        mgr.create_group("my-grp", &group)
            .expect("create_group should succeed");

        let mut app = App::new(mgr);
        app.reload();

        assert!(
            app.groups
                .iter()
                .any(|(id, name, _, _, _)| id.as_str() == "my-grp" && name.as_str() == "my-grp"),
            "reload must surface manually-created group into app.groups; got {:?}",
            app.groups.iter().map(|t| (&t.0, &t.1)).collect::<Vec<_>>()
        );
    });
}

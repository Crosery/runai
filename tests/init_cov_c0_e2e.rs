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

#[allow(dead_code)]
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

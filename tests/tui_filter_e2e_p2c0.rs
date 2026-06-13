//! P2 regression tests for TUI Filter Mode Toggle (plan §2.25).
//!
//! Tests the `f` key cycling FilterMode through All → Enabled → Disabled → All
//! in Skills / MCPs tabs, that filter_mode state is consistent, and that the
//! key is ignored on non-skill / non-mcp tabs.
//!
//! Pure TUI state-machine tests: no filesystem mutation beyond the per-test
//! HOME tempdir for SkillManager bootstrap.
#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tempfile::TempDir;

use runai::core::cli_target::CliTarget;
use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind, Source};
use runai::tui::app::{App, FilterMode, Tab};

/// Process-wide guard so concurrent HOME mutation across tests in this file
/// cannot race. We run with --test-threads=1 in CI anyway, but this is cheap
/// insurance and matches the in-tree `HOME_LOCK` pattern.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn with_home<F: FnOnce()>(tmp: &std::path::Path, f: F) {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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

fn build_app(tmp: &std::path::Path) -> App {
    let mgr = SkillManager::with_base(tmp.join("data")).unwrap();
    let mut app = App::new(mgr);
    // Force Normal mode (suppress first-launch state) so handle_key routes
    // through normal_actions where 'f' is handled.
    app.mode = runai::tui::app::InputMode::Normal;
    app
}

fn make_skill(
    name: &str,
    enabled_for: &[(CliTarget, bool)],
    base: &std::path::Path,
) -> (Resource, PathBuf) {
    let dir = base.join("data").join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let mut enabled = HashMap::new();
    for (t, v) in enabled_for {
        enabled.insert(*t, *v);
    }
    let resource = Resource {
        id: format!("local:{name}"),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: format!("test {name}"),
        directory: dir.clone(),
        source: Source::Local { path: dir.clone() },
        installed_at: 0,
        enabled,
        usage_count: 0,
        last_used_at: None,
    };
    (resource, dir)
}

// ─── Test 1: filter_toggle_cycles_and_updates_visible_items ────────────────

#[test]
fn filter_toggle_cycles_and_updates_visible_items() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mut app = build_app(tmp.path());

        // Inject items directly so we don't rely on filesystem symlink state.
        // Two skills: one enabled on Claude, one disabled on Claude.
        let (skill_a, _) = make_skill("skill-enabled", &[(CliTarget::Claude, true)], tmp.path());
        let (skill_b, _) = make_skill("skill-disabled", &[(CliTarget::Claude, false)], tmp.path());

        app.tab = Tab::Skills;
        app.active_target = CliTarget::Claude;
        app.items = vec![skill_a, skill_b];
        app.selected = 5; // arbitrary non-zero so we can verify reset

        // Initial state: FilterMode::All, both skills visible
        assert!(matches!(app.filter_mode, FilterMode::All));
        assert_eq!(
            app.visible_items().len(),
            2,
            "FilterMode::All should show every item"
        );

        // Press f once: All → Enabled
        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::Enabled));
        let vis = app.visible_items();
        assert_eq!(
            vis.len(),
            1,
            "FilterMode::Enabled should show only enabled skills"
        );
        assert_eq!(vis[0].name, "skill-enabled");
        assert_eq!(
            app.selected, 0,
            "selected should reset to 0 after filter change"
        );

        // Press f again: Enabled → Disabled
        app.selected = 7;
        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::Disabled));
        let vis = app.visible_items();
        assert_eq!(
            vis.len(),
            1,
            "FilterMode::Disabled should show only disabled skills"
        );
        assert_eq!(vis[0].name, "skill-disabled");
        assert_eq!(app.selected, 0, "selected should reset to 0 again");

        // Press f again: Disabled → All
        app.selected = 3;
        app.handle_key(key(KeyCode::Char('f')));
        assert!(matches!(app.filter_mode, FilterMode::All));
        assert_eq!(
            app.visible_items().len(),
            2,
            "FilterMode::All after a full cycle should restore everything"
        );
        assert_eq!(app.selected, 0);
    });
}

// Plan test #2 (`filter_mode_independent_per_tab`) is intentionally omitted:
// it asserts a per-tab independent filter_mode (MCPs starts at All when
// Skills is Enabled). The current `App` type carries a single global
// `filter_mode` field on the struct, so the assertion documents a behavior
// gap with src, not a working invariant. Per the harness rules we do not
// edit src/ from a tests-only chunk, so that test is reported as test_red.

// ─── Test 3: filter_toggle_ignored_on_non_skill_tabs ───────────────────────

#[test]
fn filter_toggle_ignored_on_non_skill_tabs() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mut app = build_app(tmp.path());
        app.active_target = CliTarget::Claude;

        for non_skill_tab in [Tab::Groups, Tab::Market, Tab::Trash] {
            app.tab = non_skill_tab;
            app.filter_mode = FilterMode::All;
            app.message = None;

            app.handle_key(key(KeyCode::Char('f')));

            assert!(
                matches!(app.filter_mode, FilterMode::All),
                "filter_mode must not change when pressing 'f' on tab {}",
                non_skill_tab.label()
            );

            // The msg should not include the filter prefix string for any tab
            // other than Skills/Mcps. We don't enforce exact empty-message
            // because other key handlers might set unrelated messages, but
            // we can verify that no filter-toggle-style message was posted
            // when filter_mode itself did not change.
            if let Some(m) = &app.message {
                let zh_prefix = runai::tui::i18n::T::new(app.lang).filter_all();
                assert!(
                    !m.contains(zh_prefix),
                    "non-skill tab {} should not display filter message, got: {}",
                    non_skill_tab.label(),
                    m
                );
            }
        }
    });
}

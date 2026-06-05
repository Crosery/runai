//! Unit-level coverage of the TUI hook panel state machine
//! (PLANNING.md §1.5).
//!
//! These tests run on the same `App` state machine the TUI uses but never
//! mount a ratatui terminal. They check the cheap invariants:
//!   * `Tab::Hooks` and `Tab::Community` are wired into `Tab::ALL`.
//!   * `App::new` seeds every CLI target with the unsupported default.
//!   * `handle_hooks_key` only moves the cursor inside bounds.
//!   * Toggle on a non-Claude row is a no-op + a user-visible message.
//!   * `reload_hook_status` reads the HOME-rooted Claude settings.json and
//!     flips the slot only when the runai router hook is actually present.
//!
//! HOME mocking + symlinks are unix-only (see AGENTS.md key constraints).
//! Windows skips the whole file.
#![cfg(not(target_os = "windows"))]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::cli_target::CliTarget;
use runai::core::manager::SkillManager;
use runai::tui::app::{App, Tab};

/// Process-wide HOME serialization. Multiple tests in this file mutate
/// `HOME`, and `dirs::home_dir()` reads it on unix, so concurrent setters
/// would step on each other.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Build an `App` rooted at a fresh tempdir HOME. The tempdir is returned
/// to keep it alive for the duration of the test.
fn app_in_tmp_home() -> (App, tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    let mgr = SkillManager::with_base(tmp.path().join("data")).expect("manager init");
    let mut app = App::new(mgr);
    // `App::new` may park us in FirstLaunch(0) when the manager
    // reports a fresh install — every keypress would then route to
    // the first-launch handler and never reach the hooks panel. Force
    // Normal so tests exercise the real keymap. Same fix the TUI's
    // own `tests.rs` module relies on (it asserts after `reload`).
    app.mode = runai::tui::app::InputMode::Normal;
    app.tab = Tab::Hooks;
    app.reload();
    (app, tmp, guard)
}

#[test]
fn tabs_all_contains_hooks_and_community_in_canonical_order() {
    // `Tab::ALL` is the source of truth — TUI nav, tests, and the
    // header tab strip all index off it. Locking the order guards
    // against accidental reshuffles.
    let labels: Vec<&str> = Tab::ALL.iter().map(|t| t.label()).collect();
    assert_eq!(
        labels,
        vec![
            "Skills",
            "MCPs",
            "Groups",
            "Market",
            "Community",
            "Hooks",
            "Trash",
        ],
        "Tab::ALL order is load-bearing for digit shortcuts and nav"
    );
}

#[test]
fn app_new_seeds_all_targets_as_unsupported_by_default() {
    let (app, _tmp, _guard) = app_in_tmp_home();
    for target in CliTarget::ALL {
        let slot = app.hook_status.get(target).expect("seeded entry");
        // Before `reload_hook_status` runs the panel is uniformly
        // unsupported — that's the safe default so an unrelated CLI
        // never shows up as "installed" out of nowhere.
        if *target == CliTarget::Claude {
            // After `app.reload()` from app_in_tmp_home, Claude is
            // detected as not-installed (supported = true, installed
            // = false) because tmp HOME has no settings.json.
            assert!(slot.supported, "Claude must be marked supported");
            assert!(!slot.installed, "fresh HOME has no Claude hook");
        } else {
            assert!(
                !slot.supported,
                "{} should be marked unsupported (only Claude is wired today)",
                target.name()
            );
        }
    }
}

#[test]
fn hooks_panel_navigation_clamps_at_top_and_bottom() {
    let (mut app, _tmp, _guard) = app_in_tmp_home();
    assert_eq!(app.hook_panel_idx, 0);

    // 'k' at top stays at top.
    app.handle_key(key(KeyCode::Char('k')));
    assert_eq!(app.hook_panel_idx, 0);

    // Walk down past the bottom; should stop at len-1.
    for _ in 0..(CliTarget::ALL.len() + 5) {
        app.handle_key(key(KeyCode::Char('j')));
    }
    assert_eq!(app.hook_panel_idx, CliTarget::ALL.len() - 1);

    // 'g' / 'G' jump to ends.
    app.handle_key(key(KeyCode::Char('g')));
    assert_eq!(app.hook_panel_idx, 0);
    app.handle_key(key(KeyCode::Char('G')));
    assert_eq!(app.hook_panel_idx, CliTarget::ALL.len() - 1);
}

#[test]
fn toggle_on_unsupported_row_is_a_noop_with_message() {
    let (mut app, _tmp, _guard) = app_in_tmp_home();
    // Move to Codex row (index 1).
    let codex_idx = CliTarget::ALL
        .iter()
        .position(|t| matches!(t, CliTarget::Codex))
        .expect("Codex in CliTarget::ALL");
    for _ in 0..codex_idx {
        app.handle_key(key(KeyCode::Char('j')));
    }

    let slot_before = *app
        .hook_status
        .get(&CliTarget::Codex)
        .expect("Codex seeded");

    app.handle_key(key(KeyCode::Char(' ')));

    let slot_after = *app
        .hook_status
        .get(&CliTarget::Codex)
        .expect("Codex still seeded");
    assert_eq!(slot_before, slot_after, "unsupported row must not flip");
    assert!(
        app.message
            .as_deref()
            .is_some_and(|m| m.contains("不支持") || m.contains("unsupported")),
        "user must see a 'not supported' message"
    );
}

#[test]
fn reload_hook_status_reflects_existing_claude_settings() {
    let (mut app, tmp, _guard) = app_in_tmp_home();

    // Plant a settings.json that already has the runai hook entry —
    // mirrors the shape `install_claude_hook` writes.
    let settings_path = tmp.path().join(".claude").join("settings.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
    std::fs::write(
        &settings_path,
        r#"{
  "hooks": {
    "UserPromptSubmit": [
      {"hooks": [{"type": "command", "command": "runai recommend"}]}
    ]
  }
}"#,
    )
    .unwrap();

    app.reload_hook_status();

    let claude = app.hook_status.get(&CliTarget::Claude).expect("Claude row");
    assert!(claude.supported, "Claude row is always supported");
    assert!(
        claude.installed,
        "reload_hook_status should detect the planted runai hook"
    );
}

#[test]
fn reload_hook_status_no_settings_file_marks_claude_not_installed() {
    let (mut app, tmp, _guard) = app_in_tmp_home();
    // Sanity: no settings.json was written.
    assert!(!tmp.path().join(".claude/settings.json").exists());

    app.reload_hook_status();
    let claude = app.hook_status.get(&CliTarget::Claude).expect("Claude row");
    assert!(claude.supported);
    assert!(
        !claude.installed,
        "missing settings.json should not look installed"
    );
}

#[test]
fn community_visible_filters_by_search_term() {
    use runai::tui::app::CommunitySkill;
    let (mut app, _tmp, _guard) = app_in_tmp_home();
    app.community_skills = vec![
        CommunitySkill {
            uploader_uid: "u_alice".into(),
            uploader_username: "alice".into(),
            name: "alpha-skill".into(),
            version: "1".into(),
            installs_total: 10,
            created_at: 0,
        },
        CommunitySkill {
            uploader_uid: "u_bob".into(),
            uploader_username: "bob".into(),
            name: "beta-skill".into(),
            version: "2".into(),
            installs_total: 5,
            created_at: 0,
        },
    ];
    app.search = "alpha".into();
    let vis = app.visible_community();
    assert_eq!(vis.len(), 1);
    assert_eq!(vis[0].name, "alpha-skill");

    app.search = "bob".into();
    let vis = app.visible_community();
    assert_eq!(vis.len(), 1, "uploader_username should match too");
    assert_eq!(vis[0].name, "beta-skill");

    app.search.clear();
    assert_eq!(app.visible_community().len(), 2);
}

#[test]
fn hooks_tab_does_not_swallow_quit_or_tab_keys() {
    // `handle_hooks_key` returns false for unrecognized keys so the
    // outer normal-mode dispatch (theme/help/tab-switch/etc.) still
    // sees them. Tab-switch is the load-bearing example: if Hooks ate
    // 'L', the user would be stuck on the tab.
    let (mut app, _tmp, _guard) = app_in_tmp_home();
    assert!(app.tab == Tab::Hooks);
    app.handle_key(key(KeyCode::Char('L')));
    assert!(app.tab != Tab::Hooks, "L must still navigate tabs");
}

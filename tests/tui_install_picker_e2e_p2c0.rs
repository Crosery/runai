//! P2 e2e: TUI Install-from-GitHub flow (PLANNING §2.31).
//!
//! Covers `App::handle_install_key` (entry: `src/tui/app.rs` —
//! cloud HEAD flat layout, not the folder-split `src/tui/app/keybindings.rs`
//! quoted in the older plan).
//!
//! All tests sandbox HOME under `tempfile::TempDir` and use
//! `SkillManager::with_base` so nothing touches the real `~/.runai/`.
//! These tests construct the `App` directly via library APIs and drive
//! `handle_key` synchronously; they do NOT spawn the runai binary
//! (binary spawn would still pull from real network for any github call).
//!
//! Skipped on Windows: `HOME` mocking is unix-only (see AGENTS.md
//! "dirs::home_dir on Windows" note).
#![cfg(not(target_os = "windows"))]

use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::manager::SkillManager;
use runai::tui::app::{App, InputMode, Tab};

/// Process-wide HOME lock — multiple tests in this file all set/unset
/// HOME; serialize them. Mirrors `crate::test_support::HOME_LOCK` from
/// the lib (not exposed to integration tests, so we maintain our own).
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_home<F: FnOnce()>(tmp: &std::path::Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::env::var("HOME").ok();
    // SAFETY: serialised by HOME_LOCK above; tests must not spawn threads
    // that read HOME concurrently.
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

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn fresh_app(tmp: &std::path::Path) -> App {
    let mgr = SkillManager::with_base(tmp.join("data")).expect("with_base");
    let mut app = App::new(mgr);
    // Make sure we're on a non-FirstLaunch mode so 'i' triggers the
    // install dispatch (FirstLaunch swallows keys differently).
    app.mode = InputMode::Normal;
    app.tab = Tab::Skills;
    app
}

// ───────────────────────────────────────────────────────────────────────
// Test 1 [unit_in_module]: invalid format shows error
// (Renamed from physical_e2e to unit_in_module because we cannot
// guarantee outbound network reachability from this test process.
// The plan's test #1 requires a real GitHub fetch; instead we keep
// test coverage where the e2e suite can actually run, and verify the
// invalid-source branch which exercises the same handler path up to
// `parse_github_source` failure.)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn install_github_invalid_format_shows_error() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());

        // Enter install mode.
        app.handle_key(key(KeyCode::Char('i')));
        assert!(matches!(app.mode, InputMode::Install));

        // Type an invalid source (no '/').
        for c in "invalid".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(app.input_buf, "invalid");

        // Press Enter — parse_github_source should fail because the
        // input has no '/'. The handler must report 'Invalid source: …'
        // and return to Normal mode without panicking.
        app.handle_key(key(KeyCode::Enter));

        assert!(
            matches!(app.mode, InputMode::Normal),
            "mode should return to Normal after invalid-source error"
        );
        let msg = app.message.as_deref().unwrap_or("");
        assert!(
            msg.starts_with("Invalid source:"),
            "message should start with 'Invalid source:', got {msg:?}"
        );
        // input_buf is cleared by the Enter branch before parsing.
        assert!(app.input_buf.is_empty(), "input_buf should be cleared");
    });
}

// ───────────────────────────────────────────────────────────────────────
// Test 2 [unit_in_module]: Esc cancels Install mode and clears input
// ───────────────────────────────────────────────────────────────────────

#[test]
fn install_cancel_via_esc() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());

        // Enter install mode and type a source.
        app.handle_key(key(KeyCode::Char('i')));
        for c in "owner/repo".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        assert!(matches!(app.mode, InputMode::Install));
        assert_eq!(app.input_buf, "owner/repo");

        // Esc cancels: clears input_buf and returns to Normal.
        app.handle_key(key(KeyCode::Esc));

        assert!(
            matches!(app.mode, InputMode::Normal),
            "mode should return to Normal after Esc"
        );
        assert!(app.input_buf.is_empty(), "input_buf should be cleared");
    });
}

// ───────────────────────────────────────────────────────────────────────
// Test 3 [unit_in_module]: enter on empty input is a no-op cancel
// (Defensive coverage for the empty-input branch of handle_install_key
// which the plan did not call out but is on the same flow path —
// belt-and-suspenders so a refactor of the if-empty short-circuit can't
// silently break by, e.g., calling parse_github_source on "".)
// ───────────────────────────────────────────────────────────────────────

#[test]
fn install_empty_input_returns_to_normal() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());

        app.handle_key(key(KeyCode::Char('i')));
        assert!(matches!(app.mode, InputMode::Install));
        assert!(app.input_buf.is_empty());

        // Enter on empty input: handler returns to Normal without
        // calling parse_github_source — and crucially without setting
        // any "Invalid source" message.
        app.handle_key(key(KeyCode::Enter));

        assert!(
            matches!(app.mode, InputMode::Normal),
            "empty-input Enter should return to Normal"
        );
        assert!(
            app.message
                .as_deref()
                .map(|m| !m.starts_with("Invalid source:"))
                .unwrap_or(true),
            "empty Enter must not emit an Invalid source error"
        );
    });
}

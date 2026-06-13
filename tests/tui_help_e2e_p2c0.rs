//! P2 test coverage for TUI Help Overlay (PLANNING test plan §2.30).
//!
//! Feature: in any tab, pressing '?' enters Help mode and shows the keyboard
//! shortcut overlay. Pressing any key (including 'q' / Esc / any char) returns
//! to Normal. Help mode has no sub-modes.
//!
//! Entry points in src/tui/app.rs:
//!   - InputMode::Help variant (mode enum)
//!   - handle_normal_key: `KeyCode::Char('?')` -> `self.mode = InputMode::Help`
//!   - handle_key dispatch for InputMode::Help: any key sets mode = Normal
//!
//! Tests are unit-style against the `App` state machine, but sandboxed in an
//! isolated `HOME` + `RUNE_DATA_DIR` per the safety contract — `SkillManager`
//! still resolves real paths from those env vars.

#![cfg(not(target_os = "windows"))]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::manager::SkillManager;
use runai::tui::app::{App, InputMode, Tab};
use std::path::Path;
use std::sync::Mutex;
use tempfile::TempDir;

/// Local env-mutation lock for this test file. Integration tests cannot reach
/// `runai::test_support::HOME_LOCK` (it's `#[cfg(test)]`-gated inside the crate),
/// and this file has its own single-process tests so a local mutex is enough.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Run `f` with HOME and RUNE_DATA_DIR pointed at `tmp` so SkillManager's
/// path resolution stays inside the sandbox.
fn with_sandbox<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original_home = std::env::var("HOME").ok();
    let original_data = std::env::var("RUNE_DATA_DIR").ok();
    unsafe {
        std::env::set_var("HOME", tmp);
        std::env::set_var("RUNE_DATA_DIR", tmp.join(".runai"));
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
    }
}

fn fresh_app(tmp: &Path) -> App {
    let mgr = SkillManager::with_base(tmp.join(".runai")).expect("SkillManager init");
    let mut app = App::new(mgr);
    // Force Normal mode regardless of first-launch detection — we are
    // testing the Help overlay state transition, not the first-launch flow.
    app.mode = InputMode::Normal;
    app.tab = Tab::Skills;
    app
}

#[test]
fn help_overlay_enter_and_exit() {
    let tmp = TempDir::new().unwrap();
    with_sandbox(tmp.path(), || {
        let mut app = fresh_app(tmp.path());

        // Precondition: Normal mode.
        assert!(
            matches!(app.mode, InputMode::Normal),
            "fresh app should start in Normal mode (forced)"
        );

        // Press '?' → enters Help mode.
        app.handle_key(key(KeyCode::Char('?')));
        assert!(
            matches!(app.mode, InputMode::Help),
            "'?' from Normal should enter Help overlay, got {:?}",
            std::mem::discriminant(&app.mode)
        );

        // Press any key (a letter) → returns to Normal.
        app.handle_key(key(KeyCode::Char('x')));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "any key in Help should return to Normal, got {:?}",
            std::mem::discriminant(&app.mode)
        );

        // Re-enter Help and confirm 'q' also dismisses (no separate quit path).
        app.handle_key(key(KeyCode::Char('?')));
        assert!(matches!(app.mode, InputMode::Help));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "'q' in Help should return to Normal (Help has no sub-modes)"
        );

        // Re-enter Help and confirm Esc also dismisses.
        app.handle_key(key(KeyCode::Char('?')));
        assert!(matches!(app.mode, InputMode::Help));
        app.handle_key(key(KeyCode::Esc));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "Esc in Help should return to Normal"
        );

        // Re-enter Help and confirm Enter also dismisses.
        app.handle_key(key(KeyCode::Char('?')));
        assert!(matches!(app.mode, InputMode::Help));
        app.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "Enter in Help should return to Normal"
        );

        // Re-enter Help and confirm an arrow key also dismisses (any key returns).
        app.handle_key(key(KeyCode::Char('?')));
        assert!(matches!(app.mode, InputMode::Help));
        app.handle_key(key(KeyCode::Down));
        assert!(
            matches!(app.mode, InputMode::Normal),
            "Down arrow in Help should return to Normal"
        );
    });
}

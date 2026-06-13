//! P2 regression tests for the TUI Language Toggle and Theme Toggle keys.
//!
//! Plan source: `~/.claude/plans/runai-158-test-plan.md` §2.26 (Language Toggle)
//! and §2.27 (Theme Toggle). Each test drives the public `App` state machine
//! through `handle_key`, asserting the same invariants the in-app key handler
//! in `src/tui/app.rs:674-695` enforces.
//!
//! Skipped on Windows: `App::new` indirectly depends on HOME, and `dirs::home_dir()`
//! on Windows ignores the `HOME` env var (see AGENTS.md "Key constraints"). The
//! existing `src/tui/app.rs#[cfg(all(test, not(target_os = "windows")))]` unit
//! tests are gated the same way.
//!
//! Note on `Debug`: `Tab`, `Lang`, `ThemeMode`, and `InputMode` in `src/tui/`
//! intentionally do not derive `Debug`, so we use `matches!` + boolean asserts
//! rather than `assert_eq!`.
#![cfg(not(target_os = "windows"))]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::manager::SkillManager;
use runai::tui::app::{App, InputMode, Tab};
use runai::tui::i18n::Lang;
use std::sync::Mutex;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

// Process-local HOME lock for this integration test binary. Integration
// tests can't see the crate-private `test_support::HOME_LOCK`, so we run our
// own. Within this binary all tests run on one thread under
// `--test-threads=1`, but the lock is still defensive in case future tests
// in this file run code that touches HOME.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_home<F: FnOnce()>(tmp: &std::path::Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let original_home = std::env::var("HOME").ok();
    let original_rune = std::env::var("RUNE_DATA_DIR").ok();
    let original_sm = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
    // SAFETY: serialized by HOME_LOCK; restored in the closure below.
    unsafe {
        std::env::set_var("HOME", tmp);
        // We pass an explicit base to SkillManager::with_base; clearing these
        // env vars prevents the path resolver from overriding our base mid-test.
        std::env::remove_var("RUNE_DATA_DIR");
        std::env::remove_var("SKILL_MANAGER_DATA_DIR");
    }
    f();
    unsafe {
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match original_rune {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match original_sm {
            Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
            None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
        }
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Build a minimal App rooted at `tmp/data`, with no skills, in Normal mode
/// and on the Skills tab — the canonical starting state for a key-handler
/// regression test.
fn fresh_app(tmp: &std::path::Path) -> App {
    let mgr = SkillManager::with_base(tmp.join("data")).expect("SkillManager::with_base");
    let mut app = App::new(mgr);
    // `App::new` may flip mode to FirstLaunch when the data dir is empty —
    // override to Normal so 'l' / 't' are routed through `handle_normal_key`.
    app.mode = InputMode::Normal;
    app.tab = Tab::Skills;
    app
}

// ─── §2.26 Language Toggle ──────────────────────────────────────────────────

#[test]
fn language_toggle_zh_to_en() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());
        app.lang = Lang::Zh;
        assert!(
            matches!(app.tab, Tab::Skills),
            "precondition: must be on Skills tab"
        );
        assert!(
            matches!(app.lang, Lang::Zh),
            "precondition: lang seeded to Zh"
        );

        app.handle_key(key(KeyCode::Char('l')));

        assert!(
            matches!(app.lang, Lang::En),
            "lang must flip from Zh to En after pressing 'l' on a non-Groups tab"
        );

        // The English message is "Switched to English" — confirms i18n looked
        // up the NEW language's string, not the old one.
        let expected = app.t().msg_lang_switched().to_string();
        assert_eq!(
            expected, "Switched to English",
            "msg_lang_switched() should return English copy after toggle"
        );
        assert_eq!(
            app.message.as_deref(),
            Some(expected.as_str()),
            "app.message must be set to the post-toggle msg_lang_switched()"
        );
    });
}

#[test]
fn language_toggle_disabled_on_groups_tab() {
    let tmp = TempDir::new().unwrap();
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());
        app.lang = Lang::En;
        app.tab = Tab::Groups;
        app.message = None;

        app.handle_key(key(KeyCode::Char('l')));

        assert!(
            matches!(app.lang, Lang::En),
            "Groups tab must NOT toggle language on 'l' — the key is reserved \
             there for other actions per src/tui/app.rs:674"
        );

        // The message must not carry the language-switched notice. We accept
        // either no message, or a message that is something OTHER than
        // msg_lang_switched() (other handlers on Groups may emit unrelated
        // text — we only assert what we know must be absent).
        let lang_switched_zh = "已切换为中文";
        let lang_switched_en = "Switched to English";
        if let Some(m) = app.message.as_deref() {
            assert_ne!(
                m, lang_switched_en,
                "Groups tab must not produce the English lang-switched message"
            );
            assert_ne!(
                m, lang_switched_zh,
                "Groups tab must not produce the Chinese lang-switched message"
            );
        }
    });
}

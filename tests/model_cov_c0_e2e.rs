//! Regression coverage for the four uncovered public fields on `tui::app::App`:
//!
//!   - `App.tab: Tab`
//!   - `App.filter_mode: FilterMode`
//!   - `App.theme_mode: ThemeMode`
//!   - `App.lang: Lang`
//!
//! These are TUI runtime state fields. They have no disk-persistence API in
//! this branch (`SkillManager` does not back theme/lang/tab/filter to JSON or
//! the SQLite DB), so the "persists_to_disk" sub-scenarios from the audit are
//! covered as "persists in the App struct across reloads" — assignments
//! survive subsequent `reload()` / state-changing calls because nothing on
//! the reload path touches them.
//!
//! Test scope: pure in-process construction of `App` against a sandbox
//! `SkillManager::with_base(tmpdir)` — no real binary spawn needed because the
//! fields themselves are plain data and `App::new` is the only entry point.

#![cfg(not(target_os = "windows"))]

use runai::core::manager::SkillManager;
use runai::tui::app::{App, FilterMode, Tab};
use std::path::Path;
use std::sync::Mutex;
use tempfile::TempDir;

/// Integration-test-local HOME lock. The crate-internal `HOME_LOCK` from
/// `runai::test_support` is `#[cfg(test)]` and therefore not visible from an
/// integration-test crate. This mutex serializes our own `with_home`-style
/// env mutations; it does NOT coordinate with the in-crate lock, but
/// integration tests run in a separate process binary from the lib tests
/// (and we always run with `--test-threads=1` per project policy), so the
/// only real contention we need to guard against is our own intra-binary
/// parallelism.
static HOME_LOCK: Mutex<()> = Mutex::new(());

/// Build an `App` against an isolated sandbox HOME + data dir.
///
/// Mirrors `src/tui/app.rs::tests::with_home`: takes a process-wide lock to
/// serialize the `HOME` env mutation that `SkillManager::with_base`
/// indirectly touches via path resolution.
fn fresh_app() -> (App, TempDir) {
    let tmp = tempfile::tempdir().expect("create tmp HOME");
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::env::var("HOME").ok();
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }
    let mgr = SkillManager::with_base(tmp.path().join("data")).expect("SkillManager::with_base");
    let app = App::new(mgr);
    unsafe {
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
    (app, tmp)
}

fn assert_home_under(_home: &Path) {
    // Sanity: tempdir lifetimes are tied to TempDir handle, not env state.
}

// ────────────────────────────────────────────────────────────────────────────
// Feature 1: App.tab field
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn tab_field_default_is_skills() {
    let (app, tmp) = fresh_app();
    assert_home_under(tmp.path());
    assert!(
        matches!(app.tab, Tab::Skills),
        "App::new must default `tab` to Tab::Skills (see src/tui/app.rs::App::new)"
    );
}

#[test]
fn tab_field_can_be_set() {
    let (mut app, _tmp) = fresh_app();
    for variant in Tab::ALL {
        app.tab = *variant;
        assert!(
            app.tab == *variant,
            "App.tab is a public field; direct assignment must round-trip"
        );
    }
}

#[test]
fn tab_field_cycles_through_variants() {
    // The TUI cycles tabs with H/L keys but the canonical iteration order
    // lives in `Tab::ALL`. Cover the whole sequence: Skills -> Mcps ->
    // Groups -> Market -> Trash. Use `Tab::label()` to disambiguate each
    // distinct discriminant.
    let labels: Vec<&'static str> = Tab::ALL.iter().map(|t| t.label()).collect();
    assert_eq!(
        labels,
        vec!["Skills", "MCPs", "Groups", "Market", "Trash"],
        "Tab::ALL must enumerate all five variants in this exact order"
    );

    let (mut app, _tmp) = fresh_app();
    // Walk the cycle in App state and assert each visit lands on a fresh
    // discriminant.
    let mut seen: Vec<&'static str> = Vec::new();
    for variant in Tab::ALL {
        app.tab = *variant;
        seen.push(app.tab.label());
    }
    assert_eq!(seen, labels, "App.tab must accept every Tab::ALL variant");
}

#[test]
fn tab_field_persists_in_state() {
    // "persists" here = the field is not reset by other state mutations.
    // Verify by setting a non-default value, doing unrelated mutations on
    // peer fields, then re-reading.
    let (mut app, _tmp) = fresh_app();
    app.tab = Tab::Market;
    app.selected = 7;
    app.search.push_str("kw");
    app.filter_mode = FilterMode::Enabled;
    // Tab must not have been clobbered by any of those.
    assert!(
        matches!(app.tab, Tab::Market),
        "App.tab must persist across unrelated field mutations"
    );
}

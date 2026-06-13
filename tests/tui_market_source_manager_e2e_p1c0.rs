//! P1 regression coverage for the TUI Market Source Manager.
//!
//! Plan: `~/.claude/plans/runai-158-test-plan.md` §2.12 "Market Source Manager".
//!
//! Each test exercises `runai::tui::app::App` directly with an isolated `HOME`
//! tempdir and a `SkillManager` rooted under that tempdir, so we never touch
//! the real `~/.runai/` or `~/.{claude,codex,gemini,opencode}/skills/` paths
//! (Safety contract, AGENTS.md §"5 条铁律").
//!
//! HOME mocking is unix-only — see `AGENTS.md` Key Constraints (Windows
//! `dirs::home_dir` ignores env vars).
#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use runai::core::manager::SkillManager;
use runai::core::market::{MarketSkill, SourceEntry};
use runai::tui::app::{App, InputMode, Tab};

// `runai::test_support::HOME_LOCK` is `#[cfg(test)]`-gated, so integration
// tests cannot see it. Use a local serialization mutex — combined with
// `--test-threads=1` this is belt-and-suspenders against HOME bleed.
static HOME_LOCK: Mutex<()> = Mutex::new(());

// ─── helpers ────────────────────────────────────────────────────────────────

fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned: std::sync::PoisonError<_>| poisoned.into_inner());
    let original = std::env::var("HOME").ok();
    unsafe {
        std::env::set_var("HOME", tmp);
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    unsafe {
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn make_source(owner: &str, repo: &str, enabled: bool) -> SourceEntry {
    SourceEntry {
        owner: owner.into(),
        repo: repo.into(),
        branch: "main".into(),
        skill_prefix: String::new(),
        label: format!("{owner}/{repo}"),
        description: "test source".into(),
        builtin: false,
        enabled,
    }
}

fn make_skill(name: &str, source: &SourceEntry) -> MarketSkill {
    MarketSkill {
        name: name.into(),
        repo_path: format!("skills/{name}"),
        source_label: source.label.clone(),
        source_repo: source.repo_id(),
        branch: source.branch.clone(),
        installed: false,
    }
}

/// Build an App rooted at `tmp/data` with the Market tab active and a clean
/// `Normal` input mode (App::new defaults to FirstLaunch when the DB is
/// empty — we override here so key handling exercises the normal market
/// branch).
fn fresh_market_app(tmp: &Path) -> App {
    let mgr = SkillManager::with_base(tmp.join("data")).expect("init manager in tempdir");
    let mut app = App::new(mgr);
    app.mode = InputMode::Normal;
    app.tab = Tab::Market;
    // load_sources reads builtin defaults — clear so the tests deal with the
    // sources we install explicitly.
    app.sources.clear();
    app.market_cache = HashMap::new();
    app.market_source_idx = 0;
    app.selected = 0;
    app
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Plan §2.12.1: market_source_next_advances_index — pressing ']' on the
/// Market tab advances `market_source_idx`, refreshes `visible_market()` to
/// the next source's skills, and resets the row selection to 0.
#[test]
fn market_source_next_advances_index() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let mut app = fresh_market_app(tmp.path());

        let s0 = make_source("alpha", "one", true);
        let s1 = make_source("beta", "two", true);
        let s2 = make_source("gamma", "three", true);

        let s0_skill = make_skill("from-alpha", &s0);
        let s1_skill = make_skill("from-beta", &s1);
        let s2_skill = make_skill("from-gamma", &s2);

        app.market_cache.insert(s0.repo_id(), vec![s0_skill]);
        app.market_cache.insert(s1.repo_id(), vec![s1_skill]);
        app.market_cache.insert(s2.repo_id(), vec![s2_skill]);

        app.sources.push(s0);
        app.sources.push(s1.clone());
        app.sources.push(s2);

        app.market_source_idx = 0;
        // pretend the user had scrolled down on source 0 — switching must
        // reset selection.
        app.selected = 7;

        app.handle_key(key(KeyCode::Char(']')));

        assert_eq!(
            app.market_source_idx, 1,
            "next-source key should advance index by 1"
        );
        let visible = app.visible_market();
        assert_eq!(visible.len(), 1, "visible_market reflects new source");
        assert_eq!(
            visible[0].name, "from-beta",
            "visible_market reads from the new source's cache, not the previous"
        );
        assert_eq!(
            app.selected, 0,
            "row selection must reset to 0 on source switch"
        );
    });
}

/// Plan §2.12.2: market_source_wraparound_at_end — at the last source,
/// ']' wraps back to index 0; '[' on index 0 wraps to the last. No
/// out-of-bounds panic.
#[test]
fn market_source_wraparound_at_end() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let mut app = fresh_market_app(tmp.path());

        let s0 = make_source("alpha", "one", true);
        let s1 = make_source("beta", "two", true);
        let s2 = make_source("gamma", "three", true);

        let s0_skill = make_skill("from-alpha", &s0);
        let s2_skill = make_skill("from-gamma", &s2);

        app.market_cache.insert(s0.repo_id(), vec![s0_skill]);
        app.market_cache.insert(s2.repo_id(), vec![s2_skill]);

        app.sources.push(s0.clone());
        app.sources.push(s1);
        app.sources.push(s2);

        // Start at the last enabled source.
        app.market_source_idx = 2;
        app.selected = 3;

        // Forward wrap: 2 → 0.
        app.handle_key(key(KeyCode::Char(']')));

        assert_eq!(
            app.market_source_idx, 0,
            "wrap-around: next from last source should land on first"
        );
        let visible = app.visible_market();
        assert_eq!(
            visible.len(),
            1,
            "wrap-around displays the first source's skills"
        );
        assert_eq!(visible[0].name, "from-alpha");
        assert_eq!(app.selected, 0, "selection resets on wrap-around");

        // Reverse wrap: 0 → last index (2).
        app.handle_key(key(KeyCode::Char('[')));
        assert_eq!(
            app.market_source_idx, 2,
            "wrap-around: prev from index 0 should land on the last source"
        );
        assert_eq!(
            app.selected, 0,
            "selection resets on reverse wrap-around too"
        );
    });
}

/// Plan §2.12.3: market_source_noop_when_empty — with zero enabled sources,
/// '[' and ']' are no-ops: index unchanged, `visible_market()` empty, no
/// panic.
#[test]
fn market_source_noop_when_empty() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let mut app = fresh_market_app(tmp.path());

        // Add sources but mark them all disabled — `enabled_sources()`
        // filters by `.enabled`, so this is the "no enabled source" case.
        app.sources.push(make_source("alpha", "one", false));
        app.sources.push(make_source("beta", "two", false));

        app.market_source_idx = 0;
        app.selected = 0;

        // Neither key should advance the index or panic.
        app.handle_key(key(KeyCode::Char(']')));
        assert_eq!(
            app.market_source_idx, 0,
            "no enabled sources: ']' must not advance index"
        );

        app.handle_key(key(KeyCode::Char('[')));
        assert_eq!(
            app.market_source_idx, 0,
            "no enabled sources: '[' must not change index"
        );

        let visible = app.visible_market();
        assert!(
            visible.is_empty(),
            "visible_market must be empty with no enabled sources"
        );
        // Sanity: `enabled_sources()` returns empty so `current_source` is None.
        assert!(
            app.current_source().is_none(),
            "no enabled sources: current_source is None"
        );
    });
}

/// Plan §2.12.4: market_source_manager_overlay_opens — pressing 's' on the
/// Market tab switches `InputMode` to `SourceManager` and resets
/// `source_pick_idx` to 0.
#[test]
fn market_source_manager_overlay_opens() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let mut app = fresh_market_app(tmp.path());

        app.sources.push(make_source("alpha", "one", true));
        app.sources.push(make_source("beta", "two", true));
        // Leave source_pick_idx non-zero so we can prove the reset.
        app.source_pick_idx = 5;
        assert!(matches!(app.mode, InputMode::Normal));

        app.handle_key(key(KeyCode::Char('s')));

        assert!(
            matches!(app.mode, InputMode::SourceManager),
            "'s' on Market tab should switch into SourceManager overlay"
        );
        assert_eq!(
            app.source_pick_idx, 0,
            "opening the source manager must reset source_pick_idx to 0"
        );
        // Source list itself must remain populated for subsequent
        // navigation inside the overlay.
        assert_eq!(
            app.sources.len(),
            2,
            "sources list preserved on overlay open"
        );
    });
}

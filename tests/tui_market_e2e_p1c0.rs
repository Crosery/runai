//! P1 e2e regressions for the TUI Market List & Search surface.
//!
//! Drives the public [`runai::tui::app::App`] state machine with isolated
//! HOME / RUNE_DATA_DIR so the tests touch only a tempdir. Each test asserts
//! on the visible_market() / selected behavior exposed by the App API; no
//! filesystem mutation happens outside the tempdir.
#![cfg(not(target_os = "windows"))]

use std::sync::Mutex;

use runai::core::manager::SkillManager;
use runai::core::market::{MarketSkill, SourceEntry};
use runai::tui::app::{App, InputMode, Tab};
use tempfile::TempDir;

/// Serialize tests that mutate HOME — App::new reads dirs::home_dir() under
/// the hood for some lookups (market sources file location), and parallel
/// HOME swaps would race.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_isolated_home<F: FnOnce(&TempDir)>(f: F) {
    let guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let tmp = TempDir::new().expect("tempdir");
    let original_home = std::env::var("HOME").ok();
    let original_runedd = std::env::var("RUNE_DATA_DIR").ok();
    let original_smdd = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
    unsafe {
        std::env::set_var("HOME", tmp.path());
        std::env::set_var("RUNE_DATA_DIR", tmp.path().join(".runai"));
        std::env::remove_var("SKILL_MANAGER_DATA_DIR");
    }
    f(&tmp);
    unsafe {
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match original_runedd {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match original_smdd {
            Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
            None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
        }
    }
    drop(guard);
}

fn mk_skill(name: &str, src_repo: &str, src_label: &str) -> MarketSkill {
    MarketSkill {
        name: name.into(),
        repo_path: format!("skills/{name}"),
        source_label: src_label.into(),
        source_repo: src_repo.into(),
        branch: "main".into(),
        installed: false,
    }
}

fn make_source(owner: &str, repo: &str, label: &str) -> SourceEntry {
    SourceEntry {
        owner: owner.into(),
        repo: repo.into(),
        branch: "main".into(),
        skill_prefix: String::new(),
        label: label.into(),
        description: "test source".into(),
        builtin: false,
        enabled: true,
    }
}

fn app_in_market_tab(tmp: &TempDir) -> App {
    let mgr = SkillManager::with_base(tmp.path().join(".runai")).expect("manager");
    let mut app = App::new(mgr);
    app.tab = Tab::Market;
    // App::new chooses InputMode::FirstLaunch when the data dir is empty;
    // force Normal so search / source-switch keys dispatch.
    app.mode = InputMode::Normal;
    // Wipe whatever sources the default `load_sources` produced (they may try
    // to fetch from the network later) and inject a single deterministic one.
    app.sources.clear();
    app.market_source_idx = 0;
    app
}

// ─── test 1: search filters by keyword (case insensitive) ───────────────────

#[test]
fn market_search_filters_by_keyword() {
    with_isolated_home(|tmp| {
        let mut app = app_in_market_tab(tmp);
        let src = make_source("acme", "test", "acme/test");
        let repo_id = src.repo_id();
        app.sources.push(src);

        app.market_cache.insert(
            repo_id,
            vec![
                mk_skill("alpha-skill", "acme/test", "acme/test"),
                mk_skill("beta-skill", "acme/test", "acme/test"),
                mk_skill("gamma-skill", "acme/test", "acme/test"),
            ],
        );

        // Empty search => all 3
        app.search.clear();
        assert_eq!(app.visible_market().len(), 3, "empty search shows all");

        // 'alpha' => only alpha-skill
        app.search = "alpha".into();
        let v = app.visible_market();
        assert_eq!(v.len(), 1, "alpha search should match 1");
        assert_eq!(v[0].name, "alpha-skill");

        // 'beta' => only beta-skill
        app.search = "beta".into();
        let v = app.visible_market();
        assert_eq!(v.len(), 1, "beta search should match 1");
        assert_eq!(v[0].name, "beta-skill");

        // Case-insensitivity: uppercase still finds it
        app.search = "ALPHA".into();
        let v = app.visible_market();
        assert_eq!(v.len(), 1, "uppercase search should still match");
        assert_eq!(v[0].name, "alpha-skill");

        // Mixed-case
        app.search = "GaMmA".into();
        let v = app.visible_market();
        assert_eq!(v.len(), 1, "mixed-case search should still match");
        assert_eq!(v[0].name, "gamma-skill");

        // No match => empty
        app.search = "zzz-nonexistent".into();
        assert_eq!(app.visible_market().len(), 0, "no match returns empty");
    });
}

// ─── test 2: selection stays in-range after search shrinks the list ─────────

#[test]
fn market_search_clamps_selection() {
    with_isolated_home(|tmp| {
        let mut app = app_in_market_tab(tmp);
        let src = make_source("acme", "five", "acme/five");
        let repo_id = src.repo_id();
        app.sources.push(src);

        let skills: Vec<MarketSkill> = ["one", "two", "three", "four", "five"]
            .iter()
            .map(|n| mk_skill(n, "acme/five", "acme/five"))
            .collect();
        app.market_cache.insert(repo_id, skills);

        // Position selected at the last item (idx 4 of 5).
        app.selected = 4;
        assert_eq!(app.visible_count(), 5);

        // Drive the search input pipeline via real key events: `/` enters
        // search mode, then each char appends. The search handler resets
        // selected to 0 on every keystroke; that's the actual in-tree
        // clamping behavior. After search filters the list down to <5,
        // selected must remain in range and visible_count() must reflect
        // the shrunk list.
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mk = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

        // Open search mode
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Search));

        // Type "thr" -> filters to ["three"] (length 1)
        for c in ['t', 'h', 'r'] {
            app.handle_key(mk(c));
        }
        let vc = app.visible_count();
        assert_eq!(vc, 1, "filter 'thr' should match only 'three'");
        assert!(
            app.selected < vc.max(1),
            "selected ({}) must stay within visible_count ({}) after search shrinks list",
            app.selected,
            vc
        );

        // Backspace down to "t" -> filters to ["two", "three"]
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        let vc = app.visible_count();
        assert_eq!(vc, 2, "filter 't' should match 'two' and 'three'");
        assert!(
            app.selected < vc,
            "selected ({}) must stay within visible_count ({})",
            app.selected,
            vc
        );

        // Backspace to empty -> filter clears, all 5 visible again
        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert_eq!(app.visible_count(), 5);
        assert!(app.selected < 5);

        // Esc to leave search mode
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(app.mode, InputMode::Normal));
    });
}

// ─── test 3: switching sources keeps the same search active ─────────────────
// cargo_integration: verifies multi-source market search isolation; switching
// to a different source applies the SAME search query to the NEW source's
// candidate list (no result reuse). Uses the real binary indirectly via
// `App::new` (constructs the real SkillManager + market loader), but does
// not spawn any subprocess — the App API is the surface under test.

#[test]
fn market_search_respects_source_switch() {
    with_isolated_home(|tmp| {
        let mut app = app_in_market_tab(tmp);

        // Source 1: contains alpha + beta
        let src1 = make_source("orgA", "repo1", "orgA/repo1");
        let id1 = src1.repo_id();
        // Source 2: contains alpha-different + gamma (one alpha match, one not)
        let src2 = make_source("orgB", "repo2", "orgB/repo2");
        let id2 = src2.repo_id();

        app.sources.push(src1);
        app.sources.push(src2);

        app.market_cache.insert(
            id1.clone(),
            vec![
                mk_skill("alpha-one", "orgA/repo1", "orgA/repo1"),
                mk_skill("beta-one", "orgA/repo1", "orgA/repo1"),
            ],
        );
        app.market_cache.insert(
            id2.clone(),
            vec![
                mk_skill("alpha-two", "orgB/repo2", "orgB/repo2"),
                mk_skill("alpha-three", "orgB/repo2", "orgB/repo2"),
                mk_skill("gamma-two", "orgB/repo2", "orgB/repo2"),
            ],
        );

        // Start on source 1 with search="alpha" → 1 hit
        app.market_source_idx = 0;
        app.search = "alpha".into();
        let v = app.visible_market();
        assert_eq!(v.len(), 1, "source 1 has 1 alpha skill");
        assert_eq!(v[0].name, "alpha-one");

        // Switch to source 2 via the `]` key. Search query must still be "alpha".
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        app.handle_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE));
        assert_eq!(app.market_source_idx, 1, "next source on `]`");
        assert_eq!(
            app.search, "alpha",
            "search query is preserved across switch"
        );

        // Now source 2 should show only its alpha matches (2 of them), NOT
        // re-using source 1's filtered cache.
        let v = app.visible_market();
        assert_eq!(v.len(), 2, "source 2 has 2 alpha skills");
        let names: Vec<&str> = v.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"alpha-two"));
        assert!(names.contains(&"alpha-three"));
        // Source 1's "alpha-one" must not bleed in
        assert!(
            !names.contains(&"alpha-one"),
            "source 1's items must not appear in source 2 view"
        );

        // Switch back with `[`
        app.handle_key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE));
        assert_eq!(app.market_source_idx, 0);
        let v = app.visible_market();
        assert_eq!(v.len(), 1, "back to source 1, 1 alpha skill");
        assert_eq!(v[0].name, "alpha-one");

        // Selection is reset to 0 on switch (App::handle_key spec).
        assert_eq!(app.selected, 0);
    });
}

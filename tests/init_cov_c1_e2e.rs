//! Coverage tests for `src/tui/app.rs` `App::visible_items`.
//!
//! Task chunk c1 from the W2 audit asked for tests on four `App::*` reload /
//! visibility APIs. Only `App::visible_items` exists in this release branch
//! source tree (the audit catalog referenced a folder-split `tui/app/init.rs`
//! that does not exist here — `src/tui/app.rs` is one file). The other three
//! features (`reload_trash`, `reload_market`, `reload_hook_status`) are
//! genuinely absent from HEAD; per the workflow rules I do not fabricate
//! tests against them.
//!
//! Skipped on Windows: `App` construction goes through `SkillManager` which
//! relies on HOME mocking through the `HOME` env var; on Windows
//! `dirs::home_dir()` ignores HOME (Win32 `SHGetKnownFolderPath`), so the
//! existing project tests for the same module are also gated this way.
#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use runai::core::cli_target::CliTarget;
use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind, Source};
use runai::tui::app::{App, FilterMode, Tab};

// ─── helpers ────────────────────────────────────────────────────────────────

/// Serialize tests that mutate HOME so they do not race when
/// `cargo test -- --test-threads=N` is given a value other than 1. The CI
/// gate uses `--test-threads=1` but we still defensively serialize here.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_home<R>(tmp: &Path, f: impl FnOnce() -> R) -> R {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::env::var("HOME").ok();
    // SAFETY: lock above prevents concurrent HOME mutation; restored after f().
    unsafe {
        std::env::set_var("HOME", tmp);
    }
    let result = f();
    unsafe {
        match original {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
    result
}

/// Build a fresh `App` against an isolated data dir, then *replace*
/// `app.items` with a hand-crafted vector whose `enabled` map is exactly what
/// the test wants. Real `reload()` would refresh `enabled` from on-disk
/// symlinks; `visible_items` is a pure read against `app.items` so we test
/// it that way to avoid the symlink-truth tangle.
fn fresh_app_with_items(tmp: &Path, items: Vec<Resource>) -> App {
    let mgr = SkillManager::with_base(tmp.join("data")).expect("init SkillManager");
    let mut app = App::new(mgr);
    app.tab = Tab::Skills;
    app.items = items;
    app
}

fn skill(
    id: &str,
    name: &str,
    desc: &str,
    enabled: HashMap<CliTarget, bool>,
    dir_base: &Path,
) -> Resource {
    let dir: PathBuf = dir_base.join("skills").join(name);
    Resource {
        id: id.into(),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: desc.into(),
        directory: dir.clone(),
        source: Source::Local { path: dir },
        installed_at: 0,
        enabled,
        usage_count: 0,
        last_used_at: None,
    }
}

fn names(app: &App) -> Vec<String> {
    app.visible_items().iter().map(|r| r.name.clone()).collect()
}

fn enabled_for(target: CliTarget) -> HashMap<CliTarget, bool> {
    let mut m = HashMap::new();
    m.insert(target, true);
    m
}

// ─── App::visible_items ────────────────────────────────────────────────────

#[test]
fn visible_items_returns_all_skills_with_no_filter_and_empty_search() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let dir_base = tmp.path().join("data");
        let items = vec![
            skill(
                "local:alpha",
                "alpha",
                "first skill banana flavor",
                enabled_for(CliTarget::Claude),
                &dir_base,
            ),
            skill(
                "local:beta",
                "beta",
                "second skill cherry flavor",
                HashMap::new(),
                &dir_base,
            ),
            skill(
                "local:gamma",
                "gamma",
                "third skill apple flavor",
                enabled_for(CliTarget::Codex),
                &dir_base,
            ),
        ];
        let app = fresh_app_with_items(tmp.path(), items);

        // FilterMode::All + empty search ⇒ everything in items is visible,
        // in the same order, with the same names.
        let got = names(&app);
        assert_eq!(got, vec!["alpha", "beta", "gamma"]);
    });
}

#[test]
fn visible_items_filters_by_search_query_against_name_and_description() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let dir_base = tmp.path().join("data");
        let items = vec![
            skill(
                "local:alpha",
                "alpha",
                "first skill banana flavor",
                enabled_for(CliTarget::Claude),
                &dir_base,
            ),
            skill(
                "local:beta",
                "beta",
                "second skill cherry flavor",
                HashMap::new(),
                &dir_base,
            ),
            skill(
                "local:gamma",
                "gamma",
                "third skill apple flavor",
                enabled_for(CliTarget::Codex),
                &dir_base,
            ),
        ];
        let mut app = fresh_app_with_items(tmp.path(), items);

        // Match by name fragment.
        app.search = "alph".into();
        assert_eq!(names(&app), vec!["alpha".to_string()]);

        // Match by description fragment — "banana" only appears in alpha's
        // description.
        app.search = "banana".into();
        assert_eq!(
            names(&app),
            vec!["alpha".to_string()],
            "description-fragment match should select alpha only"
        );

        // Case-insensitive: uppercase query must still match (visible_items
        // lowercases the query before substring-matching).
        app.search = "BETA".into();
        assert_eq!(names(&app), vec!["beta".to_string()]);

        // No-match returns empty.
        app.search = "doesnotexist-xyz".into();
        assert!(app.visible_items().is_empty());
    });
}

#[test]
fn visible_items_filters_by_filter_mode_enabled_and_disabled() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let dir_base = tmp.path().join("data");
        let items = vec![
            skill(
                "local:alpha",
                "alpha",
                "first skill banana flavor",
                enabled_for(CliTarget::Claude),
                &dir_base,
            ),
            skill(
                "local:beta",
                "beta",
                "second skill cherry flavor",
                HashMap::new(),
                &dir_base,
            ),
            skill(
                "local:gamma",
                "gamma",
                "third skill apple flavor",
                enabled_for(CliTarget::Codex),
                &dir_base,
            ),
        ];
        let mut app = fresh_app_with_items(tmp.path(), items);
        app.active_target = CliTarget::Claude;
        app.search.clear();

        // FilterMode::All — see all three.
        app.filter_mode = FilterMode::All;
        assert_eq!(names(&app), vec!["alpha", "beta", "gamma"]);

        // FilterMode::Enabled (target=Claude) — only alpha.
        app.filter_mode = FilterMode::Enabled;
        assert_eq!(names(&app), vec!["alpha".to_string()]);

        // FilterMode::Disabled (target=Claude) — beta + gamma (gamma's
        // Codex-enabled does not count toward Claude).
        app.filter_mode = FilterMode::Disabled;
        let mut disabled = names(&app);
        disabled.sort();
        assert_eq!(disabled, vec!["beta".to_string(), "gamma".to_string()]);

        // Switch active_target to Codex — gamma is now the enabled one.
        app.active_target = CliTarget::Codex;
        app.filter_mode = FilterMode::Enabled;
        assert_eq!(names(&app), vec!["gamma".to_string()]);

        // FilterMode::Disabled(Codex) — alpha + beta.
        app.filter_mode = FilterMode::Disabled;
        let mut disabled_codex = names(&app);
        disabled_codex.sort();
        assert_eq!(
            disabled_codex,
            vec!["alpha".to_string(), "beta".to_string()]
        );
    });
}

#[test]
fn visible_items_combines_search_and_filter_mode() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let dir_base = tmp.path().join("data");
        let items = vec![
            skill(
                "local:alpha",
                "alpha",
                "first skill banana flavor",
                enabled_for(CliTarget::Claude),
                &dir_base,
            ),
            skill(
                "local:beta",
                "beta",
                "second skill cherry flavor",
                HashMap::new(),
                &dir_base,
            ),
            skill(
                "local:gamma",
                "gamma",
                "third skill apple flavor",
                enabled_for(CliTarget::Codex),
                &dir_base,
            ),
        ];
        let mut app = fresh_app_with_items(tmp.path(), items);
        app.active_target = CliTarget::Claude;

        // search "flavor" hits all three (each desc has "flavor"); filter
        // Enabled(Claude) narrows to alpha only.
        app.search = "flavor".into();
        app.filter_mode = FilterMode::Enabled;
        assert_eq!(names(&app), vec!["alpha".to_string()]);

        // search "apple" matches only gamma; filter Enabled(Claude) drops
        // gamma → empty.
        app.search = "apple".into();
        app.filter_mode = FilterMode::Enabled;
        assert!(
            app.visible_items().is_empty(),
            "gamma is not enabled for Claude so combined filter must be empty"
        );

        // Same query under FilterMode::Disabled(Claude) → gamma re-appears.
        app.filter_mode = FilterMode::Disabled;
        assert_eq!(names(&app), vec!["gamma".to_string()]);

        // Empty search + FilterMode::All — confirm baseline still includes
        // every row when the two filters relax.
        app.search.clear();
        app.filter_mode = FilterMode::All;
        assert_eq!(app.visible_items().len(), 3);
    });
}

#[test]
fn visible_items_returns_empty_when_no_resources_exist() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let app = fresh_app_with_items(tmp.path(), Vec::new());
        assert!(
            app.visible_items().is_empty(),
            "empty items list ⇒ visible_items must be empty"
        );
    });
}

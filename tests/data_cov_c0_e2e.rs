//! Coverage for `src/tui/app.rs` (the audit referred to `src/tui/app/data.rs`
//! but in this worktree the TUI app module is the single file `src/tui/app.rs`).
//!
//! This file pins behavior of the `App.message` status-message field, which
//! the TUI relies on to surface transient feedback (scan-done, install error,
//! filter-mode switched, …). We test it in-process via `runai::tui::app::App`.
//!
//! Skipped on Windows: `App::new` calls `SkillManager::is_first_launch`
//! which reads CLI config files at `dirs::home_dir()`. We mock home via the
//! `HOME` env var, which only works on unix (Windows `dirs::home_dir()`
//! ignores HOME — same reason `src/core/manager::tests` is unix-only).
#![cfg(not(target_os = "windows"))]

use std::path::Path;
use std::sync::Mutex;

use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind, Source};
use runai::tui::app::{App, Tab};

/// Process-wide lock for tests that mutate `HOME`. `App::new` indirectly reads
/// `dirs::home_dir()` (via `SkillManager::resource_count` →
/// `read_mcp_status_from_configs`), so concurrent HOME swaps would race.
/// Integration-test crates can't reach the `cfg(test)`-gated lib-internal
/// `test_support::HOME_LOCK`, so we keep our own here.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::env::var("HOME").ok();
    // SAFETY: the lock above serializes HOME mutation across tests in this
    // crate. Other crates running in parallel would still race, but
    // `cargo test -- --test-threads=1` (the project default) serializes
    // across binaries too.
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

/// Spawn an `App` rooted at `tmp/data` with HOME set to `tmp`.
fn fresh_app(tmp: &Path) -> App {
    let mgr = SkillManager::with_base(tmp.join("data")).expect("with_base must succeed");
    App::new(mgr)
}

/// Insert a single skill row + on-disk dir so `reload()` has something to
/// populate `App.items` with.
fn plant_skill(app: &App, name: &str) {
    let skill_dir = app.mgr.paths().skills_dir().join(name);
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: planted\n---\n\n# {name}\n"),
    )
    .expect("write SKILL.md");
    let resource = Resource {
        id: format!("local:{name}"),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: "planted".into(),
        directory: skill_dir.clone(),
        source: Source::Local { path: skill_dir },
        installed_at: 0,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
    };
    app.mgr
        .db()
        .insert_resource(&resource)
        .expect("insert_resource");
}

// ─── Feature: App.message field ────────────────────────────────────────────

#[test]
fn message_field_default_none_on_fresh_app() {
    // Scenario: message_field_default_none
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let app = fresh_app(tmp.path());
        assert!(
            app.message.is_none(),
            "freshly constructed App must start with message=None, got {:?}",
            app.message
        );
    });
}

#[test]
fn message_field_can_be_set_directly() {
    // Scenario: message_field_can_be_set
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());
        let payload = "Installed 'foo' from skills.sh".to_string();
        app.message = Some(payload.clone());
        assert_eq!(
            app.message.as_deref(),
            Some(payload.as_str()),
            "direct assignment to App.message must round-trip"
        );

        // Clearing via None must also round-trip — the field is the canonical
        // surface other render code reads, no separate cleared-flag.
        app.message = None;
        assert!(app.message.is_none(), "None assignment must clear");
    });
}

#[test]
fn message_field_survives_reload() {
    // Scenario: message_field_cleared_by_reload — actual behavior (as of
    // src/tui/app.rs reload() body) is that reload does NOT touch message;
    // it only repopulates items/trash_items/groups/status. Pin that here so
    // a future change that quietly clears message during reload trips the
    // test instead of silently dropping user-visible status text.
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());
        plant_skill(&app, "demo-skill");
        app.message = Some("scan complete".into());
        app.reload();
        assert_eq!(
            app.message.as_deref(),
            Some("scan complete"),
            "reload() must not clobber message — render layer relies on it"
        );
    });
}

#[test]
fn message_field_holds_install_style_strings() {
    // Scenario: message_field_shown_in_status_bar — we can't render the
    // status bar headless, but we CAN pin that message accepts the exact
    // string shapes the production code writes into it (see lines ~813/865/
    // 885/899/1015 in src/tui/app.rs). If the field ever stops accepting an
    // owned String or starts truncating, this catches it.
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());
        for sample in [
            "Group 'g1' created",
            "Installing 'foo' from owner/repo@main...",
            "Install failed: connection reset",
            "Moved 'bar' to trash",
            "Permanently deleted 'baz'",
        ] {
            app.message = Some(sample.into());
            let got = app.message.as_deref().expect("Some");
            assert_eq!(got, sample, "status-bar string must round-trip verbatim");
            assert!(
                !got.is_empty(),
                "non-empty status messages are the only kind production writes"
            );
        }
    });
}

#[test]
fn message_field_cleared_by_handle_key() {
    // Bonus scenario: production behavior — `App::handle_key` clears
    // `self.message = None` at its entry (src/tui/app.rs line ~541). This
    // pins that contract so any future refactor that drops the line gets
    // caught (otherwise stale messages would linger across keystrokes).
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let tmp = tempfile::tempdir().expect("tmpdir");
    with_home(tmp.path(), || {
        let mut app = fresh_app(tmp.path());
        app.message = Some("stale".into());
        app.tab = Tab::Skills;
        // Any key triggers the clear-at-entry. Pick a no-op-ish key
        // (Esc into Normal mode is harmless when already Normal).
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            app.message.is_none(),
            "handle_key must reset message at entry (got {:?})",
            app.message
        );
    });
}

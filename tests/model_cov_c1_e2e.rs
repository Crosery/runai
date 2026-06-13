//! Coverage for `App` state fields in `src/tui/app.rs`.
//!
//! The audit lists `App.target` against a `model.rs` path that does not
//! exist in HEAD; the real TUI `App` struct lives in `src/tui/app.rs`
//! and exposes the semantically equivalent field
//! `active_target: CliTarget` (line 257). Tests here lock that field's
//! construction-time default, mutability across every `CliTarget`
//! variant, observable effect on `reload()` / `status`, and persistence
//! across `tab` swaps.
#![cfg(not(target_os = "windows"))]

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

use runai::core::cli_target::CliTarget;
use runai::core::manager::SkillManager;
use runai::core::resource::{Resource, ResourceKind, Source};
use runai::tui::app::{App, Tab};

// HOME-mutating tests serialize on this lock so concurrent tests in this
// file never race on `std::env::set_var("HOME", ...)`. `cargo test --
// --test-threads=1` already serializes globally, but the lock keeps the
// invariant local and survives looser harness configs.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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

fn make_managed_skill(base: &Path, name: &str) -> Resource {
    let skill_dir = base.join("skills").join(name);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: demo\n---\n\n# {name}\n"),
    )
    .unwrap();
    Resource {
        id: format!("local:{name}"),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: "demo".into(),
        directory: skill_dir.clone(),
        source: Source::Local {
            path: skill_dir.clone(),
        },
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
    }
}

// ────────────────────────────────────────────────────────────────────────────
// App.active_target  (audit: "App.target")
// ────────────────────────────────────────────────────────────────────────────

#[test]
fn target_field_default_is_claude() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("data")).unwrap();
        let app = App::new(mgr);
        assert_eq!(
            app.active_target,
            CliTarget::Claude,
            "freshly constructed App must default to CliTarget::Claude"
        );
    });
}

#[test]
fn target_field_can_be_switched_to_every_variant() {
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("data")).unwrap();
        let mut app = App::new(mgr);
        for t in CliTarget::ALL {
            app.active_target = *t;
            assert_eq!(
                app.active_target, *t,
                "active_target must accept every CliTarget variant"
            );
        }
    });
}

#[test]
fn target_field_drives_status_resource_listing() {
    // status() is parameterized by active_target. Insert a managed skill,
    // then verify the (enabled, total) tuple is computed against the
    // currently-set target after each reload().
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let data = tmp.path().join("data");
        let mgr = SkillManager::with_base(data.clone()).unwrap();
        let resource = make_managed_skill(&data, "demo-target-skill");
        mgr.db().insert_resource(&resource).unwrap();

        let mut app = App::new(mgr);

        // Claude target: skill is not symlinked into ~/.claude/skills, so
        // status() reports 0 enabled. (Total is 1 because the row exists.)
        app.active_target = CliTarget::Claude;
        app.reload();
        let (claude_es, claude_ts, _, _) = app.status;
        assert_eq!(claude_ts, 1, "total skills must reflect inserted row");
        assert_eq!(
            claude_es, 0,
            "no symlink → status reports 0 enabled for Claude"
        );

        // Switch to Codex: enabled count is still 0 (no symlink either),
        // but the field swap must succeed and reload() must rerun status().
        app.active_target = CliTarget::Codex;
        app.reload();
        let (codex_es, codex_ts, _, _) = app.status;
        assert_eq!(codex_ts, 1, "total skills must persist across target swap");
        assert_eq!(
            codex_es, 0,
            "no symlink → status reports 0 enabled for Codex too"
        );
    });
}

#[test]
fn target_field_persists_across_tab_change() {
    // Switching `app.tab` does not implicitly reset `active_target` — the
    // field is independent of which tab the user is viewing.
    let tmp = tempfile::tempdir().unwrap();
    with_home(tmp.path(), || {
        let mgr = SkillManager::with_base(tmp.path().join("data")).unwrap();
        let mut app = App::new(mgr);
        app.active_target = CliTarget::Gemini;

        app.tab = Tab::Mcps;
        assert_eq!(
            app.active_target,
            CliTarget::Gemini,
            "tab change must not clobber active_target"
        );
        app.tab = Tab::Market;
        assert_eq!(app.active_target, CliTarget::Gemini);
        app.tab = Tab::Trash;
        assert_eq!(app.active_target, CliTarget::Gemini);
        app.tab = Tab::Skills;
        assert_eq!(app.active_target, CliTarget::Gemini);
    });
}

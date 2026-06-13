//! Physical end-to-end coverage of the TUI CLI Target switching key path
//! (PLANNING test plan §2.28).
//!
//! Driving `App` directly (not the rendered terminal) is the same harness used
//! by `tests/tui_hook_panel_test.rs`: a tempdir HOME, a `SkillManager` rooted
//! under the tempdir's `.runai/`, and crossterm key events fed through
//! `App::handle_key`. The test plan calls for two physical_e2e scenarios:
//!
//!   1. Pressing `1`/`2`/`3`/`4` flips `active_target` to
//!      `Claude`/`Codex`/`Gemini`/`OpenCode`, and after each press `reload()`
//!      runs so `visible_items()` reflects the symlink state of the new
//!      target.
//!   2. The same flow holds under a non-default `RUNE_DATA_DIR` — the safety
//!      contract's double-run requirement for any code that talks to the
//!      `<data>/skills/` pool plus the 4 CLI symlink farms.
//!
//! Unix-only: `dirs::home_dir()` ignores `HOME` on Windows (`dirs` 6.x uses
//! the Win32 KnownFolder API), and symlinks need Developer Mode there. The
//! whole project's `manager::tests` module is gated the same way.
#![cfg(not(target_os = "windows"))]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use runai::core::cli_target::CliTarget;
use runai::core::manager::SkillManager;
use runai::tui::app::{App, InputMode, Tab};
use std::path::Path;

/// Process-wide HOME serialization. `dirs::home_dir()` reads `HOME` on unix,
/// so concurrent setters from a parallel test would step on each other.
/// Same idea as the lock in `tui_hook_panel_test.rs`.
static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Plant a minimal valid skill directory under `<base>/skills/<name>/`.
fn plant_skill(base: &Path, name: &str) {
    let dir = base.join("skills").join(name);
    std::fs::create_dir_all(&dir).expect("plant skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {name} test fixture\n---\n\n# {name}\n"),
    )
    .expect("plant SKILL.md");
}

/// Build an App rooted at a tempdir HOME with a known skill set.
///
/// `data_subpath` lets the caller route the managed data dir to a non-default
/// location (the `RUNE_DATA_DIR` half of the safety double-run). Pass
/// `".runai"` for the default-home case.
///
/// Returns (app, home tempdir, data path, lock guard). The tempdir and lock
/// guard must outlive the test — the tempdir backs HOME and the guard
/// serializes HOME across tests.
fn make_app(
    data_subpath: &str,
) -> (
    App,
    tempfile::TempDir,
    std::path::PathBuf,
    std::sync::MutexGuard<'static, ()>,
) {
    let guard = HOME_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().expect("create tmp HOME");
    unsafe {
        std::env::set_var("HOME", home.path());
    }
    // Pre-create the 4 CLI skills dirs so enable_resource has somewhere to
    // place its symlinks (it does `create_dir_all`, but pre-creating mirrors
    // safety_e2e's TestEnv and makes the "did the right target get touched"
    // assertions easier to reason about).
    for cli in ["claude", "codex", "gemini", "opencode"] {
        std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
            .expect("pre-create CLI skills dir");
    }

    let data = home.path().join(data_subpath);
    std::fs::create_dir_all(&data).expect("create data dir");

    // Plant both fixtures under the managed pool BEFORE constructing the
    // manager; `register_local_skill` requires the directory to exist.
    plant_skill(&data, "test-skill-a");
    plant_skill(&data, "test-skill-b");

    let mgr = SkillManager::with_base(data.clone()).expect("manager init");
    mgr.register_local_skill("test-skill-a")
        .expect("register test-skill-a");
    mgr.register_local_skill("test-skill-b")
        .expect("register test-skill-b");

    // Enable test-skill-a on Claude, Codex, Gemini; leave OpenCode + every
    // test-skill-b symlink absent so the four targets are observably
    // different after the switch.
    let id_a = mgr
        .find_resource_id("test-skill-a")
        .expect("find_resource_id for test-skill-a");
    for t in [CliTarget::Claude, CliTarget::Codex, CliTarget::Gemini] {
        mgr.enable_resource(&id_a, t, None)
            .unwrap_or_else(|e| panic!("enable test-skill-a on {}: {e}", t.name()));
    }

    let mut app = App::new(mgr);
    // App::new may park us in FirstLaunch when the manager reports zero
    // resources. We just registered two skills, so resource_count > 0 and
    // is_first_launch is false — but force Normal mode anyway, mirroring
    // tui_hook_panel_test.rs, in case the count check ever changes.
    app.mode = InputMode::Normal;
    app.tab = Tab::Skills;
    app.reload();
    (app, home, data, guard)
}

/// Look up the `is_enabled_for` value for a named skill in `app.visible_items`.
/// Panics if the skill is not visible (the test plan expects both fixtures to
/// be present in every snapshot).
fn enabled_in_visible(app: &App, name: &str, target: CliTarget) -> bool {
    let r = app
        .visible_items()
        .into_iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("skill {name} missing from visible_items"));
    r.is_enabled_for(target)
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// §2.28 test 1: drive the digit keys 1→2→3→4→1 and check that
/// `active_target` flips and `visible_items()` reports the symlink state of
/// the new target on every step.
///
/// Skill set: `test-skill-a` enabled on Claude/Codex/Gemini, disabled on
/// OpenCode; `test-skill-b` disabled everywhere. So after switching, the
/// snapshot for {Claude, Codex, Gemini} all show A=enabled / B=disabled and
/// only OpenCode shows A=disabled.
#[test]
fn cli_target_switch_updates_symlink_visibility_across_all_targets() {
    let (mut app, _home, _data, _guard) = make_app(".runai");

    // Sanity: starts on Claude.
    assert_eq!(app.active_target, CliTarget::Claude);
    assert!(
        enabled_in_visible(&app, "test-skill-a", CliTarget::Claude),
        "test-skill-a should be enabled on the initial Claude target"
    );
    assert!(
        !enabled_in_visible(&app, "test-skill-b", CliTarget::Claude),
        "test-skill-b is never enabled — must read disabled on Claude"
    );

    // Press 2 → Codex. The skill set was enabled on Codex too, so A is
    // still on / B is still off.
    app.handle_key(key(KeyCode::Char('2')));
    assert_eq!(app.active_target, CliTarget::Codex);
    assert!(
        enabled_in_visible(&app, "test-skill-a", CliTarget::Codex),
        "after switching to Codex, A must read enabled on Codex"
    );
    assert!(
        !enabled_in_visible(&app, "test-skill-b", CliTarget::Codex),
        "B is unenabled on every target"
    );

    // Press 3 → Gemini.
    app.handle_key(key(KeyCode::Char('3')));
    assert_eq!(app.active_target, CliTarget::Gemini);
    assert!(
        enabled_in_visible(&app, "test-skill-a", CliTarget::Gemini),
        "after switching to Gemini, A must read enabled on Gemini"
    );
    assert!(!enabled_in_visible(&app, "test-skill-b", CliTarget::Gemini));

    // Press 4 → OpenCode. OpenCode has NO symlink for either fixture, so
    // both must read disabled.
    app.handle_key(key(KeyCode::Char('4')));
    assert_eq!(app.active_target, CliTarget::OpenCode);
    assert!(
        !enabled_in_visible(&app, "test-skill-a", CliTarget::OpenCode),
        "OpenCode never had a symlink — A must read disabled"
    );
    assert!(!enabled_in_visible(
        &app,
        "test-skill-b",
        CliTarget::OpenCode
    ));

    // Press 1 → back to Claude. Active target restored, A still enabled.
    app.handle_key(key(KeyCode::Char('1')));
    assert_eq!(app.active_target, CliTarget::Claude);
    assert!(
        enabled_in_visible(&app, "test-skill-a", CliTarget::Claude),
        "returning to Claude must still see A enabled there"
    );
    assert!(!enabled_in_visible(&app, "test-skill-b", CliTarget::Claude));

    // status() (the bottom-bar counts) must also follow the active target.
    // (es, ts, em, tm) — only the first two are interesting for skills.
    let (enabled_skills, total_skills, _enabled_mcps, _total_mcps) = app.status;
    assert_eq!(
        total_skills, 2,
        "two registered skills must appear in total"
    );
    assert_eq!(
        enabled_skills, 1,
        "on Claude after the round-trip, only test-skill-a is enabled"
    );
}

/// §2.28 test 2: the same 1→2→3→4→1 sequence must work under a non-default
/// `RUNE_DATA_DIR`. This is the safety contract double-run: any code that
/// drives the `<data>/skills/` pool or the 4 CLI symlink farms gets exercised
/// in both the default-home and the env-override layouts.
///
/// Driving `App` directly (no subprocess), the equivalent of `RUNE_DATA_DIR`
/// is constructing `SkillManager::with_base(<custom>)` — that's the same code
/// path `paths::data_dir()` ends up at when the env var is set. We assert on
/// the on-disk state under the custom data dir to confirm nothing leaked into
/// `~/.runai/skills/` (the default), which is the regression line for the
/// 2026-04-27 incident.
#[test]
fn cli_target_switch_respects_rune_data_dir() {
    // Scenario B: HOME=tmp2, data dir under a non-default subpath. We pick
    // a subpath that isn't `.runai` so a path comparison can prove nothing
    // landed in the default location.
    let (mut app, home, data, _guard) = make_app("custom-data-dir");

    // Sanity: the fixture lives under the custom data dir, NOT under the
    // default ~/.runai.
    assert!(
        data.ends_with("custom-data-dir"),
        "data dir should be the non-default subpath, got {}",
        data.display()
    );
    assert!(
        data.join("skills/test-skill-a").exists(),
        "test-skill-a's source dir must live in the custom data dir"
    );
    assert!(
        !home.path().join(".runai/skills/test-skill-a").exists(),
        "REGRESSION: a non-default data dir leaked a skill into the default ~/.runai/skills/"
    );

    // Same 1→2→3→4→1 sequence as scenario A.
    assert_eq!(app.active_target, CliTarget::Claude);
    assert!(enabled_in_visible(&app, "test-skill-a", CliTarget::Claude));

    app.handle_key(key(KeyCode::Char('2')));
    assert_eq!(app.active_target, CliTarget::Codex);
    assert!(enabled_in_visible(&app, "test-skill-a", CliTarget::Codex));
    assert!(!enabled_in_visible(&app, "test-skill-b", CliTarget::Codex));

    app.handle_key(key(KeyCode::Char('3')));
    assert_eq!(app.active_target, CliTarget::Gemini);
    assert!(enabled_in_visible(&app, "test-skill-a", CliTarget::Gemini));

    app.handle_key(key(KeyCode::Char('4')));
    assert_eq!(app.active_target, CliTarget::OpenCode);
    assert!(
        !enabled_in_visible(&app, "test-skill-a", CliTarget::OpenCode),
        "OpenCode has no symlink, A must read disabled even in the alt data dir"
    );

    app.handle_key(key(KeyCode::Char('1')));
    assert_eq!(app.active_target, CliTarget::Claude);
    assert!(enabled_in_visible(&app, "test-skill-a", CliTarget::Claude));

    // Cross-data-dir guard: the symlinks created by `enable_resource` point
    // at the per-data-dir managed `skills/test-skill-a` — the link target
    // canonicalizes back under `custom-data-dir`, not the default.
    let link = home.path().join(".claude/skills/test-skill-a");
    let resolved = std::fs::read_link(&link).expect("read claude symlink");
    let canonical = std::fs::canonicalize(&resolved).unwrap_or(resolved.clone());
    assert!(
        canonical.starts_with(data.canonicalize().unwrap_or(data.clone())),
        "claude symlink must resolve back to the custom data dir, got {}",
        canonical.display()
    );
    assert!(
        !canonical.starts_with(home.path().join(".runai")),
        "REGRESSION: claude symlink resolved into the default ~/.runai/, \
         meaning the custom data dir contract broke"
    );
}

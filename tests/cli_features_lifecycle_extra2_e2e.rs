//! P0 e2e regression tests for runai lifecycle subcommands.
//!
//! Each test spawns the real `runai` binary in an **isolated HOME** (tempdir)
//! with `RUNE_DATA_DIR` / `SKILL_MANAGER_DATA_DIR` either cleared (default-home
//! flow) or pointed at another tempdir (cross data-dir flow) and asserts on
//! the resulting filesystem + stdout state.
//!
//! Per `AGENTS.md` safety contract 5-rules:
//! - never touches real `~/.runai/` or real `~/.{claude,codex,gemini,opencode}/`
//! - high-risk features (backup/restore/trash empty) are double-run under
//!   default home AND an explicit `RUNE_DATA_DIR` override
//! - test names match the test plan's `red_test_name`
//!
//! Skipped on Windows: symlinks require Developer Mode / Admin and the
//! existing `manager::tests` module is gated the same way.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── shared helpers ─────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Build a TestEnv: tempdir HOME with the four CLI skills dirs pre-created
/// plus an isolated `~/.runai/` for managed data. RUNE_DATA_DIR /
/// SKILL_MANAGER_DATA_DIR are cleared by default so the binary uses
/// HOME-rooted defaults.
struct TestEnv {
    home: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
                .expect("pre-create CLI skills dir");
        }
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn default_skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn default_data_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }

    fn cli_skills_dir(&self, cli: &str) -> PathBuf {
        self.home().join(format!(".{cli}/skills"))
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }

    fn run_with_rune_data(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", rune_data)
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }
}

fn make_skill(parent: &Path, name: &str, body: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let skill_md = dir.join("SKILL.md");
    std::fs::write(
        &skill_md,
        format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
    )
    .unwrap();
    dir
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Count direct subdirectories under a path (non-recursive).
fn count_subdirs(dir: &Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    std::fs::read_dir(dir)
        .map(|it| {
            it.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

// ─── 1.12 trash empty ───────────────────────────────────────────────────────

/// Plan 1.12.1 [physical_e2e]: trash empty deletes payload files AND DB trash
/// records; trash list afterwards reports empty.

/// Plan 1.12.2 [cargo_integration]: trash empty on an already-empty trash is
/// idempotent — reports "Emptied trash (0 items)" without erroring.

/// Plan 1.12.3 [physical_e2e]: trash empty under an explicit `RUNE_DATA_DIR`
/// only clears that data dir's trash and leaves the default `~/.runai/trash/`
/// untouched. Cross data-dir isolation, the 4-20 / 4-27 root cause area.

// ─── 1.13 backup ────────────────────────────────────────────────────────────

/// Return the newest backup timestamp directory under `<data>/backups/`.
/// Caller asserts whatever count it expects via `count_subdirs`.
fn newest_backup_dir(data_dir: &Path) -> PathBuf {
    let backups = data_dir.join("backups");
    assert!(
        backups.exists(),
        "expected <data>/backups/ to exist at {}",
        backups.display()
    );
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&backups)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert!(
        !entries.is_empty(),
        "expected at least one backup dir under {}",
        backups.display()
    );
    // Lexicographic sort works because timestamps are YYYYMMDD_HHMMSS.
    entries.sort();
    entries.pop().unwrap()
}

/// Plan 1.13.1 [physical_e2e]: `runai backup` writes a snapshot under
/// `<data>/backups/<ts>/` containing managed skills, managed MCPs, per-CLI
/// skill dirs, and the CLI config files that exist on disk.

/// Plan 1.13.2 [cargo_integration]: two successive backups produce independent
/// snapshots — modifying a skill between backups doesn't retroactively rewrite
/// the earlier backup.

/// Plan 1.13.3 [physical_e2e]: backup under explicit RUNE_DATA_DIR lands in
/// that dir's `backups/` subdirectory, not the default `~/.runai/backups/`.

// ─── 1.15 restore ───────────────────────────────────────────────────────────

/// Plan 1.15.1 [physical_e2e]: `runai restore` (no `--timestamp`) recovers
/// managed skills + CLI config from the newest backup, replacing the live
/// state. Verifies the restore_backup overlay across managed dirs + configs.

/// Plan 1.15.2 [physical_e2e]: `runai restore --timestamp <ts>` honors the
/// explicit timestamp instead of the newest backup.

/// Plan 1.15.3 [cargo_integration]: when restore encounters a skill already
/// present in the live managed dir, the current implementation removes the
/// live dir before recopying — i.e. restore overwrites. This test pins the
/// observed behavior so future implementations that change semantics (e.g.
/// to refuse on conflict) trigger a deliberate, visible test failure.

/// Plan 1.15.4 [physical_e2e]: `runai restore` under `RUNE_DATA_DIR` reads
/// the backup from that data dir and restores into the same data dir's
/// managed skills/ — does not cross over to the default ~/.runai/.


#[test]
fn trash_empty_clears_all() {
    let env = TestEnv::new();

    // Install 5 skills via scan + adopt, then uninstall each to land in trash.
    let names = ["alpha", "beta", "gamma", "delta", "epsilon"];
    for n in &names {
        make_skill(&env.default_skills_dir(), n, &format!("{n} desc"));
    }
    let scan = env.run(&["scan"]);
    dump(&scan, "scan to register 5 skills");
    assert!(scan.status.success(), "scan should succeed");

    for n in &names {
        let un = env.run(&["uninstall", n]);
        dump(&un, &format!("uninstall {n}"));
        assert!(un.status.success(), "uninstall {n} should succeed");
    }

    // Trash dir should now contain 5 payload subdirs.
    let trash_root = env.default_data_dir().join("trash");
    assert!(
        trash_root.exists(),
        "trash dir should exist after uninstalls"
    );
    let pre_count = count_subdirs(&trash_root);
    assert_eq!(
        pre_count, 5,
        "expected 5 trash payload subdirs, found {pre_count} at {}",
        trash_root.display()
    );

    // Pre-check: trash list shows 5 entries.
    let list_before = env.run(&["trash", "list"]);
    dump(&list_before, "trash list before empty");
    assert!(list_before.status.success());
    let lb = String::from_utf8_lossy(&list_before.stdout);
    assert!(
        lb.contains("Total: 5 trashed resources"),
        "expected 5 trashed resources listed before empty. Got:\n{lb}"
    );

    // Run: trash empty.
    let empty = env.run(&["trash", "empty"]);
    dump(&empty, "trash empty");
    assert!(empty.status.success(), "trash empty should succeed");
    let eo = String::from_utf8_lossy(&empty.stdout);
    assert!(
        eo.contains("Emptied trash (5 items)"),
        "expected 'Emptied trash (5 items)' in output. Got:\n{eo}"
    );

    // Assert: trash payload subdirs all gone.
    let post_count = count_subdirs(&trash_root);
    assert_eq!(
        post_count, 0,
        "trash payload dirs should be empty after 'trash empty'; found {post_count} subdirs at {}",
        trash_root.display()
    );

    // Assert: trash list reports 'Trash is empty.'
    let list_after = env.run(&["trash", "list"]);
    dump(&list_after, "trash list after empty");
    assert!(list_after.status.success());
    let la = String::from_utf8_lossy(&list_after.stdout);
    assert!(
        la.contains("Trash is empty."),
        "expected 'Trash is empty.' after trash empty. Got:\n{la}"
    );
}


#[test]
fn trash_empty_handles_already_empty() {
    let env = TestEnv::new();

    // Nothing in trash. Run trash empty directly.
    let out = env.run(&["trash", "empty"]);
    dump(&out, "trash empty on empty trash");
    assert!(
        out.status.success(),
        "trash empty on empty trash should succeed"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Emptied trash (0 items)"),
        "expected 'Emptied trash (0 items)' on empty trash. Got:\n{stdout}"
    );
}


#[test]
fn trash_empty_respects_rune_data_dir() {
    let env = TestEnv::new();

    // Setup A: default data dir, install + uninstall 2 skills.
    for n in ["default-a", "default-b"] {
        make_skill(&env.default_skills_dir(), n, &format!("{n} desc"));
    }
    assert!(env.run(&["scan"]).status.success());
    for n in ["default-a", "default-b"] {
        assert!(env.run(&["uninstall", n]).status.success());
    }
    let default_trash = env.default_data_dir().join("trash");
    assert_eq!(
        count_subdirs(&default_trash),
        2,
        "expected 2 trash entries in default data dir"
    );

    // Setup B: alt data dir under tempdir, install + uninstall 3 skills.
    let alt = tempfile::tempdir().unwrap();
    let alt_data = alt.path().to_path_buf();
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();
    for n in ["alt-a", "alt-b", "alt-c"] {
        make_skill(&alt_data.join("skills"), n, &format!("{n} desc"));
    }
    assert!(
        env.run_with_rune_data(&alt_data, &["scan"])
            .status
            .success()
    );
    for n in ["alt-a", "alt-b", "alt-c"] {
        let un = env.run_with_rune_data(&alt_data, &["uninstall", n]);
        dump(&un, &format!("uninstall {n} (alt RUNE_DATA_DIR)"));
        assert!(
            un.status.success(),
            "uninstall {n} under alt RUNE_DATA_DIR should succeed"
        );
    }
    let alt_trash = alt_data.join("trash");
    assert_eq!(
        count_subdirs(&alt_trash),
        3,
        "expected 3 trash entries in alt RUNE_DATA_DIR"
    );

    // Empty only the alt data dir.
    let empty_alt = env.run_with_rune_data(&alt_data, &["trash", "empty"]);
    dump(&empty_alt, "trash empty (alt RUNE_DATA_DIR)");
    assert!(empty_alt.status.success());
    let eo = String::from_utf8_lossy(&empty_alt.stdout);
    assert!(
        eo.contains("Emptied trash (3 items)"),
        "alt trash empty should report 3 items. Got:\n{eo}"
    );

    // Assert: alt trash empty, default trash untouched.
    assert_eq!(
        count_subdirs(&alt_trash),
        0,
        "alt RUNE_DATA_DIR trash should be cleared"
    );
    assert_eq!(
        count_subdirs(&default_trash),
        2,
        "REGRESSION: default ~/.runai/trash was cleared by alt RUNE_DATA_DIR's 'trash empty'"
    );

    // Now empty default and assert alt stays at 0.
    let empty_default = env.run(&["trash", "empty"]);
    dump(&empty_default, "trash empty (default)");
    assert!(empty_default.status.success());
    let do_ = String::from_utf8_lossy(&empty_default.stdout);
    assert!(
        do_.contains("Emptied trash (2 items)"),
        "default trash empty should report 2 items. Got:\n{do_}"
    );
    assert_eq!(count_subdirs(&default_trash), 0);
    assert_eq!(count_subdirs(&alt_trash), 0);
}


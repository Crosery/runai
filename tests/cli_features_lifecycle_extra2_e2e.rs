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
        pre_count,
        5,
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
        post_count,
        0,
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

#[test]
fn backup_creates_snapshot() {
    let env = TestEnv::new();

    // Pre-populate managed skills.
    make_skill(&env.default_skills_dir(), "back-a", "back-a desc");
    make_skill(&env.default_skills_dir(), "back-b", "back-b desc");

    // Pre-populate managed MCP backup file.
    let mcps_dir = env.default_data_dir().join("mcps");
    std::fs::create_dir_all(&mcps_dir).unwrap();
    std::fs::write(
        mcps_dir.join("disabled-mcp.json"),
        r#"{"command":"my-mcp","args":[]}"#,
    )
    .unwrap();

    // Pre-populate ~/.claude.json (so backup picks it up).
    std::fs::write(
        env.home().join(".claude.json"),
        r#"{"mcpServers":{},"version":"test"}"#,
    )
    .unwrap();

    // Adopt + enable so a CLI skill symlink exists in ~/.claude/skills/.
    // NOTE: `runai scan` triggers a one-shot "first backup" if none exists yet
    // (see `Scanner::scan_all`). We sleep >=1s so the explicit `runai backup`
    // below gets a distinct YYYYMMDD_HHMMSS timestamp; then assert on the
    // newest snapshot under <data>/backups/, which is the one we just made.
    assert!(env.run(&["scan"]).status.success());
    let en = env.run(&["enable", "back-a", "--target", "claude"]);
    dump(&en, "enable back-a for claude");
    assert!(en.status.success());

    std::thread::sleep(std::time::Duration::from_millis(1100));

    let out = env.run(&["backup"]);
    dump(&out, "backup");
    assert!(out.status.success(), "runai backup should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Backup created:"),
        "expected 'Backup created:' in output. Got:\n{stdout}"
    );

    // Inspect the newest backup (the explicit `runai backup` we just ran).
    let backup_dir = newest_backup_dir(&env.default_data_dir());

    assert!(
        backup_dir.join("timestamp").exists(),
        "backup should write timestamp marker"
    );
    // Managed skills snapshot includes the two skill dirs.
    assert!(
        backup_dir.join("managed-skills/back-a/SKILL.md").exists(),
        "backup missing managed-skills/back-a/SKILL.md"
    );
    assert!(
        backup_dir.join("managed-skills/back-b/SKILL.md").exists(),
        "backup missing managed-skills/back-b/SKILL.md"
    );
    // Managed MCPs snapshot present.
    assert!(
        backup_dir.join("managed-mcps/disabled-mcp.json").exists(),
        "backup missing managed-mcps/disabled-mcp.json"
    );
    // claude.json copied.
    assert!(
        backup_dir.join("claude.json").exists(),
        "backup missing claude.json"
    );
    // CLI skill symlink farm snapshotted (claude has back-a enabled).
    assert!(
        backup_dir.join("claude-skills").exists(),
        "backup missing claude-skills/ snapshot dir"
    );
    assert!(
        std::fs::symlink_metadata(backup_dir.join("claude-skills/back-a")).is_ok(),
        "backup should preserve the enabled claude/back-a symlink"
    );
}

#[test]
fn backup_creates_independent_snapshots() {
    let env = TestEnv::new();

    make_skill(&env.default_skills_dir(), "snap-skill", "original body");
    // Scan triggers the implicit first backup; sleep before each subsequent
    // backup to guarantee distinct YYYYMMDD_HHMMSS timestamps.
    assert!(env.run(&["scan"]).status.success());

    std::thread::sleep(std::time::Duration::from_millis(1100));

    // First explicit backup (skill in original state).
    let first = env.run(&["backup"]);
    dump(&first, "first explicit backup");
    assert!(first.status.success());

    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Mutate the skill: rewrite SKILL.md body.
    std::fs::write(
        env.default_skills_dir().join("snap-skill/SKILL.md"),
        "---\nname: snap-skill\ndescription: MUTATED\n---\n\n# snap-skill\n\nMUTATED\n",
    )
    .unwrap();

    // Second explicit backup (after mutation).
    let second = env.run(&["backup"]);
    dump(&second, "second explicit backup");
    assert!(second.status.success());

    // Collect all backup dirs in lexicographic (== chronological) order.
    let backups_root = env.default_data_dir().join("backups");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&backups_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    entries.sort();
    // Scan auto-backup + 2 explicit backups == 3 snapshots total.
    assert_eq!(
        entries.len(),
        3,
        "expected 3 backup snapshots (1 auto + 2 explicit), found {}",
        entries.len()
    );

    // The two explicit backups are the second-and-third (newest two).
    let first_skill = entries[1].join("managed-skills/snap-skill/SKILL.md");
    let second_skill = entries[2].join("managed-skills/snap-skill/SKILL.md");
    assert!(first_skill.exists(), "first backup missing snap-skill");
    assert!(second_skill.exists(), "second backup missing snap-skill");

    let first_body = std::fs::read_to_string(&first_skill).unwrap();
    let second_body = std::fs::read_to_string(&second_skill).unwrap();
    assert!(
        first_body.contains("original body"),
        "first backup should preserve original body. Got:\n{first_body}"
    );
    assert!(
        second_body.contains("MUTATED"),
        "second backup should reflect MUTATED body. Got:\n{second_body}"
    );
    assert_ne!(
        first_body, second_body,
        "two backups should be independent snapshots"
    );
}

#[test]
fn backup_respects_rune_data_dir() {
    let env = TestEnv::new();

    // NOTE: `runai scan` creates an implicit first backup if none exists.
    // The flow below makes scan + explicit backup => 2 backups per data dir.

    // Default home: install one skill, scan (auto-backup), explicit backup.
    make_skill(&env.default_skills_dir(), "default-skill", "default desc");
    assert!(env.run(&["scan"]).status.success());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b_default = env.run(&["backup"]);
    dump(&b_default, "backup (default)");
    assert!(b_default.status.success());

    let default_backups = env.default_data_dir().join("backups");
    let default_count_before_alt = count_subdirs(&default_backups);
    assert!(
        default_count_before_alt >= 1,
        "default ~/.runai/backups/ should hold >=1 snapshot after scan+backup"
    );

    // Sleep so timestamps for the next backup differ.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // Alt data dir: install one skill there, scan (auto-backup), explicit
    // backup — all routed through RUNE_DATA_DIR.
    let alt = tempfile::tempdir().unwrap();
    let alt_data = alt.path().to_path_buf();
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();
    make_skill(&alt_data.join("skills"), "alt-skill", "alt desc");
    assert!(
        env.run_with_rune_data(&alt_data, &["scan"])
            .status
            .success()
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b_alt = env.run_with_rune_data(&alt_data, &["backup"]);
    dump(&b_alt, "backup (alt RUNE_DATA_DIR)");
    assert!(b_alt.status.success());

    let alt_backups = alt_data.join("backups");
    assert!(
        count_subdirs(&alt_backups) >= 1,
        "alt RUNE_DATA_DIR should hold >=1 backup snapshot at {}",
        alt_backups.display()
    );

    // Verify the NEWEST snapshots contain the right content (no leaks).
    let default_snap = newest_backup_dir(&env.default_data_dir());
    assert!(
        default_snap
            .join("managed-skills/default-skill/SKILL.md")
            .exists(),
        "default backup should contain default-skill"
    );
    assert!(
        !default_snap
            .join("managed-skills/alt-skill/SKILL.md")
            .exists(),
        "REGRESSION: default backup leaked alt-skill across RUNE_DATA_DIR"
    );

    let alt_snap = newest_backup_dir(&alt_data);
    assert!(
        alt_snap.join("managed-skills/alt-skill/SKILL.md").exists(),
        "alt backup should contain alt-skill"
    );
    assert!(
        !alt_snap
            .join("managed-skills/default-skill/SKILL.md")
            .exists(),
        "REGRESSION: alt backup leaked default-skill from default ~/.runai/"
    );

    // Default ~/.runai/backups/ count unchanged after alt RUNE_DATA_DIR ops.
    assert_eq!(
        count_subdirs(&default_backups),
        default_count_before_alt,
        "alt RUNE_DATA_DIR backup should not write into default ~/.runai/backups/"
    );
}

#[test]
fn restore_recovers_from_latest_backup() {
    let env = TestEnv::new();

    // Setup: managed skill + claude.json + enable so a CLI symlink exists.
    make_skill(&env.default_skills_dir(), "keep-me", "keep-me desc");
    std::fs::write(
        env.home().join(".claude.json"),
        r#"{"mcpServers":{},"phase":"before-backup"}"#,
    )
    .unwrap();

    assert!(env.run(&["scan"]).status.success());
    assert!(
        env.run(&["enable", "keep-me", "--target", "claude"])
            .status
            .success()
    );

    // Pre-backup snapshot of the data we expect to come back.
    let original_skill_body =
        std::fs::read_to_string(env.default_skills_dir().join("keep-me/SKILL.md")).unwrap();
    let original_claude_json = std::fs::read_to_string(env.home().join(".claude.json")).unwrap();

    // Sleep so the explicit backup gets a distinct timestamp from any
    // implicit scan-time backup.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = env.run(&["backup"]);
    dump(&b, "backup before destruction");
    assert!(b.status.success());

    // Destruction: nuke the managed skill dir + clobber the claude config.
    std::fs::remove_dir_all(env.default_skills_dir().join("keep-me")).unwrap();
    assert!(!env.default_skills_dir().join("keep-me").exists());
    std::fs::write(
        env.home().join(".claude.json"),
        r#"{"mcpServers":{},"phase":"DESTROYED"}"#,
    )
    .unwrap();

    // Restore from latest backup.
    let r = env.run(&["restore"]);
    dump(&r, "restore (latest)");
    assert!(r.status.success(), "runai restore should succeed");
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert!(
        stdout.contains("Restoring from backup:") && stdout.contains("Restored"),
        "expected 'Restoring from backup: <ts>' and 'Restored N items'. Got:\n{stdout}"
    );

    // Managed skill recovered with original body.
    let restored_skill = env.default_skills_dir().join("keep-me/SKILL.md");
    assert!(
        restored_skill.exists(),
        "restore should recreate managed skill at {}",
        restored_skill.display()
    );
    assert_eq!(
        std::fs::read_to_string(&restored_skill).unwrap(),
        original_skill_body,
        "restore should bring back the skill's original body"
    );

    // CLI config rolled back.
    assert_eq!(
        std::fs::read_to_string(env.home().join(".claude.json")).unwrap(),
        original_claude_json,
        "restore should overwrite ~/.claude.json with the backup copy"
    );
}

#[test]
fn restore_accepts_timestamp_parameter() {
    let env = TestEnv::new();

    // Seed an initial skill.
    let skill_md = env.default_skills_dir().join("ts-skill/SKILL.md");
    make_skill(&env.default_skills_dir(), "ts-skill", "version-one");
    assert!(env.run(&["scan"]).status.success());

    // First explicit backup captures version-one. Sleep to clear scan's
    // implicit-backup timestamp collision.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b1 = env.run(&["backup"]);
    dump(&b1, "backup 1 (version-one)");
    assert!(b1.status.success());

    // Pick this backup's timestamp from the newest snapshot dir.
    let ts1 = newest_backup_dir(&env.default_data_dir())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    // Sleep then mutate the skill to version-two and back up again.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(
        &skill_md,
        "---\nname: ts-skill\ndescription: version-two\n---\n\n# ts-skill\n\nversion-two body\n",
    )
    .unwrap();
    let b2 = env.run(&["backup"]);
    dump(&b2, "backup 2 (version-two)");
    assert!(b2.status.success());

    // Sanity: a second backup timestamp distinct from ts1.
    let ts2 = newest_backup_dir(&env.default_data_dir())
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_ne!(ts1, ts2, "two backups should have distinct timestamps");

    // Mutate to a third version, then restore to ts1.
    std::fs::write(
        &skill_md,
        "---\nname: ts-skill\ndescription: version-three\n---\n\n# ts-skill\n\nv3\n",
    )
    .unwrap();

    let r = env.run(&["restore", "--timestamp", &ts1]);
    dump(&r, "restore --timestamp ts1");
    assert!(r.status.success(), "restore --timestamp should succeed");
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert!(
        stdout.contains(&ts1),
        "restore output should echo the chosen timestamp {ts1}. Got:\n{stdout}"
    );

    // The skill should match version-one, not version-two or version-three.
    let body = std::fs::read_to_string(&skill_md).unwrap();
    assert!(
        body.contains("version-one"),
        "restore --timestamp ts1 should bring back version-one body. Got:\n{body}"
    );
    assert!(
        !body.contains("version-two") && !body.contains("version-three"),
        "restore --timestamp ts1 should NOT have version-two/three content. Got:\n{body}"
    );
}

#[test]
fn restore_handles_existing_skills() {
    let env = TestEnv::new();

    // Original skill, scan, backup.
    let skill_md = env.default_skills_dir().join("conflict/SKILL.md");
    make_skill(&env.default_skills_dir(), "conflict", "original-body");
    assert!(env.run(&["scan"]).status.success());

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let b = env.run(&["backup"]);
    dump(&b, "backup conflict (original-body)");
    assert!(b.status.success());

    // Mutate the live skill — same name, different content.
    std::fs::write(
        &skill_md,
        "---\nname: conflict\ndescription: live-mutated\n---\n\n# conflict\n\nlive-mutated\n",
    )
    .unwrap();
    assert!(
        std::fs::read_to_string(&skill_md)
            .unwrap()
            .contains("live-mutated")
    );

    // Restore: should NOT bail because skill exists — it overwrites.
    let r = env.run(&["restore"]);
    dump(&r, "restore over existing skill");
    assert!(
        r.status.success(),
        "runai restore should not abort when a same-name skill is live"
    );
    let stdout = String::from_utf8_lossy(&r.stdout);
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        !stderr.contains("Restore failed"),
        "restore should not print 'Restore failed'. Got stderr:\n{stderr}\nstdout:\n{stdout}"
    );

    // Live skill content should now match the backup (original-body).
    let body = std::fs::read_to_string(&skill_md).unwrap();
    assert!(
        body.contains("original-body"),
        "restore must overlay the backup body. Got:\n{body}"
    );
    assert!(
        !body.contains("live-mutated"),
        "live-mutated content should be replaced by backup. Got:\n{body}"
    );
}

#[test]
fn restore_respects_rune_data_dir() {
    let env = TestEnv::new();

    // ── Setup default data dir ──────────────────────────────
    let default_skill_md = env.default_skills_dir().join("default-restore/SKILL.md");
    make_skill(&env.default_skills_dir(), "default-restore", "default-body");
    assert!(env.run(&["scan"]).status.success());
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(env.run(&["backup"]).status.success());

    // ── Setup alt data dir ──────────────────────────────────
    let alt = tempfile::tempdir().unwrap();
    let alt_data = alt.path().to_path_buf();
    std::fs::create_dir_all(alt_data.join("skills")).unwrap();
    let alt_skill_md = alt_data.join("skills/alt-restore/SKILL.md");
    make_skill(&alt_data.join("skills"), "alt-restore", "alt-body");
    assert!(
        env.run_with_rune_data(&alt_data, &["scan"])
            .status
            .success()
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    assert!(
        env.run_with_rune_data(&alt_data, &["backup"])
            .status
            .success()
    );

    // ── Destruction: blow away both managed skill dirs ─────
    std::fs::remove_dir_all(env.default_skills_dir().join("default-restore")).unwrap();
    std::fs::remove_dir_all(alt_data.join("skills/alt-restore")).unwrap();
    assert!(!default_skill_md.exists());
    assert!(!alt_skill_md.exists());

    // ── Restore default only — alt must stay destroyed. ───
    let r_default = env.run(&["restore"]);
    dump(&r_default, "restore (default)");
    assert!(r_default.status.success());
    assert!(
        default_skill_md.exists(),
        "restore (default) should recreate default-restore at {}",
        default_skill_md.display()
    );
    assert!(
        !alt_skill_md.exists(),
        "REGRESSION: restoring default should NOT have touched alt RUNE_DATA_DIR's managed dir"
    );

    // ── Restore alt — default unaffected since we already restored it. ───
    let r_alt = env.run_with_rune_data(&alt_data, &["restore"]);
    dump(&r_alt, "restore (alt)");
    assert!(r_alt.status.success());
    assert!(
        alt_skill_md.exists(),
        "restore (alt) should recreate alt-restore at {}",
        alt_skill_md.display()
    );

    // Cross-contamination check: alt's backup should NOT have planted the
    // default-restore skill into alt's managed dir.
    assert!(
        !alt_data.join("skills/default-restore/SKILL.md").exists(),
        "REGRESSION: alt restore leaked default-restore into alt managed dir"
    );
    // And the default's backup should NOT have planted alt-restore into default.
    assert!(
        !env.default_skills_dir()
            .join("alt-restore/SKILL.md")
            .exists(),
        "REGRESSION: default restore leaked alt-restore into default managed dir"
    );

    // Verify body content matches the source data dir.
    assert!(
        std::fs::read_to_string(&default_skill_md)
            .unwrap()
            .contains("default-body"),
        "default-restore body mismatch after restore"
    );
    assert!(
        std::fs::read_to_string(&alt_skill_md)
            .unwrap()
            .contains("alt-body"),
        "alt-restore body mismatch after restore"
    );
}

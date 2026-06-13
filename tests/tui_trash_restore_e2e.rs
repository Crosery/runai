//! Physical end-to-end coverage for the **Trash Restore** flow exercised by
//! the TUI Trash tab (key `r`) and the `runai trash restore` CLI subcommand.
//!
//! Both entrypoints converge on `SkillManager::restore_from_trash` in
//! `src/core/manager.rs`. These tests spawn the real binary in an isolated
//! `HOME` tempdir and assert against the resulting filesystem and SQLite
//! state — the safety contract in `AGENTS.md` requires every destructive
//! restore path to round-trip through real `RUNE_DATA_DIR` resolution rather
//! than mocked `paths::data_dir()`.
//!
//! Skipped on Windows for the same reasons as `safety_e2e.rs`
//! (`dirs::home_dir()` ignores HOME env on win + symlinks need elevation).
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

struct TestEnv {
    home: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills"))).unwrap();
        }
        std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn default_skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn default_trash_dir(&self) -> PathBuf {
        self.home().join(".runai/trash")
    }

    fn default_db(&self) -> PathBuf {
        self.home().join(".runai/runai.db")
    }

    fn cli_skills_dir(&self, cli: &str) -> PathBuf {
        self.home().join(format!(".{cli}/skills"))
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env("RUNAI_NO_AUTOSPAWN", "1");
        cmd.output().expect("runai binary spawn")
    }

    fn run_with_rune_data(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNE_DATA_DIR", rune_data)
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env("RUNAI_NO_AUTOSPAWN", "1");
        cmd.output().expect("runai binary spawn")
    }
}

fn make_skill(parent: &Path, name: &str, body: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
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

/// Trash one previously-registered skill via `runai uninstall` and return its
/// resulting payload path (the `<ts>-<slug>` subdir under the trash dir).
fn trashed_payload_dir(trash_root: &Path, name: &str) -> Option<PathBuf> {
    let read = std::fs::read_dir(trash_root).ok()?;
    for entry in read.flatten() {
        let p = entry.path();
        let fname = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        // file name format is `<deleted_at_ms>-<name-slug>`
        if fname.ends_with(&format!("-{name}")) && p.is_dir() {
            return Some(p);
        }
    }
    None
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Restore round-trip with the default data dir:
///   register → uninstall (→ trash) → trash restore → assert filesystem + DB.
///
/// Matches plan §2.20 test #1 `trash_restore_moves_skill_back`.
#[test]
fn trash_restore_moves_skill_back() {
    let env = TestEnv::new();
    let name = "restore-roundtrip";

    // Pre-create the skill dir + SKILL.md, then register/adopt via `scan` so
    // the DB has an entry to uninstall.
    let original_dir = make_skill(&env.default_skills_dir(), name, "round-trip body");
    let original_md = original_dir.join("SKILL.md");
    let original_bytes = std::fs::read(&original_md).unwrap();

    let scan = env.run(&["scan"]);
    assert!(
        scan.status.success(),
        "{}",
        String::from_utf8_lossy(&scan.stderr)
    );

    // Uninstall moves the skill into the trash dir + DB trash table.
    let unin = env.run(&["uninstall", name]);
    dump(&unin, "uninstall");
    assert!(unin.status.success(), "uninstall failed");
    assert!(
        !original_dir.exists(),
        "trash should have moved skill dir out of .runai/skills/"
    );

    // Sanity-check trash now holds a payload dir for this skill.
    let payload = trashed_payload_dir(&env.default_trash_dir(), name)
        .expect("trash payload missing after uninstall");
    assert!(
        payload.join("SKILL.md").exists(),
        "trash payload missing SKILL.md"
    );
    assert!(
        env.default_db().exists(),
        "runai.db should exist after install/uninstall"
    );

    // Restore by resource name (CLI maps it to the matching trash entry id).
    let restore = env.run(&["trash", "restore", name]);
    dump(&restore, "trash restore");
    assert!(restore.status.success(), "trash restore failed");

    // Skill dir came back, SKILL.md content preserved byte-for-byte.
    assert!(
        original_dir.exists(),
        "restore should have moved payload back to .runai/skills/{name}/"
    );
    let after_md = original_dir.join("SKILL.md");
    assert!(after_md.exists(), "SKILL.md missing after restore");
    assert_eq!(
        std::fs::read(&after_md).unwrap(),
        original_bytes,
        "restored SKILL.md bytes drifted from original"
    );

    // Trash dir is now empty of this payload (move, not copy).
    assert!(
        trashed_payload_dir(&env.default_trash_dir(), name).is_none(),
        "trash payload still present after restore; restore must MOVE not COPY"
    );

    // DB now lists the skill again (and `trash` reports empty).
    let list = env.run(&["list"]);
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains(name),
        "`runai list` does not show restored skill; stdout was: {stdout}"
    );
    let trash_list = env.run(&["trash", "list"]);
    let trash_stdout = String::from_utf8_lossy(&trash_list.stdout);
    assert!(
        trash_stdout.contains("Trash is empty") || !trash_stdout.contains(name),
        "trash list still references restored skill: {trash_stdout}"
    );
}

/// Cross-`RUNE_DATA_DIR` regression: when the active data dir is a custom
/// location, every step (skill install → trash → restore) must stay under
/// that dir. The user's default `~/.runai/` must not be touched.
///
/// Matches plan §2.20 test #3 `trash_restore_uses_custom_data_dir`.
#[test]
fn trash_restore_uses_custom_data_dir() {
    let env = TestEnv::new();
    let custom = tempfile::tempdir().unwrap();
    let custom_skills = custom.path().join("skills");
    let custom_trash = custom.path().join("trash");
    let custom_db = custom.path().join("runai.db");
    std::fs::create_dir_all(&custom_skills).unwrap();

    let name = "custom-dir-skill";

    // Place skill under the custom data dir (NOT the default ~/.runai).
    let original_dir = make_skill(&custom_skills, name, "custom-dir body");
    let original_bytes = std::fs::read(original_dir.join("SKILL.md")).unwrap();

    // Sentinel skill in the DEFAULT location — must not be touched by any
    // step below.
    let sentinel_dir = make_skill(
        &env.default_skills_dir(),
        "sentinel-untouched",
        "do not move",
    );
    let sentinel_md = sentinel_dir.join("SKILL.md");
    let sentinel_bytes = std::fs::read(&sentinel_md).unwrap();

    // scan / uninstall / restore all run with RUNE_DATA_DIR=<custom>.
    let scan = env.run_with_rune_data(custom.path(), &["scan"]);
    assert!(
        scan.status.success(),
        "{}",
        String::from_utf8_lossy(&scan.stderr)
    );

    let unin = env.run_with_rune_data(custom.path(), &["uninstall", name]);
    dump(&unin, "uninstall (custom data dir)");
    assert!(unin.status.success(), "uninstall failed");
    assert!(
        !original_dir.exists(),
        "skill should have moved out of {}",
        original_dir.display()
    );

    // Payload must live under the custom trash dir, NOT the default trash dir.
    let payload = trashed_payload_dir(&custom_trash, name)
        .expect("custom-trash payload missing after uninstall");
    assert!(
        payload.starts_with(custom.path()),
        "trash payload {} escaped custom data dir {}",
        payload.display(),
        custom.path().display()
    );
    assert!(
        !env.default_trash_dir().exists()
            || trashed_payload_dir(&env.default_trash_dir(), name).is_none(),
        "uninstall leaked payload into the default ~/.runai/trash/"
    );

    // DB writes must land under the custom dir.
    assert!(custom_db.exists(), "runai.db missing under custom data dir");

    // Restore.
    let restore = env.run_with_rune_data(custom.path(), &["trash", "restore", name]);
    dump(&restore, "trash restore (custom data dir)");
    assert!(restore.status.success(), "trash restore failed");

    // Restored skill is in the CUSTOM dir.
    assert!(
        original_dir.exists(),
        "restore did not place skill back under custom data dir"
    );
    assert_eq!(
        std::fs::read(original_dir.join("SKILL.md")).unwrap(),
        original_bytes,
        "restored bytes drifted from original"
    );

    // No bleed into default ~/.runai/skills/ for this name.
    let default_misplace = env.default_skills_dir().join(name);
    assert!(
        !default_misplace.exists(),
        "restore wrote into default skills dir at {}",
        default_misplace.display()
    );

    // Sentinel in default location untouched (byte-for-byte, dir still there).
    assert!(
        sentinel_md.exists(),
        "REGRESSION: default-dir sentinel skill disappeared during custom-RUNE_DATA_DIR restore"
    );
    assert_eq!(
        std::fs::read(&sentinel_md).unwrap(),
        sentinel_bytes,
        "sentinel skill bytes drifted (default dir touched)"
    );

    // No default DB should have been written either.
    assert!(
        !env.default_db().exists(),
        "REGRESSION: default ~/.runai/runai.db was written during custom-RUNE_DATA_DIR restore"
    );
}

/// `enable_resource` re-runs after restore for each target the skill was
/// enabled on at trash time. This test verifies the most common case
/// (single target = Claude) and asserts the symlink in `~/.claude/skills/`
/// is recreated to point at the restored skill dir, while the other 3 CLI
/// targets stay untouched (no auto-enabling).
///
/// Matches plan §2.20 test #4 `trash_restore_recreates_symlinks`.
#[test]
fn trash_restore_recreates_symlinks() {
    let env = TestEnv::new();
    let name = "symlink-restore";
    let skill_dir = make_skill(&env.default_skills_dir(), name, "symlink restore body");

    // scan → enable on claude only.
    assert!(env.run(&["scan"]).status.success(), "scan failed");
    let en = env.run(&["enable", name, "--target", "claude"]);
    dump(&en, "enable claude");
    assert!(en.status.success(), "enable failed");
    let link = env.cli_skills_dir("claude").join(name);
    assert!(
        link.symlink_metadata().is_ok(),
        "claude symlink missing pre-trash"
    );

    // Sanity: the other 3 targets should NOT have a symlink.
    for cli in ["codex", "gemini", "opencode"] {
        let other = env.cli_skills_dir(cli).join(name);
        assert!(
            other.symlink_metadata().is_err(),
            "unexpected pre-trash symlink at {}",
            other.display()
        );
    }

    // Uninstall → trash.
    let unin = env.run(&["uninstall", name]);
    dump(&unin, "uninstall");
    assert!(unin.status.success(), "uninstall failed");
    // The claude symlink must be gone after trash.
    assert!(
        link.symlink_metadata().is_err(),
        "trash did not remove claude symlink"
    );
    // The skill dir was moved out.
    assert!(!skill_dir.exists(), "trash did not move skill dir");

    // Restore.
    let restore = env.run(&["trash", "restore", name]);
    dump(&restore, "trash restore");
    assert!(restore.status.success(), "trash restore failed");

    // Restored skill dir lives at the expected absolute path.
    let restored_dir = env.default_skills_dir().join(name);
    assert!(restored_dir.exists(), "restored skill dir missing");

    // Claude symlink re-created and points at the restored dir.
    let md = link
        .symlink_metadata()
        .expect("claude symlink missing post-restore");
    assert!(
        md.file_type().is_symlink(),
        "expected a symlink at {}, got {:?}",
        link.display(),
        md.file_type()
    );
    let target = std::fs::read_link(&link).expect("read_link failed");
    let canonical_target = std::fs::canonicalize(&target).unwrap_or(target.clone());
    let canonical_restored = std::fs::canonicalize(&restored_dir).unwrap_or(restored_dir.clone());
    assert_eq!(
        canonical_target,
        canonical_restored,
        "claude symlink points at {} but restored dir is {}",
        canonical_target.display(),
        canonical_restored.display()
    );

    // Other 3 targets were not auto-enabled by restore.
    for cli in ["codex", "gemini", "opencode"] {
        let other = env.cli_skills_dir(cli).join(name);
        assert!(
            other.symlink_metadata().is_err(),
            "REGRESSION: restore auto-enabled non-active target at {}",
            other.display()
        );
    }
}

/// Conflict guard: if a same-name skill exists at the restore destination,
/// `restore_from_trash` must refuse and leave both the trash entry and the
/// existing skill intact.
///
/// Matches plan §2.20 test #5 `trash_restore_fails_on_conflict`.
#[test]
fn trash_restore_fails_on_conflict() {
    let env = TestEnv::new();
    let name = "conflict-skill";

    // Phase 1: create + scan + uninstall an "A" version.
    let dir = make_skill(&env.default_skills_dir(), name, "version A");
    assert!(env.run(&["scan"]).status.success(), "scan A failed");
    let unin = env.run(&["uninstall", name]);
    dump(&unin, "uninstall A");
    assert!(unin.status.success(), "uninstall A failed");
    assert!(!dir.exists(), "A dir should be in trash");

    // Trash now holds the A payload.
    let payload_a = trashed_payload_dir(&env.default_trash_dir(), name)
        .expect("trash payload missing after uninstall A");
    let payload_a_bytes = std::fs::read(payload_a.join("SKILL.md")).unwrap();

    // Phase 2: create a fresh "B" skill at the same name (different body), then
    // adopt it so the DB row exists for the live version too.
    make_skill(&env.default_skills_dir(), name, "version B");
    assert!(env.run(&["scan"]).status.success(), "scan B failed");
    let b_dir = env.default_skills_dir().join(name);
    let b_bytes = std::fs::read(b_dir.join("SKILL.md")).unwrap();
    assert_ne!(
        payload_a_bytes, b_bytes,
        "test setup wrong: A and B should differ"
    );

    // Phase 3: restoring A must fail because B occupies the destination.
    let restore = env.run(&["trash", "restore", name]);
    dump(&restore, "trash restore (conflict)");
    assert!(
        !restore.status.success(),
        "restore should have failed but exited 0"
    );
    let stderr = String::from_utf8_lossy(&restore.stderr);
    let stdout = String::from_utf8_lossy(&restore.stdout);
    assert!(
        stderr.contains("already exists")
            || stdout.contains("already exists")
            || stderr.to_lowercase().contains("exists"),
        "expected an 'already exists' message; stderr={stderr} stdout={stdout}"
    );

    // B untouched.
    assert!(
        b_dir.join("SKILL.md").exists(),
        "B disappeared on failed restore"
    );
    assert_eq!(
        std::fs::read(b_dir.join("SKILL.md")).unwrap(),
        b_bytes,
        "B bytes mutated on failed restore"
    );

    // A still in trash (payload + entry preserved).
    let payload_a_after = trashed_payload_dir(&env.default_trash_dir(), name)
        .expect("A trash payload disappeared on failed restore");
    assert_eq!(
        std::fs::read(payload_a_after.join("SKILL.md")).unwrap(),
        payload_a_bytes,
        "A trash bytes mutated on failed restore"
    );
    let trash_list = env.run(&["trash", "list"]);
    let trash_stdout = String::from_utf8_lossy(&trash_list.stdout);
    assert!(
        trash_stdout.contains(name),
        "trash list lost A entry after failed restore: {trash_stdout}"
    );
}

/// Cross-CLI-target symmetry: for each of the 4 CLI targets, restore must
/// re-create the per-target symlink and only that target's symlink.
///
/// Matches plan §2.20 test #6 `trash_restore_works_across_all_cli_targets`.
/// One isolated TestEnv per target keeps the assertions independent.
#[test]
fn trash_restore_works_across_all_cli_targets() {
    for target in ["claude", "codex", "gemini", "opencode"] {
        let env = TestEnv::new();
        let name = format!("xtarget-{target}");

        let skill_dir = make_skill(&env.default_skills_dir(), &name, "xtarget body");
        assert!(
            env.run(&["scan"]).status.success(),
            "scan failed for {target}"
        );

        let en = env.run(&["enable", &name, "--target", target]);
        dump(&en, &format!("enable {target}"));
        assert!(en.status.success(), "enable failed for {target}");

        let link = env.cli_skills_dir(target).join(&name);
        let md = link
            .symlink_metadata()
            .unwrap_or_else(|_| panic!("symlink missing pre-trash for {target}"));
        assert!(
            md.file_type().is_symlink(),
            "expected symlink at {} pre-trash",
            link.display()
        );

        let unin = env.run(&["uninstall", &name]);
        dump(&unin, &format!("uninstall {target}"));
        assert!(unin.status.success(), "uninstall failed for {target}");
        assert!(
            link.symlink_metadata().is_err(),
            "{target} symlink not cleaned up by trash"
        );
        assert!(!skill_dir.exists(), "{target} skill dir not moved");

        let restore = env.run(&["trash", "restore", &name]);
        dump(&restore, &format!("trash restore {target}"));
        assert!(
            restore.status.success(),
            "trash restore failed for {target}"
        );

        // Symlink restored for THIS target.
        let md = link
            .symlink_metadata()
            .unwrap_or_else(|_| panic!("symlink missing post-restore for {target}"));
        assert!(
            md.file_type().is_symlink(),
            "expected symlink at {} post-restore",
            link.display()
        );
        // Points at the restored dir.
        let want = std::fs::canonicalize(env.default_skills_dir().join(&name))
            .unwrap_or_else(|_| env.default_skills_dir().join(&name));
        let got = std::fs::canonicalize(std::fs::read_link(&link).unwrap())
            .unwrap_or_else(|_| std::fs::read_link(&link).unwrap());
        assert_eq!(
            got,
            want,
            "{target} symlink points at {} but restored dir is {}",
            got.display(),
            want.display()
        );

        // OTHER targets were not auto-enabled by restore.
        for other in ["claude", "codex", "gemini", "opencode"] {
            if other == target {
                continue;
            }
            let other_link = env.cli_skills_dir(other).join(&name);
            assert!(
                other_link.symlink_metadata().is_err(),
                "REGRESSION: restoring {target} also created symlink at {} for {other}",
                other_link.display()
            );
        }
    }
}

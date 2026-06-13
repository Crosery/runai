//! P0 physical-e2e tests for the `scan` subcommand (the CLI surface of
//! `sm_scan` MCP tool — same `mgr.scan()` codepath).
//!
//! Per the safety contract in `AGENTS.md` §1.1 / §1.2, every test runs the
//! real `runai` binary inside an isolated `HOME=$(mktemp -d)` so it cannot
//! touch the real `~/.runai/` or `~/.{claude,codex,gemini,opencode}/`.
//! `RUNE_DATA_DIR` is cleared by default and only set explicitly for the
//! cross-data-dir guard regression test.
//!
//! Skipped on Windows: symlinks require Developer Mode / Admin and the
//! existing `manager::tests` module is already gated the same way.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Build an isolated test environment: tempdir HOME with the four CLI skills
/// directories pre-created and an empty managed `~/.runai/skills/`. By
/// default `RUNE_DATA_DIR` and `SKILL_MANAGER_DATA_DIR` are cleared so the
/// binary uses the HOME-rooted default.
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

    /// Variant: do NOT pre-create `~/.runai/`. Used by the "ensure data dir
    /// exists" test which validates the first-launch directory creation.
    fn new_without_runai_dir() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
                .expect("pre-create CLI skills dir");
        }
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn default_skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn cli_skills_dir(&self, cli: &str) -> PathBuf {
        self.home().join(format!(".{cli}/skills"))
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }

    fn run_with_rune_data(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
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

fn parse_scan_counts(stdout: &str) -> (usize, usize, usize) {
    // Output format: "Scan complete: N adopted, X skipped, Y errors"
    let line = stdout
        .lines()
        .find(|l| l.starts_with("Scan complete:"))
        .unwrap_or_else(|| panic!("no 'Scan complete:' line in stdout:\n{stdout}"));
    let nums: Vec<usize> = line
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse().ok())
        .collect();
    assert!(
        nums.len() >= 3,
        "expected 3 numbers in '{line}', got {nums:?}"
    );
    (nums[0], nums[1], nums[2])
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// sm_scan finds manually-planted skills in `~/.claude/skills/` and adopts
/// them into `~/.runai/skills/`. The user's source directory is moved (not
/// copied) into managed storage and the symlink at the CLI location is
/// re-pointed at the managed copy — this is the standard adoption path.
#[test]
fn scan_adopts_cli_skill_dirs() {
    let env = TestEnv::new();

    // Plant a skill as a real directory in ~/.claude/skills/ (the common case:
    // a user hand-drops a skill into Claude's skills dir and expects runai to
    // pick it up).
    let planted = make_skill(
        &env.cli_skills_dir("claude"),
        "manual-skill",
        "manual skill body",
    );
    assert!(planted.join("SKILL.md").exists());

    let out = env.run(&["scan"]);
    dump(&out, "scan adopts cli skill");
    assert!(out.status.success(), "scan should succeed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let (adopted, _skipped, errors) = parse_scan_counts(&stdout);
    assert!(
        adopted >= 1,
        "expected adopted>=1 for the planted skill, got {adopted}"
    );
    assert_eq!(errors, 0, "scan should not error on a valid skill");

    // After adoption the managed copy must exist and carry the SKILL.md body.
    let managed = env.default_skills_dir().join("manual-skill");
    assert!(
        managed.join("SKILL.md").exists(),
        "managed copy at {} missing SKILL.md after scan",
        managed.display()
    );

    // The original CLI-side path must still resolve to *something* (either a
    // symlink to the managed dir, or the moved directory) — i.e. Claude can
    // still find the skill.
    let cli_path = env.cli_skills_dir("claude").join("manual-skill");
    assert!(
        cli_path.exists() || std::fs::symlink_metadata(&cli_path).is_ok(),
        "CLI-side path {} disappeared after adoption",
        cli_path.display()
    );

    // The skill must appear in `runai list`.
    let list = env.run(&["list"]);
    dump(&list, "list after scan");
    assert!(list.status.success(), "list should succeed");
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("manual-skill"),
        "DB does not list manual-skill after adoption. list output:\n{list_out}"
    );
}

/// AGENTS.md safety contract §4 / 2026-04-20 + 2026-04-27 regression: when
/// `RUNE_DATA_DIR` points at a non-default location, `runai scan` must NOT
/// `std::fs::rename` a real skill out of the user's default
/// `~/.runai/skills/` into the override data dir. The scanner's
/// cross-data-dir guard (`scanner::adopt_entry` using
/// `default_data_dir_no_env()` as the baseline) is what prevents this from
/// happening.
#[test]
fn scan_refuses_rename_across_data_dirs() {
    let env = TestEnv::new();

    // Plant a sentinel skill in the user's DEFAULT data dir (~/.runai/skills/).
    let sentinel = make_skill(
        &env.default_skills_dir(),
        "default-skill",
        "default body",
    );
    let sentinel_md = sentinel.join("SKILL.md");
    let original = std::fs::read(&sentinel_md).unwrap();

    // First, register the sentinel in the default DB via a normal scan (no
    // RUNE_DATA_DIR override). This mirrors the realistic state where the
    // user already has a managed skill under their default ~/.runai/.
    let bootstrap = env.run(&["scan"]);
    dump(&bootstrap, "bootstrap scan (default data dir)");
    assert!(bootstrap.status.success());

    // Surface it through Claude's skills dir as a symlink so a follow-up scan
    // has a reason to try to adopt it via the CLI path (this is exactly the
    // 2026-04-27 dangerous shape: a CLI symlink pointing into the default
    // data dir, scanned while RUNE_DATA_DIR points elsewhere).
    #[cfg(unix)]
    std::os::unix::fs::symlink(&sentinel, env.cli_skills_dir("claude").join("default-skill"))
        .unwrap();

    // Now run scan with RUNE_DATA_DIR pointing somewhere else. Without the
    // cross-data-dir guard, the adopt path would `std::fs::rename` the
    // sentinel out of `~/.runai/skills/` into the override's `skills/` —
    // which is exactly the 2026-04-27 data-loss incident.
    let other = tempfile::tempdir().unwrap();
    let out = env.run_with_rune_data(other.path(), &["scan"]);
    dump(&out, "scan with foreign RUNE_DATA_DIR");

    // Sentinel must still live at its original default location.
    assert!(
        sentinel_md.exists(),
        "REGRESSION (4-20/4-27): scan moved default skill out of ~/.runai/skills/"
    );
    assert_eq!(
        std::fs::read(&sentinel_md).unwrap(),
        original,
        "sentinel content mutated"
    );

    // Nothing should have been moved into the foreign data dir.
    let foreign = other.path().join("skills/default-skill");
    assert!(
        !foreign.exists(),
        "REGRESSION: scan rename'd the sentinel into the override data dir at {}",
        foreign.display()
    );

    // The skill remains registered in the default DB (separate run reads
    // the default home's DB without RUNE_DATA_DIR).
    let list = env.run(&["list"]);
    dump(&list, "list (default data dir) after foreign scan");
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("default-skill"),
        "default-skill missing from default DB after cross-data-dir scan:\n{list_out}"
    );
}

/// Running `runai scan` twice in a row must be idempotent: the second pass
/// adopts 0 new skills, the DB shows exactly one row per name, and no errors
/// are reported. Users (and automation) re-run scan frequently — silently
/// duplicating rows or moving already-managed files would corrupt the DB.
#[test]
fn scan_idempotent_no_dupes() {
    let env = TestEnv::new();

    // One skill planted via ~/.claude/skills/ so adoption has work to do.
    make_skill(&env.cli_skills_dir("claude"), "dup-test", "dup body");

    // First scan — adopts 1.
    let first = env.run(&["scan"]);
    dump(&first, "scan #1");
    assert!(first.status.success());
    let (adopted1, _, errors1) = parse_scan_counts(&String::from_utf8_lossy(&first.stdout));
    assert_eq!(adopted1, 1, "first scan should adopt exactly 1 skill");
    assert_eq!(errors1, 0, "first scan should report 0 errors");

    // Second scan — adopts 0 new (the skill is already managed).
    let second = env.run(&["scan"]);
    dump(&second, "scan #2");
    assert!(second.status.success());
    let (adopted2, _, errors2) = parse_scan_counts(&String::from_utf8_lossy(&second.stdout));
    assert_eq!(
        adopted2, 0,
        "REGRESSION: second scan re-adopted an already-managed skill"
    );
    assert_eq!(errors2, 0, "second scan should report 0 errors");

    // DB should show exactly one entry for `dup-test`.
    let list = env.run(&["list"]);
    dump(&list, "list after two scans");
    let list_out = String::from_utf8_lossy(&list.stdout);
    let count = list_out.matches("[skill] dup-test —").count();
    assert_eq!(
        count, 1,
        "REGRESSION: dup-test appears in {count} DB rows (expected 1):\n{list_out}"
    );
}

/// sm_scan must work symmetrically across all 4 CLI target dirs in a single
/// invocation: claude, codex, gemini, opencode. A skill planted in each
/// gets adopted exactly once (no cross-target duplication), and each name
/// appears in `runai list`.
#[test]
fn scan_covers_all_cli_targets_symmetrically() {
    let env = TestEnv::new();

    let targets_and_names = [
        ("claude", "claude-only-skill"),
        ("codex", "codex-only-skill"),
        ("gemini", "gemini-only-skill"),
        ("opencode", "opencode-only-skill"),
    ];
    for (cli, name) in &targets_and_names {
        make_skill(&env.cli_skills_dir(cli), name, &format!("{name} body"));
    }

    let out = env.run(&["scan"]);
    dump(&out, "scan all 4 targets");
    assert!(out.status.success(), "scan should succeed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let (adopted, _, errors) = parse_scan_counts(&stdout);
    assert!(
        adopted >= 4,
        "expected adopted>=4 across 4 CLI dirs, got {adopted}"
    );
    assert_eq!(errors, 0, "scan should not error on valid per-target skills");

    let list = env.run(&["list"]);
    dump(&list, "list after multi-target scan");
    assert!(list.status.success());
    let list_out = String::from_utf8_lossy(&list.stdout);
    for (_, name) in &targets_and_names {
        // Match the bracketed kind+name prefix so the description (which also
        // contains the name) does not double-count.
        let row_marker = format!("[skill] {name} —");
        let count = list_out.matches(&row_marker).count();
        assert!(
            count >= 1,
            "skill {name} missing from list after scan:\n{list_out}"
        );
        // Each name must appear in exactly one DB row — no cross-target dupes.
        assert_eq!(
            count, 1,
            "REGRESSION: skill {name} duplicated {count}x in list (expected 1 row):\n{list_out}"
        );
    }
}

/// First-launch path: if `~/.runai/` does not exist yet, `runai scan` must
/// create the managed directory structure (and DB) on demand. We delete the
/// pre-created `~/.runai/` from the test env so the binary has to bootstrap
/// from a clean slate.
#[test]
fn scan_ensures_data_dir_exists() {
    let env = TestEnv::new_without_runai_dir();
    assert!(
        !env.home().join(".runai").exists(),
        "test precondition: ~/.runai/ should not exist before scan"
    );

    // Plant a skill so scan has something to adopt.
    make_skill(
        &env.cli_skills_dir("claude"),
        "bootstrap-skill",
        "bootstrap body",
    );

    let out = env.run(&["scan"]);
    dump(&out, "scan bootstraps ~/.runai");
    assert!(
        out.status.success(),
        "scan should succeed even when ~/.runai is missing"
    );

    // The managed skill tree must now exist.
    let runai = env.home().join(".runai");
    let skills = runai.join("skills");
    assert!(
        runai.exists(),
        "scan did not create ~/.runai/ on first launch"
    );
    assert!(
        skills.exists(),
        "scan did not create ~/.runai/skills/ on first launch"
    );

    // And the DB file must have been created.
    let db = runai.join("runai.db");
    assert!(
        db.exists(),
        "scan did not create ~/.runai/runai.db on first launch"
    );

    // Sanity: list should be runnable now that the DB exists.
    let list = env.run(&["list"]);
    dump(&list, "list after first-time scan");
    assert!(list.status.success(), "list should succeed after bootstrap");
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("bootstrap-skill"),
        "bootstrap-skill missing from list after first-time scan:\n{list_out}"
    );
}

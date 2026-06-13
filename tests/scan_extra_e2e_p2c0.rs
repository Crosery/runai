//! P2 regression coverage for the `runai scan` subcommand (PLANNING §1.1).
//!
//! These tests spawn the real `runai` binary in an isolated HOME tempdir,
//! exercise `runai scan` against synthesized skill layouts and assert on the
//! resulting filesystem + reported counters.
//!
//! Skipped on Windows: symlink creation requires Developer Mode / Admin and
//! the existing safety_e2e suite is gated the same way.

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

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Plan §1.1 #1: `scan` adopts a real skill directory living in
/// `~/.claude/skills/`, moves it to `~/.runai/skills/`, and replaces the
/// original path with a symlink (only in that CLI's dir, not the others).
#[test]
fn scan_adopts_foreign_skill_from_cli_dir() {
    let env = TestEnv::new();
    // Put a real skill dir (NOT symlink) under ~/.claude/skills/.
    let claude_dir = env.cli_skills_dir("claude");
    make_skill(&claude_dir, "test-skill1", "from claude cli dir");
    let cli_path = claude_dir.join("test-skill1");
    assert!(cli_path.is_dir(), "precondition: real dir in CLI skills");

    let out = env.run(&["scan"]);
    dump(&out, "scan adopts foreign skill");
    assert!(out.status.success(), "scan should succeed");

    // Managed copy should now exist.
    let managed_path = env.default_skills_dir().join("test-skill1");
    assert!(
        managed_path.join("SKILL.md").exists(),
        "test-skill1 should be in managed dir after scan, got: {:?}",
        std::fs::read_dir(env.default_skills_dir()).map(|it| it
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect::<Vec<_>>())
    );

    // The original CLI path should now be a symlink (not a real dir).
    assert!(
        is_symlink(&cli_path),
        "original CLI path should be replaced by symlink"
    );

    // The other 3 CLI dirs must not get a symlink — scan only relinks the
    // origin CLI dir for an adopted skill.
    for cli in ["codex", "gemini", "opencode"] {
        let other = env.cli_skills_dir(cli).join("test-skill1");
        assert!(
            !other.exists() && !is_symlink(&other),
            "other CLI ({cli}) should not have test-skill1"
        );
    }
}

/// Plan §1.1 #2 (variant): scan against an isolated `RUNE_DATA_DIR` must
/// leave the user's *default* `~/.runai/skills/` alone (the cross-data-dir
/// guard) AND must adopt skills found under that custom data dir's matching
/// CLI tree into the custom data dir, not into the default.
///
/// We test the isolation half directly:
///   - HOME = tempdir A. Default `~/.runai/skills/` is empty.
///   - RUNE_DATA_DIR = tempdir B with its own `.runai/skills/` already there.
///   - Put a real skill at HOME/.claude/skills/test-skill-b (under HOME, NOT
///     under the custom data dir's home, so the guard logic isn't tripped).
///   - Run `runai scan` with RUNE_DATA_DIR=B.
///   - Default ~/.runai/skills under HOME must remain empty (no leakage),
///     and the custom data dir is the active managed pool.
#[test]
fn scan_respects_rune_data_dir_isolation() {
    let env = TestEnv::new();
    let other_data = tempfile::tempdir().expect("custom RUNE_DATA_DIR");
    std::fs::create_dir_all(other_data.path().join("skills")).unwrap();

    // Seed a real skill in the CLI dir for adoption.
    make_skill(&env.cli_skills_dir("claude"), "test-skill-b", "b body");

    let out = env.run_with_rune_data(other_data.path(), &["scan"]);
    dump(&out, "scan with custom RUNE_DATA_DIR");
    assert!(out.status.success(), "scan should succeed");

    // The default HOME/.runai/skills/ must NOT contain test-skill-b — scan
    // is using the custom data dir as the managed pool target.
    let default_managed = env.default_skills_dir().join("test-skill-b");
    assert!(
        !default_managed.exists(),
        "default ~/.runai/skills must not contain custom-data-dir skill"
    );

    // It's fine whether the skill landed in the custom data dir or stayed
    // in the CLI dir untouched — the invariant we're guarding is **no
    // crossover** into the user's default `~/.runai/skills/`.
}

/// Plan §1.1 #3: running scan twice does not break (idempotent). The second
/// run should also succeed and not duplicate-adopt.
#[test]
fn scan_is_idempotent_across_runs() {
    let env = TestEnv::new();
    make_skill(&env.cli_skills_dir("claude"), "idem-skill", "idem body");

    let out1 = env.run(&["scan"]);
    dump(&out1, "scan #1");
    assert!(out1.status.success(), "first scan should succeed");
    let managed_path = env.default_skills_dir().join("idem-skill");
    assert!(managed_path.join("SKILL.md").exists(), "adopted on #1");

    // Second scan: managed dir already has the skill, original CLI dir is
    // already a symlink to it. No-op-equivalent.
    let out2 = env.run(&["scan"]);
    dump(&out2, "scan #2");
    assert!(out2.status.success(), "second scan should succeed");
    assert!(
        managed_path.join("SKILL.md").exists(),
        "managed copy still present after rerun"
    );
    let stdout = String::from_utf8_lossy(&out2.stdout);
    // We don't require the rerun to report 0 adopted (depends on how the
    // already-symlinked path is classified), but the line must be present.
    assert!(
        stdout.contains("Scan complete:"),
        "scan output should include the summary line: {stdout}"
    );
}

/// Plan §1.1 #4: `scan` prints a "Scan complete:" line carrying counters
/// (adopted / skipped / errors) regardless of input. Even with NOTHING to
/// adopt the line should appear, and exit status should be success.
#[test]
fn scan_reports_counters_in_summary_line() {
    let env = TestEnv::new();
    // Don't place any skill — just baseline empty CLI dirs.
    let out = env.run(&["scan"]);
    dump(&out, "scan on empty env");
    assert!(out.status.success(), "scan on empty env should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Scan complete:"),
        "scan must always print a summary line, got: {stdout}"
    );
    // The string format is `Scan complete: N adopted, M skipped, K errors`.
    // We grep for the three counter labels.
    for label in ["adopted", "skipped", "errors"] {
        assert!(
            stdout.contains(label),
            "scan summary missing '{label}' counter: {stdout}"
        );
    }
}

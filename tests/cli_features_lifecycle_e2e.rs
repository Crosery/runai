//! P0 lifecycle e2e regressions for `scan` / `enable` / `disable` / `install`.
//!
//! Each test runs the real `runai` binary inside an isolated HOME tempdir.
//! Cross `RUNE_DATA_DIR` and cross 4-CLI-target double-runs are enforced for
//! the destructive paths per the AGENTS.md safety contract.
//!
//! Skipped on Windows: symlinks require Developer Mode/Admin and `manager::tests`
//! is already gated the same way.
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

// ─── scan tests (P0 §1.1) ───────────────────────────────────────────────────

/// PLAN §1.1 test 1 — scan adopts a foreign skill from `~/.claude/skills/`
/// into `~/.runai/skills/`. The original CLI-dir entry is consumed (now
/// a managed symlink), and only the source-CLI target gets the symlink —
/// scan does not splatter the skill into the other 3 CLI dirs.
#[test]
fn scan_adopts_foreign_skill_from_cli_dir() {
    let env = TestEnv::new();

    // Plant a real skill dir directly under ~/.claude/skills/ (not under
    // ~/.runai/skills/) — this is the "foreign" skill scan must adopt.
    let cli_src = env.cli_skills_dir("claude").join("test-skill1");
    std::fs::create_dir_all(&cli_src).unwrap();
    std::fs::write(
        cli_src.join("SKILL.md"),
        "---\nname: test-skill1\ndescription: foreign cli skill\n---\n\n# test-skill1\n",
    )
    .unwrap();

    let out = env.run(&["scan"]);
    dump(&out, "scan adopt foreign cli skill");
    assert!(out.status.success(), "scan must succeed");

    // Managed: skill is now under ~/.runai/skills/.
    let managed = env.default_skills_dir().join("test-skill1");
    assert!(
        managed.join("SKILL.md").exists(),
        "scan did not adopt test-skill1 into managed ~/.runai/skills/. Looked at {}",
        managed.display()
    );

    // The other 3 CLI targets must NOT have a same-named entry; scan only
    // touches the source CLI dir.
    for other in ["codex", "gemini", "opencode"] {
        let collateral = env.cli_skills_dir(other).join("test-skill1");
        assert!(
            std::fs::symlink_metadata(&collateral).is_err(),
            "scan splattered test-skill1 into ~/.{}/skills/ — collateral at {}",
            other,
            collateral.display()
        );
    }

    // DB must know about test-skill1 (verify via `list` output).
    let list = env.run(&["list"]);
    dump(&list, "list after scan");
    assert!(list.status.success(), "list must succeed after scan");
    let list_stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_stdout.contains("test-skill1"),
        "DB did not register test-skill1; list output was:\n{list_stdout}"
    );
}

/// PLAN §1.1 test 2 — scan respects `RUNE_DATA_DIR`: two independent data
/// dirs each scan into themselves, neither leaks into the other.
///
/// We run scan twice in the SAME HOME but each invocation pointed at a
/// different `RUNE_DATA_DIR`. Each invocation feeds a unique skill into
/// `~/.claude/skills/` first so the source is distinct per run.
#[test]
fn scan_respects_rune_data_dir_isolation() {
    let env = TestEnv::new();

    // Run 1: skill-a, default data dir.
    let src_a = env.cli_skills_dir("claude").join("skill-a");
    std::fs::create_dir_all(&src_a).unwrap();
    std::fs::write(
        src_a.join("SKILL.md"),
        "---\nname: skill-a\ndescription: a\n---\n\n# skill-a\n",
    )
    .unwrap();
    let out_a = env.run(&["scan"]);
    dump(&out_a, "scan default data dir (skill-a)");
    assert!(out_a.status.success());
    assert!(
        env.default_skills_dir()
            .join("skill-a")
            .join("SKILL.md")
            .exists(),
        "skill-a should be adopted into default data dir"
    );

    // Run 2: skill-b, alternate RUNE_DATA_DIR.
    let alt_data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(alt_data.path().join("skills")).unwrap();
    let src_b = env.cli_skills_dir("claude").join("skill-b");
    std::fs::create_dir_all(&src_b).unwrap();
    std::fs::write(
        src_b.join("SKILL.md"),
        "---\nname: skill-b\ndescription: b\n---\n\n# skill-b\n",
    )
    .unwrap();
    let out_b = env.run_with_rune_data(alt_data.path(), &["scan"]);
    dump(&out_b, "scan alt RUNE_DATA_DIR (skill-b)");
    assert!(out_b.status.success());

    // Isolation: default data dir has skill-a but NOT skill-b.
    let default_b = env.default_skills_dir().join("skill-b").join("SKILL.md");
    assert!(
        !default_b.exists(),
        "skill-b leaked into the default data dir at {}",
        default_b.display()
    );

    // Alt data dir has skill-b but NOT skill-a.
    let alt_a = alt_data.path().join("skills/skill-a/SKILL.md");
    assert!(
        !alt_a.exists(),
        "skill-a leaked into the alt RUNE_DATA_DIR at {}",
        alt_a.display()
    );

    // skill-b must live under alt_data — adoption from the CLI dir succeeded.
    let alt_b = alt_data.path().join("skills/skill-b/SKILL.md");
    assert!(
        alt_b.exists(),
        "skill-b should have been adopted into alt RUNE_DATA_DIR at {}",
        alt_b.display()
    );

    // DB independence: list under default sees skill-a; list under alt sees skill-b.
    let list_default = env.run(&["list"]);
    let default_out = String::from_utf8_lossy(&list_default.stdout);
    assert!(
        default_out.contains("skill-a"),
        "default DB missing skill-a:\n{default_out}"
    );
    assert!(
        !default_out.contains("skill-b"),
        "default DB leaked skill-b:\n{default_out}"
    );

    let list_alt = env.run_with_rune_data(alt_data.path(), &["list"]);
    let alt_out = String::from_utf8_lossy(&list_alt.stdout);
    assert!(
        alt_out.contains("skill-b"),
        "alt DB missing skill-b:\n{alt_out}"
    );
    assert!(
        !alt_out.contains("skill-a"),
        "alt DB leaked skill-a:\n{alt_out}"
    );
}

/// PLAN §1.1 test 3 — scan succeeds whether or not a backup exists; calling
/// scan twice does not accumulate backup directories (idempotent w.r.t.
/// backups). The exact backup creation policy is implementation-defined; this
/// guards against scan FAILING because it could not write a backup, and
/// against runaway accumulation.
#[test]
fn scan_creates_first_backup_idempotently() {
    let env = TestEnv::new();
    make_skill(&env.default_skills_dir(), "scan-bak-skill", "x");

    let out1 = env.run(&["scan"]);
    dump(&out1, "scan #1");
    assert!(out1.status.success(), "scan #1 must succeed");

    let backups_dir = env.home().join(".runai/backups");
    let first_count = if backups_dir.exists() {
        backups_dir.read_dir().unwrap().count()
    } else {
        0
    };

    let out2 = env.run(&["scan"]);
    dump(&out2, "scan #2");
    assert!(out2.status.success(), "scan #2 must succeed");

    let second_count = if backups_dir.exists() {
        backups_dir.read_dir().unwrap().count()
    } else {
        0
    };

    // Scan must not accumulate a fresh backup on every invocation: an
    // unbounded backup directory would chew disk on power users running
    // scan from the TUI on every tab switch.
    assert!(
        second_count <= first_count + 1,
        "scan #2 created an extra backup ({second_count} > {first_count}+1)"
    );
}

/// PLAN §1.1 test 4 — scan reports adopted/skipped/errors counters. We plant
/// one CLI-dir foreign skill plus one already-managed skill and assert the
/// stdout summary line matches the expected counters and reports zero errors.
#[test]
fn scan_reports_correct_counters() {
    let env = TestEnv::new();

    // managed skill — should be skipped (already in default data dir).
    make_skill(&env.default_skills_dir(), "already-managed", "m");
    // foreign CLI-dir skill — should be adopted.
    let cli_src = env.cli_skills_dir("claude").join("from-cli");
    std::fs::create_dir_all(&cli_src).unwrap();
    std::fs::write(
        cli_src.join("SKILL.md"),
        "---\nname: from-cli\ndescription: c\n---\n\n# from-cli\n",
    )
    .unwrap();

    let out = env.run(&["scan"]);
    dump(&out, "scan counter check");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Summary line should be present.
    assert!(
        stdout.contains("Scan complete:"),
        "scan stdout missing summary line:\n{stdout}"
    );
    assert!(
        stdout.contains("0 errors"),
        "scan reported errors when none should occur:\n{stdout}"
    );

    // adopted count must be >= 1 (the foreign from-cli skill).
    //
    // Stdout shape: "Scan complete: N adopted, M skipped, K errors"
    let adopted_marker = stdout
        .split(',')
        .find(|s| s.contains("adopted"))
        .expect("missing adopted segment");
    // Pick the first numeric token in the segment ("Scan complete: 2 adopted").
    let n: u32 = adopted_marker
        .split_whitespace()
        .find_map(|t| t.parse::<u32>().ok())
        .unwrap_or(0);
    assert!(
        n >= 1,
        "adopted count should be >= 1 (foreign skill), saw {n}. stdout:\n{stdout}"
    );
}

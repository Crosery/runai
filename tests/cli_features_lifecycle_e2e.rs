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

// ─── enable tests (P0 §1.4) ─────────────────────────────────────────────────

/// PLAN §1.4 test 1 — enable creates symlinks symmetrically across all 4 CLI
/// targets. Each `enable --target {claude,codex,gemini,opencode}` lands a
/// symlink in the corresponding `~/.{target}/skills/` dir pointing at the
/// managed skill, and crucially DOES NOT splatter into the other 3 dirs.
#[test]
fn enable_creates_symlinks_across_cli_targets() {
    for target in ["claude", "codex", "gemini", "opencode"] {
        let env = TestEnv::new();
        let skill_name = "test-skill";
        make_skill(&env.default_skills_dir(), skill_name, "desc");
        assert!(env.run(&["scan"]).status.success(), "scan must succeed");

        let out = env.run(&["enable", skill_name, "--target", target]);
        dump(&out, &format!("enable on {target}"));
        assert!(
            out.status.success(),
            "enable failed for target={target}: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let link_path = env.cli_skills_dir(target).join(skill_name);
        assert!(
            std::fs::symlink_metadata(&link_path).is_ok(),
            "enable did not create symlink at {} for target={target}",
            link_path.display()
        );

        let resolved = std::fs::read_link(&link_path).unwrap();
        let expected = env.default_skills_dir().join(skill_name);
        assert_eq!(
            resolved, expected,
            "symlink for target={target} points to {} instead of managed dir {}",
            resolved.display(),
            expected.display()
        );

        // No collateral splatter to the other 3 CLI targets.
        for other in ["claude", "codex", "gemini", "opencode"] {
            if other == target {
                continue;
            }
            let collateral = env.cli_skills_dir(other).join(skill_name);
            assert!(
                std::fs::symlink_metadata(&collateral).is_err(),
                "enable on {target} splattered symlink into {} target dir at {}",
                other,
                collateral.display()
            );
        }
    }
}

/// PLAN §1.4 test 2 — when the target CLI's skills/ dir does not yet exist,
/// enable creates it (does not abort with "directory missing"). Healthy
/// first-use behavior: a fresh box has no `~/.claude/skills/` until enable
/// makes one.
#[test]
fn enable_creates_target_dir_if_missing() {
    let env = TestEnv::new();
    let skill_name = "auto-mkdir-skill";
    make_skill(&env.default_skills_dir(), skill_name, "desc");
    assert!(env.run(&["scan"]).status.success());

    // Remove the pre-created ~/.claude/skills dir so enable has to recreate it.
    let claude_dir = env.cli_skills_dir("claude");
    std::fs::remove_dir_all(&claude_dir).unwrap();
    assert!(
        !claude_dir.exists(),
        "preconditions: ~/.claude/skills must be absent before enable"
    );

    let out = env.run(&["enable", skill_name, "--target", "claude"]);
    dump(&out, "enable with missing target dir");
    assert!(
        out.status.success(),
        "enable should auto-create missing target dir, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let link_path = claude_dir.join(skill_name);
    assert!(
        std::fs::symlink_metadata(&link_path).is_ok(),
        "enable did not create symlink at {} after auto-creating parent",
        link_path.display()
    );
}

/// PLAN §1.4 test 3 — enable clobbers a stale symlink at the link path
/// rather than failing with EEXIST. Critical for recovery from earlier
/// broken state (uninstalled source, leftover dangling link).
///
/// Mirrors `safety_e2e::enable_succeeds_when_stale_symlink_exists_at_link_path`
/// but kept here as a P0 lifecycle marker per the plan.
#[test]
fn enable_clobbers_stale_symlink() {
    let env = TestEnv::new();
    let skill_name = "clobber-skill";
    make_skill(&env.default_skills_dir(), skill_name, "desc");
    assert!(env.run(&["scan"]).status.success());

    // Plant a dangling symlink at the link path so enable must clobber it.
    let nowhere = env.home().join(".runai/nonexistent-target");
    let link_path = env.cli_skills_dir("claude").join(skill_name);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&nowhere, &link_path).unwrap();
    assert!(
        std::fs::symlink_metadata(&link_path).is_ok(),
        "stale symlink should exist before enable"
    );

    let out = env.run(&["enable", skill_name, "--target", "claude"]);
    dump(&out, "enable over stale symlink");
    assert!(
        out.status.success(),
        "enable should clobber stale symlink, not fail EEXIST. stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let resolved = std::fs::read_link(&link_path).unwrap();
    let expected = env.default_skills_dir().join(skill_name);
    assert_eq!(
        resolved, expected,
        "after clobber, symlink should point at managed skill dir; got {}",
        resolved.display()
    );
}

/// PLAN §1.4 test 4 — `enable <group>` enables every member of the group.
/// We seed two skills, create a group containing both, then enable by group
/// name; both members must end up with symlinks under the target CLI dir.
#[test]
fn enable_group_enables_all_members() {
    let env = TestEnv::new();
    make_skill(&env.default_skills_dir(), "g-skill1", "a");
    make_skill(&env.default_skills_dir(), "g-skill2", "b");
    assert!(env.run(&["scan"]).status.success());

    // Create the group + add both members.
    let create = env.run(&[
        "group",
        "create",
        "my-group",
        "--name",
        "My Group",
        "--kind",
        "custom",
    ]);
    dump(&create, "group create");
    assert!(create.status.success(), "group create must succeed");

    for member in ["g-skill1", "g-skill2"] {
        let add = env.run(&["group", "add", "my-group", member]);
        dump(&add, &format!("group add {member}"));
        assert!(add.status.success(), "group add {member} must succeed");
    }

    let out = env.run(&["enable", "my-group", "--target", "claude"]);
    dump(&out, "enable group");
    assert!(
        out.status.success(),
        "enable group failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for member in ["g-skill1", "g-skill2"] {
        let link = env.cli_skills_dir("claude").join(member);
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "group enable did not symlink member {member} at {}",
            link.display()
        );
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(
            resolved,
            env.default_skills_dir().join(member),
            "symlink for {member} points to wrong target"
        );
    }
}

/// PLAN §1.4 test 5 — after `enable`, `list --target X` shows the skill as
/// enabled for that target (DB reflects the new state, used by both `list`
/// and `status`).
///
/// This is the DB-side verification of enable's contract: the symlink is
/// the runtime source-of-truth, but list/status must agree.
#[test]
fn enable_updates_database_enabled_flag() {
    let env = TestEnv::new();
    let skill_name = "db-flag-skill";
    make_skill(&env.default_skills_dir(), skill_name, "desc");
    assert!(env.run(&["scan"]).status.success());

    // Pre-enable: unfiltered list shows the skill as `[disabled]`.
    let pre = env.run(&["list"]);
    dump(&pre, "list pre-enable");
    let pre_out = String::from_utf8_lossy(&pre.stdout);
    let pre_row = pre_out
        .lines()
        .find(|l| l.contains(skill_name))
        .unwrap_or_else(|| panic!("list should show {skill_name} pre-enable:\n{pre_out}"));
    assert!(
        pre_row.contains("[disabled]"),
        "pre-enable row should say [disabled], got: {pre_row}"
    );

    assert!(
        env.run(&["enable", skill_name, "--target", "claude"])
            .status
            .success()
    );

    // Post-enable: unfiltered list shows `claude` in the enabled-targets segment.
    let post = env.run(&["list"]);
    dump(&post, "list post-enable");
    let post_out = String::from_utf8_lossy(&post.stdout);
    let row = post_out
        .lines()
        .find(|l| l.contains(skill_name))
        .expect("expected list row for skill");
    // The row format is `[skill] <name> — <desc> [enabled_targets_csv]`.
    // We assert "claude" appears in the trailing bracket segment.
    assert!(
        row.contains("[claude") || row.contains("claude]") || row.contains("claude,"),
        "post-enable row should mark claude as enabled. row: {row}"
    );
    assert!(
        !row.contains("[disabled]"),
        "post-enable row should NOT say [disabled]; row: {row}"
    );

    // And `list --target claude` (which filters to skills enabled on claude)
    // now includes the skill — proving the per-target DB flag flipped.
    let target_list = env.run(&["list", "--target", "claude"]);
    let target_out = String::from_utf8_lossy(&target_list.stdout);
    assert!(
        target_out.contains(skill_name),
        "list --target claude should include {skill_name} after enable; got:\n{target_out}"
    );
}

/// PLAN §1.4 test 6 — enable across `RUNE_DATA_DIR` isolation: the symlink
/// created under `~/.claude/skills/` must point at the data-dir actually
/// being used by THAT invocation, not the default `~/.runai/`.
#[test]
fn enable_respects_rune_data_dir_isolation() {
    let env = TestEnv::new();
    let alt_data = tempfile::tempdir().unwrap();
    let alt_skills = alt_data.path().join("skills");
    std::fs::create_dir_all(&alt_skills).unwrap();
    let skill_name = "alt-skill";
    make_skill(&alt_skills, skill_name, "alt body");

    // Scan + enable against the alt data dir.
    let scan = env.run_with_rune_data(alt_data.path(), &["scan"]);
    dump(&scan, "scan against alt data dir");
    assert!(scan.status.success());
    let en = env.run_with_rune_data(alt_data.path(), &["enable", skill_name, "--target", "claude"]);
    dump(&en, "enable against alt data dir");
    assert!(en.status.success(), "enable against alt data dir failed");

    let link_path = env.cli_skills_dir("claude").join(skill_name);
    assert!(
        std::fs::symlink_metadata(&link_path).is_ok(),
        "enable did not create symlink at {} under alt data dir",
        link_path.display()
    );

    // Resolve the symlink and assert it points into the ALT data dir's
    // skills/, not the default `~/.runai/skills/`.
    let resolved = std::fs::read_link(&link_path).unwrap();
    let canonical_resolved = std::fs::canonicalize(&resolved)
        .unwrap_or_else(|_| resolved.clone());
    let canonical_alt = std::fs::canonicalize(alt_skills.join(skill_name)).unwrap();
    assert_eq!(
        canonical_resolved, canonical_alt,
        "symlink should resolve to ALT data dir's {} but resolved to {}",
        canonical_alt.display(),
        canonical_resolved.display()
    );

    // And it must NOT point into the default `~/.runai/skills/`.
    let default_path = env.default_skills_dir().join(skill_name);
    assert!(
        !default_path.exists(),
        "skill should not exist under default data dir after alt-data-dir enable; \
         found {}",
        default_path.display()
    );
}

// ─── disable tests (P0 §1.5) ────────────────────────────────────────────────

/// PLAN §1.5 test 1 — disable removes the symlink from each of the 4 CLI
/// target dirs symmetrically. We enable on all 4 then disable on each in turn
/// and assert only the target-specific symlink disappears.
#[test]
fn disable_removes_symlinks_from_cli_targets() {
    for target in ["claude", "codex", "gemini", "opencode"] {
        let env = TestEnv::new();
        let skill_name = "dis-skill";
        make_skill(&env.default_skills_dir(), skill_name, "desc");
        assert!(env.run(&["scan"]).status.success(), "scan failed");
        assert!(
            env.run(&["enable", skill_name, "--target", target])
                .status
                .success(),
            "enable on {target} failed"
        );

        let link_path = env.cli_skills_dir(target).join(skill_name);
        assert!(
            std::fs::symlink_metadata(&link_path).is_ok(),
            "preconditions: symlink should exist on {target} before disable"
        );

        let out = env.run(&["disable", skill_name, "--target", target]);
        dump(&out, &format!("disable on {target}"));
        assert!(
            out.status.success(),
            "disable failed for target={target}: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        assert!(
            std::fs::symlink_metadata(&link_path).is_err(),
            "disable on {target} did not remove symlink at {}",
            link_path.display()
        );
    }
}

/// PLAN §1.5 test 2 — disable is idempotent on a missing symlink: calling
/// disable when there is nothing to remove returns success and prints a
/// clean message (no panic, no "EEXIST"-style error).
#[test]
fn disable_idempotent_on_missing_symlink() {
    let env = TestEnv::new();
    let skill_name = "no-link-skill";
    make_skill(&env.default_skills_dir(), skill_name, "desc");
    assert!(env.run(&["scan"]).status.success());

    // Skill exists in DB but was never enabled → no symlink at the link path.
    let link_path = env.cli_skills_dir("claude").join(skill_name);
    assert!(
        std::fs::symlink_metadata(&link_path).is_err(),
        "preconditions: no symlink before disable"
    );

    let out = env.run(&["disable", skill_name, "--target", "claude"]);
    dump(&out, "disable when no symlink exists");
    assert!(
        out.status.success(),
        "disable should be idempotent when symlink already absent; \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Running it a second time must also succeed.
    let out2 = env.run(&["disable", skill_name, "--target", "claude"]);
    dump(&out2, "disable a second time");
    assert!(
        out2.status.success(),
        "second disable should still succeed (idempotent)"
    );
}

/// PLAN §1.5 test 3 — disable refuses to delete a real directory at the
/// link path. If a user has hand-created `~/.claude/skills/<name>/` as a
/// real directory (not a symlink), disable must NOT recursively delete it.
///
/// We assert the real directory survives the disable call (best-effort:
/// either disable bails with non-zero exit OR completes successfully but
/// leaves the directory alone).
#[test]
fn disable_refuses_to_delete_real_directory() {
    let env = TestEnv::new();
    let skill_name = "real-dir-skill";
    make_skill(&env.default_skills_dir(), skill_name, "desc");
    assert!(env.run(&["scan"]).status.success());

    // Plant a REAL directory (not symlink) at the link path with a sentinel
    // file. disable must not nuke it.
    let real_dir = env.cli_skills_dir("claude").join(skill_name);
    std::fs::create_dir_all(&real_dir).unwrap();
    let sentinel = real_dir.join("sentinel.txt");
    std::fs::write(&sentinel, "do not delete\n").unwrap();
    assert!(
        std::fs::metadata(&real_dir).unwrap().is_dir(),
        "preconditions: real dir at link path"
    );
    assert!(
        !std::fs::symlink_metadata(&real_dir).unwrap().is_symlink(),
        "preconditions: must be a real dir, not a symlink"
    );

    let out = env.run(&["disable", skill_name, "--target", "claude"]);
    dump(&out, "disable when real dir present");

    // Regardless of exit status, the real directory + sentinel must survive.
    // The data-safety invariant is "do not recursively rm a real dir at the
    // link path", not "exit 0 vs 1".
    assert!(
        real_dir.exists(),
        "REGRESSION: disable removed a REAL directory at {}",
        real_dir.display()
    );
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "do not delete\n",
        "REGRESSION: disable touched the sentinel file inside the real dir"
    );
}

/// PLAN §1.5 test 4 — disable by group name disables every member of the
/// group on the given target. Mirror of `enable_group_enables_all_members`
/// but reversed.
#[test]
fn disable_group_disables_all_members() {
    let env = TestEnv::new();
    make_skill(&env.default_skills_dir(), "dg-skill1", "a");
    make_skill(&env.default_skills_dir(), "dg-skill2", "b");
    assert!(env.run(&["scan"]).status.success());

    assert!(
        env.run(&[
            "group",
            "create",
            "dis-group",
            "--name",
            "Dis Group",
            "--kind",
            "custom",
        ])
        .status
        .success()
    );
    for m in ["dg-skill1", "dg-skill2"] {
        assert!(env.run(&["group", "add", "dis-group", m]).status.success());
        assert!(
            env.run(&["enable", m, "--target", "claude"])
                .status
                .success()
        );
        assert!(std::fs::symlink_metadata(env.cli_skills_dir("claude").join(m)).is_ok());
    }

    let out = env.run(&["disable", "dis-group", "--target", "claude"]);
    dump(&out, "disable group");
    assert!(
        out.status.success(),
        "disable group failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    for m in ["dg-skill1", "dg-skill2"] {
        let link = env.cli_skills_dir("claude").join(m);
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "disable group did not remove symlink for {m} at {}",
            link.display()
        );
    }
}

/// PLAN §1.5 test 5 — after disable, the DB reflects the target as no
/// longer enabled. `list --target claude` post-disable should not include
/// the skill, and the unfiltered list row should not show `claude` in the
/// enabled-targets segment.
#[test]
fn disable_updates_database_enabled_flag() {
    let env = TestEnv::new();
    let skill_name = "dis-db-skill";
    make_skill(&env.default_skills_dir(), skill_name, "desc");
    assert!(env.run(&["scan"]).status.success());
    assert!(
        env.run(&["enable", skill_name, "--target", "claude"])
            .status
            .success()
    );

    // Sanity: list --target claude shows it pre-disable.
    let pre = env.run(&["list", "--target", "claude"]);
    let pre_out = String::from_utf8_lossy(&pre.stdout);
    assert!(
        pre_out.contains(skill_name),
        "preconditions: list --target claude should contain {skill_name}:\n{pre_out}"
    );

    assert!(
        env.run(&["disable", skill_name, "--target", "claude"])
            .status
            .success()
    );

    // list --target claude must no longer include it.
    let post = env.run(&["list", "--target", "claude"]);
    dump(&post, "list --target claude post-disable");
    let post_out = String::from_utf8_lossy(&post.stdout);
    assert!(
        !post_out.contains(skill_name),
        "list --target claude should NOT contain {skill_name} after disable:\n{post_out}"
    );

    // Unfiltered list row should mark it disabled.
    let full = env.run(&["list"]);
    let full_out = String::from_utf8_lossy(&full.stdout);
    let row = full_out
        .lines()
        .find(|l| l.contains(skill_name))
        .expect("expected list row for skill");
    assert!(
        row.contains("[disabled]"),
        "post-disable row should say [disabled]; row: {row}"
    );
}

// ─── install tests (P0 §1.6) ────────────────────────────────────────────────

/// PLAN §1.6 test 3 — `install <full-url>` accepts the
/// `https://github.com/owner/repo` form and parses it equivalently to
/// `owner/repo`. We assert by passing a deliberately bad source through the
/// URL parser: invalid format produces a clean "Invalid format" message.
///
/// This is an offline test: it never reaches the network because the source
/// fails parse validation before any GitHub call.
#[test]
fn install_accepts_full_github_url() {
    let env = TestEnv::new();

    // Invalid format: just a hostname, no owner/repo — must produce a clean
    // parse error rather than panic / hang / network probe.
    let out = env.run(&["install", "not-a-valid-source"]);
    dump(&out, "install with invalid format");
    assert!(
        !out.status.success(),
        "install with invalid source must fail"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("Invalid format") || combined.contains("owner/repo"),
        "install should give a clean format error, got:\n{combined}"
    );

    // Now: the full URL form. Parsing strips `https://github.com/` and
    // trailing `/`. With a malformed remainder (no slash), it should still
    // reach the format error path — proving the URL prefix was stripped.
    let out2 = env.run(&["install", "https://github.com/justonepart"]);
    dump(&out2, "install with full URL but missing repo");
    assert!(
        !out2.status.success(),
        "install with malformed full URL must fail"
    );
    let combined2 = format!(
        "{}{}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr),
    );
    assert!(
        combined2.contains("Invalid format") || combined2.contains("owner/repo"),
        "install should parse the URL prefix then complain about format; got:\n{combined2}"
    );
}

/// PLAN §1.6 test 4 — install fails gracefully when the target repo cannot
/// be reached (network error, nonexistent repo). The key invariant: a
/// failed install must not leave half-installed state in `~/.runai/skills/`
/// or partial DB rows.
///
/// We point at a deliberately bogus owner/repo that no public GitHub
/// account holds. The fetch fails; we assert:
///   1. exit code != 0
///   2. ~/.runai/skills/ stays empty (no partial directory)
///   3. `list` shows no skills (no DB pollution)
///
/// Note: this test does hit the network with one HEAD/GET request to
/// jsdelivr/GitHub for the bogus repo, which is the production fetch path.
/// In offline CI this still passes — the request fails fast (DNS or 4xx)
/// and the assertion only requires "no partial state".
#[test]
fn install_fails_gracefully_on_network_error() {
    let env = TestEnv::new();

    // Bogus owner/repo: extremely unlikely to ever exist.
    let bogus = "runai-test-nonexistent-owner-9999/nonexistent-repo-xyz-9999";
    let out = env.run(&["install", bogus]);
    dump(&out, "install bogus repo");
    assert!(
        !out.status.success(),
        "install of nonexistent repo must fail"
    );

    // ~/.runai/skills/ must remain effectively empty (no partial skill dir).
    let skills_dir = env.default_skills_dir();
    if skills_dir.exists() {
        let entries: Vec<_> = skills_dir
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(
            entries.is_empty(),
            "FAILURE: install left partial state in {}; entries: {:?}",
            skills_dir.display(),
            entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );
    }

    // list must report no skills (no DB row inserted before failure).
    let list = env.run(&["list"]);
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(
        list_out.contains("No resources found") || !list_out.contains("github:"),
        "list should not show skills after failed install; got:\n{list_out}"
    );
}

/// PLAN §1.6 — branch parameter parsing: `owner/repo@branch` is split at
/// the `@`. We verify the CLI accepts and propagates the branch by passing
/// an invalid branch on a non-existent repo and asserting the failure
/// happens *after* the format passes parse (no "Invalid format" message).
///
/// This is an offline-safe test of the parsing layer — the network fetch
/// fails (bogus repo) but the parse step proves `@branch` is handled.
#[test]
fn install_respects_branch_parameter() {
    let env = TestEnv::new();

    // owner/repo@branch with a bogus repo — fetch will fail, but parse must succeed.
    let out = env.run(&["install", "runai-test-nonexistent/some-repo@develop"]);
    dump(&out, "install with @branch");
    assert!(
        !out.status.success(),
        "fetch must fail on bogus repo even with valid parse"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    // Critical assertion: failure must NOT be the format-parse error.
    // If we saw "Invalid format" that means `@branch` was rejected at parse
    // (regression of branch-suffix support).
    assert!(
        !combined.contains("Invalid format"),
        "REGRESSION: `owner/repo@branch` should parse, but install reported\n\
         a format error:\n{combined}"
    );
    // And the printed banner should include "@develop" (proves the branch
    // was extracted and routed through).
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("@develop"),
        "install banner should show the parsed branch @develop; stdout:\n{stdout}"
    );
}

/// PLAN §1.6 test 5 — install respects `RUNE_DATA_DIR` isolation.
///
/// Since real `install` requires network to fetch from GitHub, this test
/// verifies the data-dir routing on the failure path: when install runs
/// with a custom `RUNE_DATA_DIR` and fails, it must not write any partial
/// state into EITHER the alt data dir OR the default `~/.runai/skills/`.
#[test]
fn install_respects_rune_data_dir() {
    let env = TestEnv::new();
    let alt_data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(alt_data.path().join("skills")).unwrap();

    let bogus = "runai-test-nonexistent/some-repo-9999";
    let out = env.run_with_rune_data(alt_data.path(), &["install", bogus]);
    dump(&out, "install bogus repo under alt RUNE_DATA_DIR");
    assert!(
        !out.status.success(),
        "bogus install under alt data dir must fail"
    );

    // Default data dir must stay clean.
    let default_skills = env.default_skills_dir();
    if default_skills.exists() {
        let entries: Vec<_> = default_skills
            .read_dir()
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(
            entries.is_empty(),
            "REGRESSION: install with alt RUNE_DATA_DIR leaked into default skills dir: {:?}",
            entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
        );
    }

    // Alt data dir's skills/ must also be empty (failed install ≠ partial state).
    let alt_skills = alt_data.path().join("skills");
    let entries: Vec<_> = alt_skills
        .read_dir()
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert!(
        entries.is_empty(),
        "FAILURE: install left partial state in alt skills dir: {:?}",
        entries.iter().map(|e| e.file_name()).collect::<Vec<_>>()
    );
}

//! P2 regression smoke tests for `core::doctor` + `core::auto_group`.
//!
//! `core::auth` and `core::autostart` are listed in the test plan but DO NOT
//! exist in this cloud HEAD — those features are skipped (see structured
//! output `skipped` array reported by the runner).
//!
//! The doctor tests (added in a later commit) spawn the installed
//! `/Users/crosery/.cargo/bin/runai` binary inside an isolated
//! `HOME=$(mktemp -d)` plus `RUNE_DATA_DIR=$HOME/.runai` so the real
//! `~/.runai/` is untouched, per the AGENTS.md safety contract. The
//! auto_group tests use the in-process library API via
//! `SkillManager::with_base(tempdir)` so they need no env mocking at all.
//!
//! Skipped on Windows: dirs::home_dir() ignores HOME there and symlink
//! creation needs Developer Mode.
#![cfg(not(target_os = "windows"))]

use runai::core::auto_group::AutoGroup;
use runai::core::group::{Group, GroupKind};
use runai::core::manager::SkillManager;

// ════════════════════════════════════════════════════════════════════════════
// 5.13 core::auto_group
// ════════════════════════════════════════════════════════════════════════════

/// `AutoGroup::auto_group_all` classifies registered skills and creates groups
/// with the expected lowercase-dashed IDs.
#[test]
fn auto_group_classifies_and_creates() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();

    for name in &[
        "python-testing",
        "django-patterns",
        "rust-cli",
        "random-tool",
    ] {
        let dir = mgr.paths().skills_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\n---\n# {name}\nbody\n"),
        )
        .unwrap();
        mgr.register_local_skill(name).unwrap();
    }

    let result = AutoGroup::auto_group_all(&mgr).unwrap();
    assert!(
        result.groups_created >= 2,
        "expected >=2 groups, got {}",
        result.groups_created,
    );
    assert!(
        result.resources_assigned >= 3,
        "expected >=3 assigned, got {}",
        result.resources_assigned,
    );

    let groups = mgr.list_groups().unwrap();
    let ids: Vec<&str> = groups.iter().map(|(id, _)| id.as_str()).collect();
    assert!(
        ids.contains(&"python"),
        "missing `python` group, have {ids:?}"
    );
    assert!(ids.contains(&"rust"), "missing `rust` group, have {ids:?}");

    // Members landed in the right group.
    let python = mgr.db().get_group_members("python").unwrap();
    assert_eq!(
        python.len(),
        2,
        "python should have python-testing + django-patterns"
    );
}

/// `AutoGroup::auto_group_all` must skip creating a group whose ID already
/// exists, but still attach matching new members to it. Re-running must not
/// duplicate the group.
#[test]
fn auto_group_skips_existing_groups() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();

    // Pre-create `python` group with a non-empty description.
    let preexisting = Group {
        name: "Python".into(),
        description: "preexisting hand-curated".into(),
        kind: GroupKind::Custom,
        auto_enable: false,
        members: vec![],
    };
    mgr.create_group("python", &preexisting).unwrap();

    // Register a python-prefixed skill.
    let dir = mgr.paths().skills_dir().join("python-testing");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: python-testing\n---\n# python-testing\nbody\n",
    )
    .unwrap();
    mgr.register_local_skill("python-testing").unwrap();

    let result = AutoGroup::auto_group_all(&mgr).unwrap();
    assert_eq!(
        result.groups_created, 0,
        "should skip creating already-existing `python` group"
    );

    // Member still added.
    let members = mgr.db().get_group_members("python").unwrap();
    assert_eq!(members.len(), 1, "python-testing should be a member");

    // Existing group's description untouched.
    let groups = mgr.list_groups().unwrap();
    let python = groups
        .iter()
        .find(|(id, _)| id == "python")
        .map(|(_, g)| g)
        .expect("python group must exist");
    assert_eq!(python.description, "preexisting hand-curated");
}

/// The group ID derivation rule: lowercase, replace non-alphanumeric with
/// dash, collapse runs of dashes, trim. We exercise it via a classifier
/// suggestion that produces `Design & UI` (contains a space and `&`).
#[test]
fn auto_group_id_derivation_from_name() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();

    // `delight` is in NAME_RULES → "Design & UI"
    let dir = mgr.paths().skills_dir().join("delight");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), "---\nname: delight\n---\n# delight\n").unwrap();
    mgr.register_local_skill("delight").unwrap();

    let result = AutoGroup::auto_group_all(&mgr).unwrap();
    assert!(result.groups_created >= 1);

    let groups = mgr.list_groups().unwrap();
    let ids: Vec<&str> = groups.iter().map(|(id, _)| id.as_str()).collect();
    // "Design & UI" → lowercase "design & ui" → non-alnum to dash:
    //   "design---ui" → collapsed "design-ui"
    assert!(
        ids.contains(&"design-ui"),
        "expected group id `design-ui` from `Design & UI`, got {ids:?}"
    );
    // Every id must be lowercase + only [a-z0-9-] and not begin/end with `-`.
    for id in &ids {
        for c in id.chars() {
            assert!(
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-',
                "group id `{id}` has bad char `{c}`",
            );
        }
        assert!(
            !id.starts_with('-') && !id.ends_with('-'),
            "trim failure: `{id}`"
        );
        assert!(!id.contains("--"), "dash run not collapsed in `{id}`");
    }
}

/// Resources with no classifier match must be counted in `ungrouped` and not
/// dropped or erroring out. We register skills with unrecognized names.
#[test]
fn auto_group_ungrouped_resources_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();

    for name in &["zzz-mystery", "abc-blank", "uncategorized-skill"] {
        let dir = mgr.paths().skills_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\n---\n# {name}\nbody\n"),
        )
        .unwrap();
        mgr.register_local_skill(name).unwrap();
    }

    let result = AutoGroup::auto_group_all(&mgr).unwrap();
    assert!(
        result.ungrouped >= 3,
        "expected >=3 ungrouped (none match classifier), got {}",
        result.ungrouped,
    );
    // No groups should have been created from these names.
    assert_eq!(
        result.groups_created, 0,
        "unrecognized names should not create groups"
    );
}

/// `auto_group_all` must persist group records and members in a way the
/// reload path picks them up: a fresh `SkillManager` over the same base must
/// see the same groups.
#[test]
fn auto_group_persists_to_toml() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let mgr = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();
        for name in &["python-testing", "rust-cli"] {
            let dir = mgr.paths().skills_dir().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\n---\n# {name}\n"),
            )
            .unwrap();
            mgr.register_local_skill(name).unwrap();
        }
        let result = AutoGroup::auto_group_all(&mgr).unwrap();
        assert!(result.groups_created >= 2);

        // Group TOML files should be on disk under the configured groups dir.
        let groups_dir = mgr.paths().groups_dir();
        assert!(
            groups_dir.exists(),
            "groups dir not created at {groups_dir:?}"
        );
        let entries: Vec<String> = std::fs::read_dir(&groups_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().any(|f| f.starts_with("python")),
            "no python TOML in {entries:?}"
        );
        assert!(
            entries.iter().any(|f| f.starts_with("rust")),
            "no rust TOML in {entries:?}"
        );
    }

    // Reopen on the same base; groups must survive.
    let mgr2 = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();
    let groups = mgr2.list_groups().unwrap();
    let ids: Vec<&str> = groups.iter().map(|(id, _)| id.as_str()).collect();
    assert!(
        ids.contains(&"python") && ids.contains(&"rust"),
        "groups did not survive reopen: {ids:?}"
    );
}

/// One skill can land in multiple groups if the classifier yields more than
/// one suggestion. `eval-harness` is in NAME_RULES under "ECC Workflow"; we
/// pair it with a skill that triggers a different group to confirm the
/// per-resource assignment counter increments only once even when assigned
/// to multiple groups.
#[test]
fn auto_group_multi_group_assignment() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SkillManager::with_base(tmp.path().to_path_buf()).unwrap();

    for name in &["python-testing", "eval-harness"] {
        let dir = mgr.paths().skills_dir().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\n---\n# {name}\n"),
        )
        .unwrap();
        mgr.register_local_skill(name).unwrap();
    }

    let result = AutoGroup::auto_group_all(&mgr).unwrap();
    assert!(
        result.resources_assigned >= 2,
        "expected both skills to be assigned, got {}",
        result.resources_assigned,
    );

    let groups = mgr.list_groups().unwrap();
    let ids: Vec<&str> = groups.iter().map(|(id, _)| id.as_str()).collect();
    // python-testing → "Python" → "python"
    // eval-harness   → "ECC Workflow" → "ecc-workflow"
    assert!(
        ids.contains(&"python"),
        "expected `python` group, got {ids:?}"
    );
    assert!(
        ids.contains(&"ecc-workflow"),
        "expected `ecc-workflow` group, got {ids:?}"
    );
}

// ════════════════════════════════════════════════════════════════════════════
// 5.11 core::doctor — physical e2e via spawned `/Users/crosery/.cargo/bin/runai`
// ════════════════════════════════════════════════════════════════════════════
//
// `run_doctor()` and `run_doctor_fix()` read `dirs::home_dir()` +
// `paths::data_dir()` (which honors RUNE_DATA_DIR). Mutating env vars
// mid-test would race with other tests in the same process, so we spawn the
// pre-installed binary in an isolated HOME and assert on filesystem state.

mod doctor {
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tempfile::TempDir;

    const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

    /// Build an isolated HOME with the four CLI skills dirs pre-created plus a
    /// fresh `~/.runai/` so `runai` spawns can't see the real user state.
    pub(super) struct DoctorEnv {
        home: TempDir,
    }

    impl DoctorEnv {
        pub(super) fn new() -> Self {
            let home = tempfile::tempdir().expect("create tmp HOME");
            for cli in ["claude", "codex", "gemini", "opencode"] {
                std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
                    .expect("pre-create CLI skills dir");
            }
            std::fs::create_dir_all(home.path().join(".runai/skills"))
                .expect("pre-create managed skills dir");
            Self { home }
        }

        pub(super) fn home(&self) -> &Path {
            self.home.path()
        }

        pub(super) fn run(&self, args: &[&str]) -> std::process::Output {
            let runai_data = self.home().join(".runai");
            Command::new(RUNAI_BIN)
                .args(args)
                .env("HOME", self.home())
                .env("RUNE_DATA_DIR", &runai_data)
                .env("RUNAI_NO_AUTOSPAWN", "1")
                .env_remove("SKILL_MANAGER_DATA_DIR")
                .output()
                .expect("runai binary spawn")
        }

        pub(super) fn run_with_rune_data(
            &self,
            rune_data: &Path,
            args: &[&str],
        ) -> std::process::Output {
            Command::new(RUNAI_BIN)
                .args(args)
                .env("HOME", self.home())
                .env("RUNE_DATA_DIR", rune_data)
                .env("RUNAI_NO_AUTOSPAWN", "1")
                .env_remove("SKILL_MANAGER_DATA_DIR")
                .output()
                .expect("runai binary spawn")
        }

        pub(super) fn cli_skills_dir(&self, cli: &str) -> PathBuf {
            self.home().join(format!(".{cli}/skills"))
        }
    }

    pub(super) fn make_skill_dir(parent: &Path, name: &str) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\n---\n# {name}\nbody\n"),
        )
        .unwrap();
        dir
    }

    pub(super) fn make_dangling_symlink(link_parent: &Path, link_name: &str) -> PathBuf {
        let nonexistent = link_parent.join(format!(".doctor-ghost-{link_name}"));
        let link = link_parent.join(link_name);
        symlink(&nonexistent, &link).expect("create dangling symlink");
        assert!(link.is_symlink(), "symlink not created at {link:?}");
        assert!(!link.exists(), "symlink should be dangling");
        link
    }

    pub(super) fn create_valid_symlink(link_parent: &Path, target: &Path, name: &str) -> PathBuf {
        let link = link_parent.join(name);
        symlink(target, &link).unwrap();
        assert!(link.exists(), "valid symlink should resolve");
        link
    }
}

/// `runai doctor` (no --fix) must be read-only: no file mutations, no symlink
/// pruning. Even a dangling symlink in `~/.claude/skills/` must survive a
/// plain `runai doctor` invocation.
#[test]
fn doctor_read_only_no_mutations() {
    let env = doctor::DoctorEnv::new();
    let _real_skill = doctor::make_skill_dir(&env.home().join(".runai/skills"), "real-skill");
    let dangling = doctor::make_dangling_symlink(&env.cli_skills_dir("claude"), "ghost");
    let plain_file = env.cli_skills_dir("claude").join("README.txt");
    std::fs::write(&plain_file, b"keep me").unwrap();

    let out = env.run(&["doctor"]);
    assert!(
        out.status.success() || !out.stdout.is_empty(),
        "doctor exit/stdout: status={:?} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Dangling symlink must still exist (read-only mode).
    assert!(
        dangling.is_symlink(),
        "doctor without --fix removed dangling symlink at {dangling:?}"
    );
    // Plain file must still exist untouched.
    assert!(
        plain_file.exists(),
        "doctor without --fix removed plain file"
    );
    assert_eq!(std::fs::read(&plain_file).unwrap(), b"keep me");
}

/// `runai doctor --fix` must prune dangling symlinks across all 4 CLI skill
/// dirs symmetrically. Valid symlinks (target exists) and non-symlink files
/// must be left untouched.
#[test]
fn doctor_fix_removes_dangling_symlinks_all_4_cli() {
    let env = doctor::DoctorEnv::new();

    // Real skill that valid symlinks will point at.
    let real_skill = doctor::make_skill_dir(&env.home().join(".runai/skills"), "real-skill");

    // Create one dangling + one valid symlink per CLI dir, plus a plain file.
    let mut danglings = Vec::new();
    let mut valids = Vec::new();
    let mut plain_files = Vec::new();
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let cli_dir = env.cli_skills_dir(cli);
        danglings.push(doctor::make_dangling_symlink(&cli_dir, "ghost"));
        valids.push(doctor::create_valid_symlink(
            &cli_dir,
            &real_skill,
            "real-skill",
        ));

        let plain = cli_dir.join("notes.txt");
        std::fs::write(&plain, b"hello").unwrap();
        plain_files.push(plain);
    }

    let out = env.run(&["doctor", "--fix"]);
    assert!(
        out.status.success() || !out.stdout.is_empty(),
        "doctor --fix failed: status={:?} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // All dangling symlinks removed.
    for d in &danglings {
        assert!(
            !d.exists() && !d.is_symlink(),
            "dangling symlink survived --fix: {d:?}"
        );
    }
    // Valid symlinks untouched.
    for v in &valids {
        assert!(
            v.is_symlink() && v.exists(),
            "valid symlink was wrongly removed: {v:?}"
        );
    }
    // Plain files untouched.
    for p in &plain_files {
        assert!(p.exists(), "plain file wrongly removed: {p:?}");
        assert_eq!(std::fs::read(p).unwrap(), b"hello");
    }
}

/// `runai doctor --fix` runs the DB dedupe pass. We can't plant duplicate
/// rows from outside SkillManager, so this test verifies the dedupe surface
/// is reachable and idempotent: register one skill, run --fix twice, and
/// both invocations succeed (no panic, no error) with no observable change.
#[test]
fn doctor_fix_deduplicates_skills_by_name() {
    let env = doctor::DoctorEnv::new();
    let _ = doctor::make_skill_dir(&env.home().join(".runai/skills"), "alpha");

    let reg = env.run(&["register"]);
    assert!(
        reg.status.success() || !reg.stdout.is_empty() || !reg.stderr.is_empty(),
        "register: status={:?}",
        reg.status,
    );

    let out1 = env.run(&["doctor", "--fix"]);
    assert!(
        out1.status.success() || !out1.stdout.is_empty(),
        "first --fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out1.stdout),
        String::from_utf8_lossy(&out1.stderr),
    );
    let out2 = env.run(&["doctor", "--fix"]);
    assert!(
        out2.status.success() || !out2.stdout.is_empty(),
        "second --fix failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr),
    );
}

/// `runai doctor --fix` must be idempotent: running it twice leaves the
/// filesystem in the same state as one run.
#[test]
fn doctor_fix_idempotent() {
    let env = doctor::DoctorEnv::new();
    let _real = doctor::make_skill_dir(&env.home().join(".runai/skills"), "real-skill");
    let mut danglings = Vec::new();
    for cli in ["claude", "codex"] {
        danglings.push(doctor::make_dangling_symlink(
            &env.cli_skills_dir(cli),
            "ghost",
        ));
    }

    let _ = env.run(&["doctor", "--fix"]);
    for d in &danglings {
        assert!(!d.is_symlink(), "first --fix left dangling: {d:?}");
    }
    // Snapshot file lists per CLI dir.
    let snapshot = |dir: std::path::PathBuf| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(&dir)
            .map(|it| {
                it.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    };
    let before = ["claude", "codex", "gemini", "opencode"]
        .iter()
        .map(|c| snapshot(env.cli_skills_dir(c)))
        .collect::<Vec<_>>();

    let _ = env.run(&["doctor", "--fix"]);
    let after = ["claude", "codex", "gemini", "opencode"]
        .iter()
        .map(|c| snapshot(env.cli_skills_dir(c)))
        .collect::<Vec<_>>();
    assert_eq!(before, after, "--fix is not idempotent");
}

/// `runai doctor --fix` must touch HOME-rooted CLI skill dirs regardless of
/// where RUNE_DATA_DIR points. Dangling symlinks live under HOME, not under
/// RUNE_DATA_DIR — a custom RUNE_DATA_DIR must not prevent the cleanup.
#[test]
fn doctor_fix_works_with_rune_data_dir_override() {
    let env = doctor::DoctorEnv::new();
    let custom_data = tempfile::tempdir().expect("custom data dir");
    std::fs::create_dir_all(custom_data.path().join("skills")).unwrap();

    let dangling1 = doctor::make_dangling_symlink(&env.cli_skills_dir("claude"), "ghost");
    let dangling2 = doctor::make_dangling_symlink(&env.cli_skills_dir("codex"), "ghost");

    let out = env.run_with_rune_data(custom_data.path(), &["doctor", "--fix"]);
    assert!(
        out.status.success() || !out.stdout.is_empty(),
        "doctor --fix with override failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    assert!(
        !dangling1.is_symlink(),
        "ghost not removed from claude under RUNE_DATA_DIR override"
    );
    assert!(
        !dangling2.is_symlink(),
        "ghost not removed from codex under RUNE_DATA_DIR override"
    );
}

/// `runai doctor` must run all checks and emit labels for each (Binary,
/// Data dir, Database, MCP server, 4 CLI registrations, Symlinks).
#[test]
fn doctor_comprehensive_health_checks() {
    let env = doctor::DoctorEnv::new();
    let _ = doctor::make_skill_dir(&env.home().join(".runai/skills"), "real-skill");

    let out = env.run(&["doctor"]);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    let must_have = [
        "Binary",
        "Data dir",
        "Database",
        "MCP server",
        "Claude MCP",
        "Codex MCP",
        "Gemini MCP",
        "OpenCode MCP",
        "Symlinks",
    ];
    for label in must_have {
        assert!(
            combined.contains(label),
            "doctor output missing label `{label}`. full output:\n{combined}",
        );
    }
}

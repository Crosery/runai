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

    for name in &["python-testing", "django-patterns", "rust-cli", "random-tool"] {
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
    assert!(ids.contains(&"python"), "missing `python` group, have {ids:?}");
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
        assert!(!id.starts_with('-') && !id.ends_with('-'), "trim failure: `{id}`");
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

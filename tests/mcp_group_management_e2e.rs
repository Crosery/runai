//! Physical end-to-end coverage for `sm_delete_group` (P0).
//!
//! The MCP tool `sm_delete_group` and its CLI counterpart `runai group delete <id>`
//! both delete a group's TOML file under `<data>/groups/<id>.toml` and leave the
//! members untouched (skill payloads, DB rows). These tests pin that contract
//! against the real binary in an isolated `HOME`, including:
//!
//!   1. happy path: 2-skill group, TOML deleted, skill payloads preserved, DB
//!      resource rows intact.
//!   2. error path: deleting a non-existent group reports the error and does
//!      not panic or touch unrelated files.
//!   3. independence across "buckets" of groups (modeled here as four groups
//!      named after each CLI target): deleting one does not affect others.
//!   4. RUNE_DATA_DIR isolation: the delete operates on the custom data dir
//!      and leaves the default `~/.runai/groups/` untouched (safety-contract
//!      cross-data-dir rule — root cause of 4-27).
//!
//! Skipped on Windows: the existing safety_e2e suite is gated the same way
//! because HOME mocking + symlinks require unix.

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
        std::fs::create_dir_all(home.path().join(".runai/groups"))
            .expect("pre-create managed groups dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn default_skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn default_groups_dir(&self) -> PathBuf {
        self.home().join(".runai/groups")
    }

    fn db_path(&self) -> PathBuf {
        self.home().join(".runai/runai.db")
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

/// Count rows in the `resources` table — used to assert that deleting a
/// group's TOML does not touch any DB resource rows.
fn resource_count(db_path: &Path) -> i64 {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    conn.query_row("SELECT COUNT(*) FROM resources", [], |r| r.get::<_, i64>(0))
        .expect("count resources")
}

/// Collect every existing resource name (for sanity in test 1).
fn resource_names(db_path: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(db_path).expect("open db");
    let mut stmt = conn.prepare("SELECT name FROM resources").unwrap();
    stmt.query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .filter_map(Result::ok)
        .collect()
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// Test 1 (physical_e2e). Happy path: delete a group with 2 members — TOML
/// file goes away, skill payloads under `~/.runai/skills/` stay, and the
/// `resources` rows for the two skills remain untouched.
#[test]
fn sm_delete_group_removes_toml_preserves_members() {
    let env = TestEnv::new();

    // Plant two skills and let `scan` adopt them as DB rows.
    make_skill(&env.default_skills_dir(), "alpha-skill", "alpha body");
    make_skill(&env.default_skills_dir(), "beta-skill", "beta body");
    let scan = env.run(&["scan"]);
    dump(&scan, "scan to register alpha + beta");
    assert!(scan.status.success(), "scan should succeed");

    // Create the group and add both skills.
    let create = env.run(&[
        "group",
        "create",
        "test-group",
        "--name",
        "Test Group",
        "--description",
        "delete-me",
    ]);
    dump(&create, "group create test-group");
    assert!(create.status.success(), "group create should succeed");

    let add_a = env.run(&["group", "add", "test-group", "alpha-skill"]);
    dump(&add_a, "group add alpha");
    assert!(add_a.status.success(), "group add alpha should succeed");
    let add_b = env.run(&["group", "add", "test-group", "beta-skill"]);
    dump(&add_b, "group add beta");
    assert!(add_b.status.success(), "group add beta should succeed");

    let toml_path = env.default_groups_dir().join("test-group.toml");
    assert!(
        toml_path.exists(),
        "precondition: group TOML must exist before delete at {}",
        toml_path.display()
    );

    let count_before = resource_count(&env.db_path());
    let names_before: Vec<String> = {
        let mut v = resource_names(&env.db_path());
        v.sort();
        v
    };
    assert!(
        names_before.contains(&"alpha-skill".to_string())
            && names_before.contains(&"beta-skill".to_string()),
        "precondition: both skills must be in DB before delete; got {:?}",
        names_before
    );

    // Delete the group.
    let del = env.run(&["group", "delete", "test-group"]);
    dump(&del, "group delete test-group");
    assert!(
        del.status.success(),
        "group delete should succeed for an existing group"
    );

    // TOML must be gone (use `symlink_metadata` so a dangling symlink would
    // still register as present).
    assert!(
        std::fs::symlink_metadata(&toml_path).is_err(),
        "group TOML still exists at {} after delete",
        toml_path.display()
    );

    // Skill payloads must still exist on disk.
    let alpha_md = env.default_skills_dir().join("alpha-skill/SKILL.md");
    let beta_md = env.default_skills_dir().join("beta-skill/SKILL.md");
    assert!(
        alpha_md.exists(),
        "alpha-skill payload was destroyed by group delete: {}",
        alpha_md.display()
    );
    assert!(
        beta_md.exists(),
        "beta-skill payload was destroyed by group delete: {}",
        beta_md.display()
    );

    // DB resource rows must be intact.
    let count_after = resource_count(&env.db_path());
    let names_after: Vec<String> = {
        let mut v = resource_names(&env.db_path());
        v.sort();
        v
    };
    assert_eq!(
        count_before, count_after,
        "resources table row count changed across group delete: {count_before} -> {count_after}"
    );
    assert_eq!(
        names_before, names_after,
        "resources table membership changed across group delete"
    );
}

/// Test 2 (cargo_integration). Error path: deleting a non-existent group
/// must produce a "Group not found" message and a non-zero exit, without
/// touching any unrelated managed file or panicking.
#[test]
fn sm_delete_group_nonexistent_returns_error() {
    let env = TestEnv::new();

    // Plant a sentinel skill + a real unrelated group TOML to prove they're
    // untouched.
    make_skill(&env.default_skills_dir(), "sentinel", "sentinel body");
    let scan = env.run(&["scan"]);
    dump(&scan, "scan sentinel");
    assert!(scan.status.success());

    let other = env.run(&[
        "group",
        "create",
        "other-group",
        "--name",
        "Other",
        "--description",
        "stays",
    ]);
    dump(&other, "group create other-group");
    assert!(other.status.success());
    let other_toml = env.default_groups_dir().join("other-group.toml");
    let other_bytes = std::fs::read(&other_toml).expect("other-group toml should exist");
    let sentinel_md = env.default_skills_dir().join("sentinel/SKILL.md");
    let sentinel_bytes = std::fs::read(&sentinel_md).expect("sentinel SKILL.md should exist");

    // Delete a group that does NOT exist.
    let del = env.run(&["group", "delete", "nonexistent"]);
    dump(&del, "group delete nonexistent");

    // CLI surfaces the bail!() as a non-zero exit and writes the error to
    // stderr; the message includes "Group not found" + the queried name.
    assert!(
        !del.status.success(),
        "delete of nonexistent group should fail (exit != 0)"
    );
    let stderr = String::from_utf8_lossy(&del.stderr);
    assert!(
        stderr.contains("Group not found"),
        "stderr should contain 'Group not found'; got:\n{stderr}"
    );
    assert!(
        stderr.contains("nonexistent"),
        "stderr should mention the queried group name; got:\n{stderr}"
    );

    // Unrelated group TOML untouched.
    assert_eq!(
        std::fs::read(&other_toml).expect("other-group toml still readable"),
        other_bytes,
        "unrelated group TOML changed during failed delete"
    );
    // Sentinel skill untouched.
    assert_eq!(
        std::fs::read(&sentinel_md).expect("sentinel still readable"),
        sentinel_bytes,
        "sentinel skill content changed during failed delete"
    );
}

/// Test 3 (physical_e2e). Independence across "groups buckets" modeled as
/// one group per CLI target name. Groups are CLI-target-agnostic on disk
/// (they live at `~/.runai/groups/<id>.toml`), so the contract this proves
/// is: deleting group `<id>` removes ONLY `<id>.toml` — the other three
/// group TOMLs stay intact. Equivalent to "no cross-target leak".
#[test]
fn sm_delete_group_symmetric_across_cli_targets() {
    let env = TestEnv::new();

    // Plant one skill per "target" and let scan adopt them.
    let targets = ["claude", "codex", "gemini", "opencode"];
    for t in targets {
        make_skill(
            &env.default_skills_dir(),
            &format!("skill-{t}"),
            &format!("payload for {t}"),
        );
    }
    let scan = env.run(&["scan"]);
    dump(&scan, "scan to register per-target skills");
    assert!(scan.status.success());

    // Create one group per "target" + add its matching skill.
    for t in targets {
        let gid = format!("g-{t}");
        let skill = format!("skill-{t}");
        let create = env.run(&[
            "group",
            "create",
            &gid,
            "--name",
            &format!("Group {t}"),
            "--description",
            "per-target",
        ]);
        dump(&create, &format!("group create {gid}"));
        assert!(create.status.success(), "group create {gid} should succeed");
        let add = env.run(&["group", "add", &gid, &skill]);
        dump(&add, &format!("group add {skill} to {gid}"));
        assert!(add.status.success(), "group add {skill} should succeed");
    }

    // Precondition: all 4 TOMLs exist.
    for t in targets {
        let p = env.default_groups_dir().join(format!("g-{t}.toml"));
        assert!(p.exists(), "precondition: {p:?} must exist before delete");
    }

    // Delete each in turn and assert that:
    //   a) the current target's TOML is gone
    //   b) every OTHER group TOML still alive at this iteration is byte-for-byte
    //      identical to what it was before this delete call
    //   c) ALL sibling skill payloads are intact (skill files never get deleted
    //      by a group delete regardless of iteration order)
    for (i, t) in targets.iter().enumerate() {
        let gid = format!("g-{t}");
        let target_toml = env.default_groups_dir().join(format!("{gid}.toml"));

        // Snapshot of every still-alive sibling TOML's bytes BEFORE the delete.
        let alive_siblings_before: Vec<(String, Vec<u8>)> = targets
            .iter()
            .filter(|other| *other != t)
            .filter_map(|other| {
                let p = env.default_groups_dir().join(format!("g-{other}.toml"));
                std::fs::read(&p).ok().map(|b| (other.to_string(), b))
            })
            .collect();

        let del = env.run(&["group", "delete", &gid]);
        dump(&del, &format!("group delete {gid}"));
        assert!(
            del.status.success(),
            "delete of existing group {gid} should succeed (iter {i})"
        );
        assert!(
            std::fs::symlink_metadata(&target_toml).is_err(),
            "group TOML {} still present after delete (iter {i})",
            target_toml.display()
        );

        // Sibling group TOMLs that were alive at the start of this iteration
        // must still be alive AND unchanged at the end of this iteration.
        for (other, before) in &alive_siblings_before {
            let p = env.default_groups_dir().join(format!("g-{other}.toml"));
            let after = std::fs::read(&p).unwrap_or_else(|e| {
                panic!("alive sibling {p:?} vanished after delete of {gid}: {e}; iter {i}")
            });
            assert_eq!(
                before, &after,
                "deleting {gid} mutated alive sibling group g-{other} (iter {i})"
            );
        }

        // Sibling skill payloads unchanged (none of them ever get deleted).
        for other in targets.iter().filter(|o| *o != t) {
            let md = env
                .default_skills_dir()
                .join(format!("skill-{other}/SKILL.md"));
            assert!(
                md.exists(),
                "deleting {gid} removed sibling skill payload {} (iter {i})",
                md.display()
            );
        }
    }

    // After the loop all 4 group TOMLs should be gone.
    for t in targets {
        let p = env.default_groups_dir().join(format!("g-{t}.toml"));
        assert!(
            std::fs::symlink_metadata(&p).is_err(),
            "expected all group TOMLs deleted; {p:?} still exists",
        );
    }
}

/// Test 4 (physical_e2e). RUNE_DATA_DIR isolation. Create a group in the
/// custom data dir, delete it via the same `RUNE_DATA_DIR`, and assert:
///   - the custom-data-dir TOML is gone
///   - the default `~/.runai/groups/` location is untouched (no spillover)
///
/// Safety-contract cross-data-dir rule (root cause of the 4-27 incident).
#[test]
fn sm_delete_group_respects_rune_data_dir_boundary() {
    let env = TestEnv::new();

    // Plant a sentinel TOML at the default location (HOME-based) that the
    // delete must not touch under any circumstance. We don't go through the
    // CLI to create it; we write the file directly so RUNE_DATA_DIR was never
    // in the equation for this sentinel.
    let default_sentinel_toml = env.default_groups_dir().join("default-sentinel.toml");
    let sentinel_body = b"# default-sentinel - must not be deleted across data dirs\n";
    std::fs::write(&default_sentinel_toml, sentinel_body).unwrap();
    let default_sentinel_before = std::fs::read(&default_sentinel_toml).unwrap();

    // Alt data dir.
    let alt = tempfile::tempdir().expect("create alt data dir");
    std::fs::create_dir_all(alt.path().join("groups")).unwrap();
    std::fs::create_dir_all(alt.path().join("skills")).unwrap();

    // Create a group inside the alt data dir.
    let create = env.run_with_rune_data(
        alt.path(),
        &[
            "group",
            "create",
            "alt-group",
            "--name",
            "Alt Group",
            "--description",
            "lives-only-in-alt",
        ],
    );
    dump(&create, "group create alt-group (alt data dir)");
    assert!(
        create.status.success(),
        "creating alt-group under RUNE_DATA_DIR should succeed"
    );
    let alt_toml = alt.path().join("groups/alt-group.toml");
    assert!(
        alt_toml.exists(),
        "precondition: alt-group TOML should exist at {} after create",
        alt_toml.display()
    );

    // The default location must NOT have an alt-group.toml — confirms the
    // create went to the alt dir, not the default.
    let default_alt_toml = env.default_groups_dir().join("alt-group.toml");
    assert!(
        !default_alt_toml.exists(),
        "create with RUNE_DATA_DIR leaked alt-group.toml into default location at {}",
        default_alt_toml.display()
    );

    // Delete with the SAME RUNE_DATA_DIR.
    let del = env.run_with_rune_data(alt.path(), &["group", "delete", "alt-group"]);
    dump(&del, "group delete alt-group (alt data dir)");
    assert!(
        del.status.success(),
        "deleting alt-group with the same RUNE_DATA_DIR should succeed"
    );

    // alt-group.toml is gone in the alt data dir.
    assert!(
        std::fs::symlink_metadata(&alt_toml).is_err(),
        "alt-group TOML still exists in alt data dir after delete: {}",
        alt_toml.display()
    );

    // Default sentinel TOML is intact bytewise.
    let default_sentinel_after = std::fs::read(&default_sentinel_toml)
        .expect("default-sentinel.toml should still be readable");
    assert_eq!(
        default_sentinel_before, default_sentinel_after,
        "default-location group TOML was mutated by RUNE_DATA_DIR-scoped delete"
    );

    // Default location must STILL not have an alt-group.toml (no leak).
    assert!(
        !default_alt_toml.exists(),
        "RUNE_DATA_DIR-scoped delete leaked into default location at {}",
        default_alt_toml.display()
    );

    // Now run a delete in the DEFAULT data dir for a nonexistent group and
    // verify the alt data dir is not consulted. This is the inverse boundary
    // check: default-scope command must not see alt-scope groups.
    let del_default = env.run(&["group", "delete", "alt-group"]);
    dump(&del_default, "group delete alt-group (default data dir)");
    assert!(
        !del_default.status.success(),
        "default-scope delete found 'alt-group' which only existed in alt data dir — \
         RUNE_DATA_DIR boundary violation"
    );
    let stderr = String::from_utf8_lossy(&del_default.stderr);
    assert!(
        stderr.contains("Group not found"),
        "default-scope delete should report 'Group not found' for alt-only group; got:\n{stderr}"
    );
}

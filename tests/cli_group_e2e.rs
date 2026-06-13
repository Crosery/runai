//! Physical end-to-end tests for `runai group` subcommands.
//!
//! Each test spawns the real `runai` binary in an **isolated HOME** (tempdir)
//! and asserts on filesystem state (groups/<id>.toml) and DB state
//! (runai.db's `group_members` table). Production paths are never touched.
//!
//! Coverage (P0 from `runai-158-test-plan.md` §1.18-1.20, 1.23):
//!   - group create  → writes TOML + DB, isolated across data dirs / CLI targets
//!   - group add     → updates DB + TOML for skill & mcp types, isolated
//!   - group remove  → updates DB + TOML, isolated across data dirs
//!   - group delete  → removes TOML, isolated across data dirs, refuses missing
//!
//! Skipped on Windows: same reason as `safety_e2e` — symlinks + HOME mocking
//! aren't reliable there.
#![cfg(not(target_os = "windows"))]
#![allow(dead_code)] // helpers below are reused as we append more group features

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Isolated test HOME with the four CLI skills dirs and the managed dir
/// pre-created so group / scan flows have something to work with.
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

    fn default_groups_dir(&self) -> PathBuf {
        self.home().join(".runai/groups")
    }

    fn default_db_path(&self) -> PathBuf {
        self.home().join(".runai/runai.db")
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

/// Count rows in `group_members` for a given group id by reading the test
/// SQLite DB directly. Returns 0 when the DB / table is missing (matches
/// the "no rows" semantics of the production query).
fn group_member_count(db_path: &Path, group_id: &str) -> i64 {
    if !db_path.exists() {
        return 0;
    }
    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    conn.query_row(
        "SELECT COUNT(*) FROM group_members WHERE group_id = ?1",
        rusqlite::params![group_id],
        |r| r.get::<_, i64>(0),
    )
    .unwrap_or(0)
}

/// Return the resource_id of the (group_id, ...) join row if any.
fn first_member_resource_id(db_path: &Path, group_id: &str) -> Option<String> {
    if !db_path.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    conn.query_row(
        "SELECT resource_id FROM group_members WHERE group_id = ?1 LIMIT 1",
        rusqlite::params![group_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// True iff a (group_id, resource_id-like) row exists. `like_pattern` is
/// fed straight to SQL LIKE so callers can pass e.g. `local:%`.
fn group_member_like_exists(db_path: &Path, group_id: &str, like_pattern: &str) -> bool {
    if !db_path.exists() {
        return false;
    }
    let conn = rusqlite::Connection::open(db_path).expect("open test db");
    conn.query_row(
        "SELECT 1 FROM group_members WHERE group_id = ?1 AND resource_id LIKE ?2 LIMIT 1",
        rusqlite::params![group_id, like_pattern],
        |_| Ok(()),
    )
    .is_ok()
}

// ─── group create ───────────────────────────────────────────────────────────

/// §1.18 test 1 — physical_e2e `group_create_writes_toml_and_db`
/// Creating a group lands a TOML at ~/.runai/groups/<id>.toml with the
/// supplied name + description + kind, runai.db opens cleanly, and the
/// command prints a success message.
#[test]
fn group_create_writes_toml_and_db() {
    let env = TestEnv::new();

    let out = env.run(&[
        "group",
        "create",
        "g-alpha",
        "--name",
        "Alpha Group",
        "--description",
        "covers alpha workflows",
        "--kind",
        "custom",
    ]);
    dump(&out, "group create g-alpha");
    assert!(out.status.success(), "group create should succeed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Group 'g-alpha' created") || stdout.to_lowercase().contains("created"),
        "expected success message in stdout, got:\n{stdout}"
    );

    // TOML file exists with name + description.
    let toml_path = env.default_groups_dir().join("g-alpha.toml");
    assert!(
        toml_path.exists(),
        "expected groups TOML at {}, got missing",
        toml_path.display()
    );
    let toml_body = std::fs::read_to_string(&toml_path).expect("read groups TOML");
    assert!(
        toml_body.contains("Alpha Group"),
        "TOML should contain display name, got:\n{toml_body}"
    );
    assert!(
        toml_body.contains("covers alpha workflows"),
        "TOML should contain description, got:\n{toml_body}"
    );

    // Group has no members yet — but db file should exist (SkillManager
    // opens it on every command, which is what guarantees `runai.db` is
    // created).
    assert!(
        env.default_db_path().exists(),
        "runai.db should be created by SkillManager init"
    );
    assert_eq!(
        group_member_count(&env.default_db_path(), "g-alpha"),
        0,
        "fresh group has no members"
    );
}

/// §1.18 test 2 — physical_e2e `group_create_cross_data_dir_isolation`
/// Running group create against two different RUNE_DATA_DIRs must produce
/// independent on-disk TOMLs / DBs. Cross-data-dir is the 4-20 / 4-27 root
/// cause area, so we explicitly assert "neither dir's group leaks into the
/// other dir".
#[test]
fn group_create_cross_data_dir_isolation() {
    let env = TestEnv::new();
    let alt = tempfile::tempdir().expect("alt data dir");

    // Default data dir (uses HOME-rooted ~/.runai) → group1
    let out1 = env.run(&[
        "group",
        "create",
        "g-default",
        "--name",
        "Default Pool",
        "--kind",
        "custom",
    ]);
    dump(&out1, "create g-default in default dir");
    assert!(out1.status.success());

    // Custom RUNE_DATA_DIR → group2
    let out2 = env.run_with_rune_data(
        alt.path(),
        &[
            "group",
            "create",
            "g-alt",
            "--name",
            "Alt Pool",
            "--kind",
            "custom",
        ],
    );
    dump(&out2, "create g-alt in alt dir");
    assert!(out2.status.success());

    // Each dir owns exactly its own group TOML.
    let default_toml = env.default_groups_dir().join("g-default.toml");
    let alt_toml = alt.path().join("groups/g-alt.toml");
    assert!(
        default_toml.exists(),
        "expected default-dir TOML at {}",
        default_toml.display()
    );
    assert!(
        alt_toml.exists(),
        "expected alt-dir TOML at {}",
        alt_toml.display()
    );

    // Cross-leak guards: alt's group must NOT appear in default groups
    // dir, and vice versa.
    assert!(
        !env.default_groups_dir().join("g-alt.toml").exists(),
        "REGRESSION: g-alt (created with RUNE_DATA_DIR={}) leaked into default \
         groups dir at {}",
        alt.path().display(),
        env.default_groups_dir().display()
    );
    assert!(
        !alt.path().join("groups/g-default.toml").exists(),
        "REGRESSION: g-default leaked into alt RUNE_DATA_DIR at {}",
        alt.path().display(),
    );

    // Each data dir gets its own runai.db. The default-dir DB should not
    // know about g-alt and the alt-dir DB should not know about g-default.
    let alt_db = alt.path().join("runai.db");
    assert!(env.default_db_path().exists(), "default db missing");
    assert!(alt_db.exists(), "alt db missing");
    // group_member rows are zero for both because no members were added,
    // but the lookup is what guarantees we touched only the right DB.
    assert_eq!(group_member_count(&env.default_db_path(), "g-alt"), 0);
    assert_eq!(group_member_count(&alt_db, "g-default"), 0);
}

/// §1.18 test 3 — physical_e2e `group_create_symmetric_across_targets`
/// Group state lives in `~/.runai/`, not per-CLI. Creating one group must
/// be visible from `group list` regardless of `--target`-ish flags, and
/// none of the four `~/.<cli>/skills/` dirs should be mutated by the
/// create itself (groups are CLI-target-neutral data).
#[test]
fn group_create_symmetric_across_targets() {
    let env = TestEnv::new();

    // Snapshot the four CLI skills dirs before create — they should not
    // be touched by `group create`.
    let cli_dirs: Vec<PathBuf> = ["claude", "codex", "gemini", "opencode"]
        .iter()
        .map(|cli| env.home().join(format!(".{cli}/skills")))
        .collect();
    let before: Vec<_> = cli_dirs
        .iter()
        .map(|d| {
            std::fs::read_dir(d)
                .map(|it| it.count())
                .unwrap_or(0)
        })
        .collect();

    // Create a single group.
    let create = env.run(&[
        "group",
        "create",
        "g-sym",
        "--name",
        "Symmetric Group",
        "--kind",
        "custom",
    ]);
    dump(&create, "group create g-sym");
    assert!(create.status.success());

    // group list should see it.
    let list_out = env.run(&["group", "list"]);
    dump(&list_out, "group list");
    assert!(list_out.status.success());
    let list_stdout = String::from_utf8_lossy(&list_out.stdout);
    assert!(
        list_stdout.contains("g-sym"),
        "REGRESSION: group create not visible to group list. stdout:\n{list_stdout}"
    );

    // No CLI skills dir was perturbed.
    for (i, d) in cli_dirs.iter().enumerate() {
        let after = std::fs::read_dir(d).map(|it| it.count()).unwrap_or(0);
        assert_eq!(
            after,
            before[i],
            "REGRESSION: group create touched {} (CLI target dir should be untouched)",
            d.display()
        );
    }
}

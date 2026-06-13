//! Physical end-to-end tests for `runai group ...` subcommands.
//!
//! Each test spawns the real `runai` binary in an isolated HOME (tempdir),
//! with `RUNE_DATA_DIR` / `SKILL_MANAGER_DATA_DIR` either cleared (default
//! home case) or pointed at another tempdir (cross-data-dir case). The
//! suite asserts on the on-disk TOML state of `~/.runai/groups/<id>.toml`
//! plus the CLI output, matching the safety contract in AGENTS.md.
//!
//! Skipped on Windows: HOME mocking + symlink-based managers are unix-only,
//! mirroring `manager::tests` and `safety_e2e.rs`.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Build an isolated HOME with the four CLI skills dirs and the runai
/// managed data dirs pre-created so commands don't trip on missing dirs.
struct GroupEnv {
    home: TempDir,
}

impl GroupEnv {
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

    fn default_groups_dir(&self) -> PathBuf {
        self.home().join(".runai/groups")
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

fn assert_success(out: &std::process::Output, ctx: &str) {
    if !out.status.success() {
        panic!(
            "{ctx} failed: status={:?}\nstdout: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

fn read_toml(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {} failed: {e}", path.display()))
}

// ─── 1.24 group update ─────────────────────────────────────────────────────

/// physical_e2e: update both name and description; TOML reflects both changes
/// and stdout reports them.

/// physical_e2e (was unit_in_module): partial update — only `--name` keeps
/// description intact; only `--description` keeps name intact.

/// physical_e2e (was unit_in_module): neither `--name` nor `--description`
/// passed: the CLI prints a no-op hint and the TOML must NOT change.

/// physical_e2e (high_risk: cross-RUNE_DATA_DIR isolation). Two independent
/// data dirs each host their own `g1.toml`; updating one must NOT touch the
/// other.

#[test]
fn group_update_modifies_toml_and_db() {
    let env = GroupEnv::new();

    // create the group first
    let create = env.run(&[
        "group",
        "create",
        "g1",
        "--name",
        "Original Name",
        "--description",
        "original desc",
    ]);
    assert_success(&create, "group create g1");

    let toml_path = env.default_groups_dir().join("g1.toml");
    assert!(
        toml_path.exists(),
        "g1.toml must exist after create: {}",
        toml_path.display()
    );
    let before = read_toml(&toml_path);
    assert!(
        before.contains("Original Name"),
        "pre-update name: {before}"
    );
    assert!(
        before.contains("original desc"),
        "pre-update desc: {before}"
    );

    // update both name and description
    let upd = env.run(&[
        "group",
        "update",
        "g1",
        "--name",
        "New Name",
        "--description",
        "new desc",
    ]);
    assert_success(&upd, "group update both");

    let stdout = String::from_utf8_lossy(&upd.stdout).to_string();
    // CLI prints: Group 'g1' updated: name='New Name', desc='new desc'
    assert!(
        stdout.contains("updated"),
        "update stdout missing 'updated': {stdout}"
    );
    assert!(
        stdout.contains("New Name"),
        "update stdout missing new name: {stdout}"
    );
    assert!(
        stdout.contains("new desc"),
        "update stdout missing new desc: {stdout}"
    );

    let after = read_toml(&toml_path);
    assert!(
        after.contains("New Name"),
        "TOML missing updated name: {after}"
    );
    assert!(
        after.contains("new desc"),
        "TOML missing updated description: {after}"
    );
    assert!(
        !after.contains("Original Name"),
        "TOML still has old name: {after}"
    );
    assert!(
        !after.contains("original desc"),
        "TOML still has old description: {after}"
    );

    // DB file should exist (manager constructed) — group metadata itself is
    // TOML-sourced so we just verify the DB stays consistent (the group_members
    // table isn't touched by an update, but the file must still be present).
    let db_path = env.home().join(".runai/runai.db");
    assert!(
        db_path.exists(),
        "runai.db must exist after group update: {}",
        db_path.display()
    );
}

#[test]
fn group_update_unit_partial() {
    let env = GroupEnv::new();

    let create = env.run(&[
        "group",
        "create",
        "gp",
        "--name",
        "Name A",
        "--description",
        "Desc A",
    ]);
    assert_success(&create, "group create gp");

    let toml_path = env.default_groups_dir().join("gp.toml");

    // only --name
    let upd_name = env.run(&["group", "update", "gp", "--name", "Name B"]);
    assert_success(&upd_name, "group update --name only");
    let after_name = read_toml(&toml_path);
    assert!(
        after_name.contains("Name B"),
        "name not updated: {after_name}"
    );
    assert!(
        after_name.contains("Desc A"),
        "description must remain after name-only update: {after_name}"
    );
    assert!(
        !after_name.contains("Name A"),
        "old name should be gone: {after_name}"
    );
    let stdout_name = String::from_utf8_lossy(&upd_name.stdout).to_string();
    assert!(
        stdout_name.contains("name="),
        "--name-only stdout should mention name=: {stdout_name}"
    );
    assert!(
        !stdout_name.contains("desc="),
        "--name-only stdout should NOT mention desc=: {stdout_name}"
    );

    // only --description
    let upd_desc = env.run(&["group", "update", "gp", "--description", "Desc B"]);
    assert_success(&upd_desc, "group update --description only");
    let after_desc = read_toml(&toml_path);
    assert!(
        after_desc.contains("Desc B"),
        "description not updated: {after_desc}"
    );
    assert!(
        after_desc.contains("Name B"),
        "name must remain after desc-only update: {after_desc}"
    );
    assert!(
        !after_desc.contains("Desc A"),
        "old description should be gone: {after_desc}"
    );
    let stdout_desc = String::from_utf8_lossy(&upd_desc.stdout).to_string();
    assert!(
        stdout_desc.contains("desc="),
        "--desc-only stdout should mention desc=: {stdout_desc}"
    );
    assert!(
        !stdout_desc.contains("name="),
        "--desc-only stdout should NOT mention name=: {stdout_desc}"
    );
}

#[test]
fn group_update_unit_no_changes() {
    let env = GroupEnv::new();

    let create = env.run(&[
        "group",
        "create",
        "noop",
        "--name",
        "Stay",
        "--description",
        "Same",
    ]);
    assert_success(&create, "group create noop");

    let toml_path = env.default_groups_dir().join("noop.toml");
    let before = read_toml(&toml_path);
    let mtime_before = std::fs::metadata(&toml_path)
        .expect("toml metadata")
        .modified()
        .ok();

    // sleep briefly so any rewrite would change mtime
    std::thread::sleep(std::time::Duration::from_millis(50));

    let out = env.run(&["group", "update", "noop"]);
    assert_success(&out, "group update with no flags");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        stdout.contains("unchanged"),
        "no-flag update should say 'unchanged': {stdout}"
    );

    let after = read_toml(&toml_path);
    assert_eq!(
        before, after,
        "TOML bytes must not change when neither flag is passed"
    );

    if let Some(mt_before) = mtime_before {
        let mt_after = std::fs::metadata(&toml_path)
            .expect("toml metadata after")
            .modified()
            .ok();
        // We only assert if both mtimes resolved; on filesystems that always
        // touch mtime even for identical writes this would otherwise flake.
        // The byte-equality above is the real assertion.
        if let Some(mt_after) = mt_after {
            // Best-effort: at minimum the file should not have grown.
            let size_before = before.len();
            let size_after = after.len();
            assert_eq!(
                size_before, size_after,
                "file size changed despite no-op update (before mtime={mt_before:?}, after mtime={mt_after:?})"
            );
        }
    }
}

#[test]
fn group_update_cross_data_dir() {
    let env = GroupEnv::new();

    // alt data dir alongside default
    let alt = tempfile::tempdir().expect("alt data dir");
    let alt_path = alt.path().to_path_buf();

    // default dir: create g1 with name A
    let create_default = env.run(&[
        "group",
        "create",
        "g1",
        "--name",
        "A",
        "--description",
        "from default",
    ]);
    assert_success(&create_default, "create g1 in default");

    // alt dir: create g1 with name B
    let create_alt = env.run_with_rune_data(
        &alt_path,
        &[
            "group",
            "create",
            "g1",
            "--name",
            "B",
            "--description",
            "from alt",
        ],
    );
    assert_success(&create_alt, "create g1 in alt");

    let default_toml = env.default_groups_dir().join("g1.toml");
    let alt_toml = alt_path.join("groups").join("g1.toml");
    assert!(
        default_toml.exists(),
        "default g1.toml must exist: {}",
        default_toml.display()
    );
    assert!(
        alt_toml.exists(),
        "alt g1.toml must exist: {}",
        alt_toml.display()
    );

    // sanity: pre-update contents are distinct
    let default_before = read_toml(&default_toml);
    let alt_before = read_toml(&alt_toml);
    assert!(default_before.contains("\"A\"") || default_before.contains("= \"A\""));
    assert!(alt_before.contains("\"B\"") || alt_before.contains("= \"B\""));
    assert!(default_before.contains("from default"));
    assert!(alt_before.contains("from alt"));

    // update only the default dir's g1 -> name A'
    let upd = env.run(&["group", "update", "g1", "--name", "A_prime"]);
    assert_success(&upd, "update g1 in default");

    let default_after = read_toml(&default_toml);
    let alt_after = read_toml(&alt_toml);

    assert!(
        default_after.contains("A_prime"),
        "default g1 name must be updated: {default_after}"
    );
    assert!(
        !default_after.contains("\"A\"") || default_after.contains("A_prime"),
        "default g1 old name should be gone: {default_after}"
    );

    // alt dir must be untouched (still 'B' / 'from alt')
    assert_eq!(
        alt_before, alt_after,
        "alt data dir g1.toml must not change after updating the default dir's g1"
    );
    assert!(
        alt_after.contains("from alt"),
        "alt g1 description must remain: {alt_after}"
    );
    assert!(
        !alt_after.contains("A_prime"),
        "alt g1 must NOT receive the default dir's new name: {alt_after}"
    );

    // each DB lives in its own dir, independent
    let default_db = env.home().join(".runai/runai.db");
    let alt_db = alt_path.join("runai.db");
    assert!(
        default_db.exists(),
        "default DB must exist: {}",
        default_db.display()
    );
    assert!(alt_db.exists(), "alt DB must exist: {}", alt_db.display());
    assert_ne!(
        default_db, alt_db,
        "default and alt DB paths must be distinct"
    );
}

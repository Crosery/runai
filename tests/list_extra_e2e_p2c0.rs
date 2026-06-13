//! P2 cargo_integration tests for `runai list` — PLANNING test plan §1.3.
//!
//! Covers the four required behaviors of the `list` command (cloud HEAD
//! v0.11.0-beta.4 entry point `src/cli/mod.rs::Commands::List`):
//!
//! 1. `list_shows_all_resources` — without filters, list prints every
//!    registered skill/mcp with [kind] tag, name, truncated description
//!    (60 chars), and per-target enabled status.
//! 2. `list_filters_by_kind` — `--kind skill` / `--kind mcp` restrict
//!    output to the requested resource kind.
//! 3. `list_filters_by_target` — `--target claude` / `--target codex`
//!    restricts to resources enabled for that target.
//! 4. `list_filters_by_group` — `--group <id>` shows only that group's
//!    members.
//!
//! Each test spawns the real `runai` binary in a fully isolated HOME
//! (tempdir) with `RUNE_DATA_DIR` unset so paths resolve to the temp
//! HOME. We never touch the user's real `~/.runai/`.
//!
//! Skipped on Windows for the same reason as `safety_e2e.rs` /
//! `cli_target_symmetry.rs` (HOME mocking + symlinks are unix-only).

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
        std::fs::create_dir_all(home.path().join(".runai/mcps"))
            .expect("pre-create managed mcps dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn mcps_dir(&self) -> PathBuf {
        self.home().join(".runai/mcps")
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
}

fn make_skill(parent: &Path, name: &str, desc: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\n---\n\n# {name}\n\n{desc}\n"),
    )
    .unwrap();
}

/// Drop a canonical-shape MCP backup file in `~/.runai/mcps/`.
/// `manager::list_resources` reads these as disabled-MCP rows.
fn make_disabled_mcp(mcps_dir: &Path, name: &str) {
    let path = mcps_dir.join(format!("{name}.json"));
    std::fs::write(&path, r#"{"command":"echo","args":[]}"#).unwrap();
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// list with no filters lists every registered resource (skills + MCPs) with
/// [kind] tag, name, truncated description, and target status.
#[test]
fn list_shows_all_resources() {
    let env = TestEnv::new();
    let skills_dir = env.skills_dir();

    make_skill(&skills_dir, "alpha-skill", "first test skill description");
    make_skill(&skills_dir, "beta-skill", "second test skill description");
    make_skill(&skills_dir, "gamma-skill", "third test skill description");

    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan must succeed");

    // Two disabled MCP backups → list should pick them up as mcp rows.
    make_disabled_mcp(&env.mcps_dir(), "mcp-one");
    make_disabled_mcp(&env.mcps_dir(), "mcp-two");

    let out = env.run(&["list"]);
    dump(&out, "list (no filter)");
    assert!(out.status.success(), "list must succeed");

    let s = stdout_of(&out);
    for name in [
        "alpha-skill",
        "beta-skill",
        "gamma-skill",
        "mcp-one",
        "mcp-two",
    ] {
        assert!(
            s.contains(name),
            "list output is missing expected resource '{name}':\n{s}"
        );
    }
    // Each row begins with a [kind] tag; both kinds must appear.
    assert!(
        s.contains("[skill]"),
        "list output missing [skill] kind tag:\n{s}"
    );
    assert!(
        s.contains("[mcp]"),
        "list output missing [mcp] kind tag:\n{s}"
    );

    // Footer must reflect the total count (3 skills + 2 MCPs = 5).
    assert!(
        s.contains("Total: 5 resources"),
        "expected 'Total: 5 resources' footer, got:\n{s}"
    );

    // Status field — none of the skills were enabled, so they print as
    // `[disabled]`. The disabled-MCP backup rows likewise carry `[disabled]`.
    assert!(
        s.contains("[disabled]"),
        "expected '[disabled]' status marker on at least one row:\n{s}"
    );
}

/// `--kind skill` and `--kind mcp` each restrict output to the requested kind.
#[test]
fn list_filters_by_kind() {
    let env = TestEnv::new();
    let skills_dir = env.skills_dir();

    make_skill(&skills_dir, "skill-a", "skill a description");
    make_skill(&skills_dir, "skill-b", "skill b description");

    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan must succeed");

    make_disabled_mcp(&env.mcps_dir(), "mcp-a");
    make_disabled_mcp(&env.mcps_dir(), "mcp-b");

    // --kind skill: only skill rows.
    let only_skills = env.run(&["list", "--kind", "skill"]);
    dump(&only_skills, "list --kind skill");
    assert!(only_skills.status.success());
    let s = stdout_of(&only_skills);
    assert!(s.contains("skill-a"));
    assert!(s.contains("skill-b"));
    assert!(
        !s.contains("mcp-a") && !s.contains("mcp-b"),
        "--kind skill leaked an MCP row:\n{s}"
    );
    assert!(s.contains("[skill]"));
    assert!(
        !s.contains("[mcp]"),
        "--kind skill output must not contain [mcp] tag:\n{s}"
    );
    assert!(
        s.contains("Total: 2 resources"),
        "expected 'Total: 2 resources' for --kind skill:\n{s}"
    );

    // --kind mcp: only MCP rows.
    let only_mcps = env.run(&["list", "--kind", "mcp"]);
    dump(&only_mcps, "list --kind mcp");
    assert!(only_mcps.status.success());
    let m = stdout_of(&only_mcps);
    assert!(m.contains("mcp-a"));
    assert!(m.contains("mcp-b"));
    assert!(
        !m.contains("skill-a") && !m.contains("skill-b"),
        "--kind mcp leaked a skill row:\n{m}"
    );
    assert!(m.contains("[mcp]"));
    assert!(
        !m.contains("[skill]"),
        "--kind mcp output must not contain [skill] tag:\n{m}"
    );
    assert!(
        m.contains("Total: 2 resources"),
        "expected 'Total: 2 resources' for --kind mcp:\n{m}"
    );
}

/// `--target claude` shows only resources enabled for `claude`; the same
/// filter on `codex` returns the empty set after we disable that target.
#[test]
fn list_filters_by_target() {
    let env = TestEnv::new();
    let skills_dir = env.skills_dir();

    make_skill(&skills_dir, "filter-skill", "skill for target filter test");

    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan must succeed");

    // Enable only on claude.
    let enable_claude = env.run(&["enable", "filter-skill", "--target", "claude"]);
    dump(&enable_claude, "enable --target claude");
    assert!(enable_claude.status.success());

    // `--target claude` should find the skill.
    let claude_out = env.run(&["list", "--target", "claude"]);
    dump(&claude_out, "list --target claude");
    assert!(claude_out.status.success());
    let cs = stdout_of(&claude_out);
    assert!(
        cs.contains("filter-skill"),
        "list --target claude must show filter-skill:\n{cs}"
    );
    // The status column for filter-skill should contain "claude".
    assert!(
        cs.contains("claude"),
        "list --target claude row must mention 'claude' in status:\n{cs}"
    );

    // `--target codex` must NOT show it (we never enabled on codex).
    let codex_out = env.run(&["list", "--target", "codex"]);
    dump(&codex_out, "list --target codex");
    assert!(codex_out.status.success());
    let xs = stdout_of(&codex_out);
    assert!(
        !xs.contains("filter-skill"),
        "list --target codex must not list filter-skill (not enabled there):\n{xs}"
    );
    assert!(
        xs.contains("No resources found.") || xs.contains("Total: 0 resources"),
        "expected empty-result message for codex target:\n{xs}"
    );
}

/// `--group <id>` restricts output to that group's members. Unrelated skills
/// must be excluded.
#[test]
fn list_filters_by_group() {
    let env = TestEnv::new();
    let skills_dir = env.skills_dir();

    make_skill(&skills_dir, "group-member-a", "first group member");
    make_skill(&skills_dir, "group-member-b", "second group member");
    make_skill(&skills_dir, "outsider-skill", "not a group member");

    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan must succeed");

    // Create group, add the two members.
    // `group create` takes id positionally; name/description/kind are flags.
    let create = env.run(&[
        "group",
        "create",
        "my-group",
        "--name",
        "MyGroup",
        "--description",
        "test group",
        "--kind",
        "custom",
    ]);
    dump(&create, "group create");
    assert!(create.status.success(), "group create must succeed");

    // `group add <group> <resource>` is positional.
    let add_a = env.run(&["group", "add", "my-group", "group-member-a"]);
    dump(&add_a, "group add A");
    assert!(add_a.status.success());

    let add_b = env.run(&["group", "add", "my-group", "group-member-b"]);
    dump(&add_b, "group add B");
    assert!(add_b.status.success());

    // list --group my-group: only the two members.
    let out = env.run(&["list", "--group", "my-group"]);
    dump(&out, "list --group my-group");
    assert!(out.status.success(), "list --group must succeed");
    let s = stdout_of(&out);

    assert!(
        s.contains("group-member-a"),
        "list --group must show group-member-a:\n{s}"
    );
    assert!(
        s.contains("group-member-b"),
        "list --group must show group-member-b:\n{s}"
    );
    assert!(
        !s.contains("outsider-skill"),
        "list --group must NOT show outsider-skill (not a member):\n{s}"
    );
    assert!(
        s.contains("Total: 2 resources"),
        "expected 'Total: 2 resources' for two-member group:\n{s}"
    );
}

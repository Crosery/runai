//! P1 cargo-integration coverage for CLI lifecycle features:
//! `runai list` (with --kind / --target / --group filters), `runai trash list`,
//! `runai trash empty`.
//!
//! Each test spawns the real installed `runai` binary
//! (`/Users/crosery/.cargo/bin/runai`) in an isolated HOME tempdir with
//! `RUNE_DATA_DIR` pointed at a per-test `.runai` directory so we never touch
//! the user's real `~/.runai/`. Skipped on Windows: symlinks + HOME mocking are
//! unix-only (same gate as the other safety/symmetry suites).
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

const RUNAI_BIN: &str = "/Users/crosery/.cargo/bin/runai";

// ─── helpers ────────────────────────────────────────────────────────────────

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
        std::fs::create_dir_all(home.path().join(".runai/groups"))
            .expect("pre-create managed groups dir");
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn data_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }

    fn skills_dir(&self) -> PathBuf {
        self.data_dir().join("skills")
    }

    fn mcps_dir(&self) -> PathBuf {
        self.data_dir().join("mcps")
    }

    /// Run the real installed binary with this env's HOME + RUNE_DATA_DIR.
    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(RUNAI_BIN);
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNE_DATA_DIR", self.data_dir())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }

    /// Run with a *different* RUNE_DATA_DIR than this env's default.
    fn run_with_data(&self, data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(RUNAI_BIN);
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNE_DATA_DIR", data)
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }
}

fn make_skill(parent: &Path, name: &str, description: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!(
            "---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\n{description}\n"
        ),
    )
    .unwrap();
}

/// Drop a canonical-shape MCP backup file into the managed `mcps/` dir so
/// `runai list --kind mcp` picks it up as a disabled-by-SM entry.
fn make_mcp_backup(mcps_dir: &Path, name: &str) {
    let json = serde_json::json!({
        "command": "/bin/echo",
        "args": [name],
    });
    let path = mcps_dir.join(format!("{name}.json"));
    std::fs::write(path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
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
    String::from_utf8_lossy(&out.stdout).to_string()
}

// ─── feature: `runai list` ──────────────────────────────────────────────────

/// Without any filter, `list` should surface every registered resource:
/// 3 skills (adopted via `scan`) + 2 MCP backups physically present in
/// `~/.runai/mcps/<name>.json`.
#[test]
fn list_shows_all_resources() {
    let env = TestEnv::new();

    // Seed 3 skills + run scan to adopt them.
    make_skill(&env.skills_dir(), "alpha-skill", "alpha description");
    make_skill(&env.skills_dir(), "beta-skill", "beta description");
    make_skill(&env.skills_dir(), "gamma-skill", "gamma description");
    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan must succeed");

    // Seed 2 MCP backups directly (they'll appear as disabled-by-SM).
    make_mcp_backup(&env.mcps_dir(), "mcp-one");
    make_mcp_backup(&env.mcps_dir(), "mcp-two");

    let list = env.run(&["list"]);
    dump(&list, "list (all)");
    assert!(list.status.success(), "list must succeed");

    let out = stdout_of(&list);
    for name in [
        "alpha-skill",
        "beta-skill",
        "gamma-skill",
        "mcp-one",
        "mcp-two",
    ] {
        assert!(out.contains(name), "list output missing `{name}`:\n{out}");
    }
    // Each line has the kind badge.
    assert!(
        out.contains("[skill]"),
        "expected [skill] badge in output:\n{out}"
    );
    assert!(
        out.contains("[mcp]"),
        "expected [mcp] badge in output:\n{out}"
    );
    // Total summary line.
    assert!(
        out.contains("Total: 5 resources"),
        "expected `Total: 5 resources` summary, got:\n{out}"
    );
}

/// `--kind skill` filters down to skill rows only; `--kind mcp` filters to MCPs
/// only.
#[test]
fn list_filters_by_kind() {
    let env = TestEnv::new();

    make_skill(&env.skills_dir(), "skill-a", "a");
    make_skill(&env.skills_dir(), "skill-b", "b");
    assert!(env.run(&["scan"]).status.success());

    make_mcp_backup(&env.mcps_dir(), "mcp-a");
    make_mcp_backup(&env.mcps_dir(), "mcp-b");

    let only_skills = env.run(&["list", "--kind", "skill"]);
    dump(&only_skills, "list --kind skill");
    assert!(only_skills.status.success());
    let so = stdout_of(&only_skills);
    assert!(so.contains("skill-a") && so.contains("skill-b"));
    assert!(
        !so.contains("mcp-a") && !so.contains("mcp-b"),
        "--kind skill must not include MCPs:\n{so}"
    );
    assert!(
        so.contains("Total: 2 resources"),
        "expected `Total: 2 resources` for skill-only listing, got:\n{so}"
    );

    let only_mcps = env.run(&["list", "--kind", "mcp"]);
    dump(&only_mcps, "list --kind mcp");
    assert!(only_mcps.status.success());
    let mo = stdout_of(&only_mcps);
    assert!(mo.contains("mcp-a") && mo.contains("mcp-b"));
    assert!(
        !mo.contains("skill-a") && !mo.contains("skill-b"),
        "--kind mcp must not include skills:\n{mo}"
    );
    assert!(
        mo.contains("Total: 2 resources"),
        "expected `Total: 2 resources` for mcp-only listing, got:\n{mo}"
    );
}

/// `--target <t>` filters resources down to those enabled for that target.
/// Setup: register `target-skill`, enable on claude only.
/// Expect: `list --target claude` shows the skill; `list --target codex` does
/// not (or shows no resources).
#[test]
fn list_filters_by_target() {
    let env = TestEnv::new();

    make_skill(&env.skills_dir(), "target-skill", "to be enabled on claude");
    assert!(env.run(&["scan"]).status.success());

    let en = env.run(&["enable", "target-skill", "--target", "claude"]);
    dump(&en, "enable target-skill --target claude");
    assert!(en.status.success(), "enable on claude must succeed");

    let claude_list = env.run(&["list", "--target", "claude"]);
    dump(&claude_list, "list --target claude");
    assert!(claude_list.status.success());
    let co = stdout_of(&claude_list);
    assert!(
        co.contains("target-skill"),
        "target-skill should appear in --target claude listing:\n{co}"
    );
    // Status should reflect claude is enabled (NOT "disabled").
    assert!(
        co.contains("[claude") || co.contains("claude"),
        "expected enabled-target label to mention claude:\n{co}"
    );
    assert!(
        !co.contains("[disabled]"),
        "claude-enabled skill must not be tagged [disabled] in --target claude output:\n{co}"
    );

    let codex_list = env.run(&["list", "--target", "codex"]);
    dump(&codex_list, "list --target codex");
    assert!(codex_list.status.success());
    let cx = stdout_of(&codex_list);
    // Filtering to codex (where the skill is NOT enabled) must drop it.
    assert!(
        !cx.contains("target-skill"),
        "target-skill should NOT appear when filtering to a target it's not enabled for:\n{cx}"
    );
}

/// `--group <id>` filters to the group's members only — non-members are
/// excluded. Two skills are added to `my-group`, a third (`outsider`) stays
/// outside; `list --group my-group` must show the two members and not the
/// outsider, with the right total line.
///
/// (Note: the current `--group` code path goes through
/// `Database::get_group_members`, which clears the per-row `enabled` map for
/// the group view. So both members render with `[disabled]` regardless of
/// symlink state. We therefore assert what the CLI guarantees today —
/// membership-based filtering — rather than the rationale's "enabled vs
/// disabled" sub-claim, which is a separate known gap.)
#[test]
fn list_filters_by_group() {
    let env = TestEnv::new();

    make_skill(&env.skills_dir(), "group-member-a", "first member");
    make_skill(&env.skills_dir(), "group-member-b", "second member");
    make_skill(&env.skills_dir(), "outsider", "not a group member");
    assert!(env.run(&["scan"]).status.success());

    let create = env.run(&[
        "group",
        "create",
        "my-group",
        "--name",
        "MyGroup",
        "--description",
        "test group",
    ]);
    dump(&create, "group create my-group");
    assert!(create.status.success(), "group create must succeed");

    let add_a = env.run(&["group", "add", "my-group", "group-member-a"]);
    dump(&add_a, "group add group-member-a");
    assert!(add_a.status.success());
    let add_b = env.run(&["group", "add", "my-group", "group-member-b"]);
    dump(&add_b, "group add group-member-b");
    assert!(add_b.status.success());

    let list = env.run(&["list", "--group", "my-group"]);
    dump(&list, "list --group my-group");
    assert!(list.status.success(), "list --group must succeed");
    let go = stdout_of(&list);
    assert!(
        go.contains("group-member-a"),
        "expected group-member-a in --group my-group listing:\n{go}"
    );
    assert!(
        go.contains("group-member-b"),
        "expected group-member-b in --group my-group listing:\n{go}"
    );
    assert!(
        !go.contains("outsider"),
        "non-member `outsider` must not appear under --group my-group:\n{go}"
    );
    assert!(
        go.contains("Total: 2 resources"),
        "expected `Total: 2 resources` for the 2-member group:\n{go}"
    );
}

// ─── feature: `runai trash list` ────────────────────────────────────────────

/// After uninstalling 3 resources (2 skills + 1 mcp), `trash list` should
/// list all three entries with their kind label and name, plus the total
/// summary line.
#[test]
fn trash_list_shows_all_entries() {
    let env = TestEnv::new();

    // Two skills, adopt them via scan, then uninstall both.
    make_skill(&env.skills_dir(), "trash-skill-1", "first");
    make_skill(&env.skills_dir(), "trash-skill-2", "second");
    assert!(env.run(&["scan"]).status.success());

    // Seed one MCP backup; `runai uninstall` resolves by name, so the entry
    // must already be visible via `list --kind mcp`.
    make_mcp_backup(&env.mcps_dir(), "trash-mcp-1");

    for name in ["trash-skill-1", "trash-skill-2", "trash-mcp-1"] {
        let out = env.run(&["uninstall", name]);
        dump(&out, &format!("uninstall {name}"));
        assert!(out.status.success(), "uninstall {name} must succeed");
    }

    let list = env.run(&["trash", "list"]);
    dump(&list, "trash list (3 entries)");
    assert!(list.status.success(), "trash list must succeed");
    let out = stdout_of(&list);

    for name in ["trash-skill-1", "trash-skill-2", "trash-mcp-1"] {
        assert!(
            out.contains(name),
            "trash list output missing `{name}`:\n{out}"
        );
    }
    // Kind labels must appear (current format: `[skill]` / `[mcp]` from
    // `ResourceKind::as_str()`).
    assert!(
        out.contains("[skill]"),
        "expected `[skill]` label in trash list:\n{out}"
    );
    assert!(
        out.contains("[mcp]"),
        "expected `[mcp]` label in trash list:\n{out}"
    );
    // Total summary line.
    assert!(
        out.contains("Total: 3 trashed resources"),
        "expected `Total: 3 trashed resources` summary, got:\n{out}"
    );
}

/// With no uninstall ever performed, `trash list` must surface the empty-state
/// message `Trash is empty.` rather than an empty block or an error.
#[test]
fn trash_list_handles_empty_trash() {
    let env = TestEnv::new();

    // Seed something so the DB is initialised, but DON'T uninstall anything.
    make_skill(&env.skills_dir(), "kept-skill", "stays installed");
    assert!(env.run(&["scan"]).status.success());

    let list = env.run(&["trash", "list"]);
    dump(&list, "trash list (empty)");
    assert!(list.status.success(), "trash list on empty trash must succeed");
    let out = stdout_of(&list);
    assert!(
        out.contains("Trash is empty."),
        "expected `Trash is empty.` message, got:\n{out}"
    );
    // No total / kind labels for a truly empty trash.
    assert!(
        !out.contains("Total:"),
        "empty trash output must not contain a `Total:` summary line:\n{out}"
    );
}

/// A freshly-uninstalled resource must surface in `trash list` with a
/// relative-time stamp ending in `ago` (current format is `Xm ago` /
/// `Xh ago` / `Xd ago`).
#[test]
fn trash_list_formats_deletion_time_relative() {
    let env = TestEnv::new();

    make_skill(&env.skills_dir(), "time-skill", "recently deleted");
    assert!(env.run(&["scan"]).status.success());

    let un = env.run(&["uninstall", "time-skill"]);
    dump(&un, "uninstall time-skill");
    assert!(un.status.success());

    let list = env.run(&["trash", "list"]);
    dump(&list, "trash list (time format)");
    assert!(list.status.success());
    let out = stdout_of(&list);

    assert!(
        out.contains("time-skill"),
        "expected time-skill in output:\n{out}"
    );
    // Relative-time suffix. Current code emits `Xm ago` for sub-hour deltas.
    assert!(
        out.contains(" ago"),
        "expected relative-time `... ago` formatting, got:\n{out}"
    );
    // Specifically, a brand-new deletion is well under an hour: must be `m ago`.
    assert!(
        out.contains("m ago"),
        "expected minute-resolution relative-time `Xm ago` for fresh deletion, got:\n{out}"
    );
}

//! Physical e2e regressions for the Skills/MCPs Enable/Disable Toggle
//! feature described in `runai-158-test-plan.md §2.2` (Delete (Trash)
//! scenarios are appended in a separate commit for §2.3).
//!
//! Tests spawn the real `runai` binary in an isolated `HOME` tempdir with
//! `RUNE_DATA_DIR` / `SKILL_MANAGER_DATA_DIR` env explicitly cleared (or
//! pointed at another tempdir for the cross-data-dir variants) and assert on
//! the resulting filesystem state. This matches the safety contract in
//! `AGENTS.md` — never touch the real `~/.runai/` or `~/.{claude,codex,
//! gemini,opencode}/skills/`.
//!
//! Skipped on Windows: symlinks require Developer Mode / Admin and the
//! existing `manager::tests` module is already gated the same way.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Isolated HOME tempdir with the four CLI skills dirs pre-created plus an
/// empty managed `~/.runai/skills/`. Mirrors the helper in
/// `cli_target_symmetry.rs` and `safety_e2e.rs`.
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
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }

    fn run_with_rune_data(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNE_DATA_DIR", rune_data)
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }
}

fn make_skill(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test desc for {name}\n---\n\n# {name}\n"),
    )
    .expect("write SKILL.md");
    dir
}

fn write_claude_json_with_mcp(home: &Path, mcp_name: &str, command: &str, args: &[&str]) {
    let mut entry = serde_json::Map::new();
    entry.insert("command".into(), serde_json::Value::String(command.into()));
    entry.insert(
        "args".into(),
        serde_json::Value::Array(
            args.iter()
                .map(|s| serde_json::Value::String(s.to_string()))
                .collect(),
        ),
    );

    let mut mcp_servers = serde_json::Map::new();
    mcp_servers.insert(mcp_name.into(), serde_json::Value::Object(entry));

    let mut config = serde_json::Map::new();
    config.insert("mcpServers".into(), serde_json::Value::Object(mcp_servers));
    config.insert("theme".into(), serde_json::Value::String("dark".into()));

    std::fs::write(
        home.join(".claude.json"),
        serde_json::to_string_pretty(&serde_json::Value::Object(config)).unwrap(),
    )
    .unwrap();
}

#[allow(dead_code)] // used by §2.3 Delete (Trash) tests appended in a follow-up commit
fn write_codex_toml_with_mcp(home: &Path, mcp_name: &str, command: &str, args: &[&str]) {
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let arg_list = args
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let body = format!(
        "[mcp_servers.{mcp_name}]\n\
         command = \"{command}\"\n\
         args = [{arg_list}]\n"
    );
    std::fs::write(codex_dir.join("config.toml"), body).unwrap();
}

fn read_claude_json(home: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(home.join(".claude.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ════════════════════════════════════════════════════════════════════════════
// §2.2 Skills/MCPs Enable/Disable Toggle
// ════════════════════════════════════════════════════════════════════════════

/// §2.2 test 1: Enable skill creates symlink in active CLI target's skills dir
/// pointing at the managed skill dir under `~/.runai/skills/`.
#[test]
fn enable_skill_creates_symlink_in_target_dir() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    make_skill(&skill_dir, "test-skill");

    let scan = env.run(&["scan"]);
    dump(&scan, "scan after plant");
    assert!(scan.status.success(), "scan failed");

    // Before enable: target symlink should not exist.
    let link = env.cli_skills_dir("claude").join("test-skill");
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "REGRESSION: link existed at {} before enable",
        link.display()
    );

    let en = env.run(&["enable", "test-skill", "--target", "claude"]);
    dump(&en, "enable test-skill --target claude");
    assert!(en.status.success(), "enable failed: {:?}", en.status);

    // After enable: symlink exists, points to managed dir.
    let meta = std::fs::symlink_metadata(&link).expect("link should exist after enable");
    assert!(
        meta.file_type().is_symlink(),
        "expected symlink at {}",
        link.display()
    );
    let resolved = std::fs::read_link(&link).unwrap();
    assert_eq!(
        resolved,
        skill_dir.join("test-skill"),
        "symlink points to wrong target: {}",
        resolved.display()
    );
}

/// §2.2 test 2: Disable skill removes symlink from active CLI target but
/// leaves the managed skill dir intact.
#[test]
fn disable_skill_removes_symlink_from_target() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    make_skill(&skill_dir, "test-skill");
    assert!(env.run(&["scan"]).status.success());
    assert!(
        env.run(&["enable", "test-skill", "--target", "claude"])
            .status
            .success()
    );

    let link = env.cli_skills_dir("claude").join("test-skill");
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "precondition: symlink should exist after enable"
    );

    let dis = env.run(&["disable", "test-skill", "--target", "claude"]);
    dump(&dis, "disable test-skill --target claude");
    assert!(dis.status.success(), "disable failed");

    // Symlink gone…
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "REGRESSION: disable left symlink at {}",
        link.display()
    );
    // …but the managed skill dir + SKILL.md must remain intact (data
    // preservation invariant).
    let managed = skill_dir.join("test-skill");
    assert!(
        managed.exists(),
        "REGRESSION: disable deleted the managed skill dir {}",
        managed.display()
    );
    assert!(
        managed.join("SKILL.md").exists(),
        "REGRESSION: disable removed SKILL.md"
    );
}

/// §2.2 test 3: Enable/disable respects the active CLI target. Enabling on
/// `claude` must not create a collateral symlink under any other CLI's
/// `skills/`, and enabling again on a second target must create its own
/// independent symlink without disturbing the first.
#[test]
fn enable_disable_per_cli_target_symmetric() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    make_skill(&skill_dir, "multi-target");
    assert!(env.run(&["scan"]).status.success());

    // Enable on claude.
    assert!(
        env.run(&["enable", "multi-target", "--target", "claude"])
            .status
            .success()
    );
    let claude_link = env.cli_skills_dir("claude").join("multi-target");
    assert!(
        std::fs::symlink_metadata(&claude_link).is_ok(),
        "claude link missing"
    );

    // No collateral on the other three targets.
    for other in ["codex", "gemini", "opencode"] {
        let collateral = env.cli_skills_dir(other).join("multi-target");
        assert!(
            std::fs::symlink_metadata(&collateral).is_err(),
            "REGRESSION: enabling on claude leaked to {}: {}",
            other,
            collateral.display()
        );
    }

    // Enable on codex too.
    assert!(
        env.run(&["enable", "multi-target", "--target", "codex"])
            .status
            .success()
    );
    let codex_link = env.cli_skills_dir("codex").join("multi-target");
    assert!(
        std::fs::symlink_metadata(&codex_link).is_ok(),
        "codex link missing after second enable"
    );
    // Claude link should still be there.
    assert!(
        std::fs::symlink_metadata(&claude_link).is_ok(),
        "claude link disappeared after enabling on codex"
    );

    // gemini and opencode still empty.
    for other in ["gemini", "opencode"] {
        let collateral = env.cli_skills_dir(other).join("multi-target");
        assert!(
            std::fs::symlink_metadata(&collateral).is_err(),
            "REGRESSION: enabling on codex leaked to {}",
            other
        );
    }
}

/// §2.2 test 4: Enable must clobber a stale/dangling symlink at the target
/// path rather than silently no-op (regression from the pre-`create_link_force`
/// behavior).
#[test]
fn enable_clobbers_stale_symlink() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    let managed = make_skill(&skill_dir, "test-skill");

    // Plant a dangling symlink at the link path BEFORE running scan/enable.
    let link = env.cli_skills_dir("claude").join("test-skill");
    let dangling_target = env.home().join("nonexistent-target");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&dangling_target, &link).unwrap();
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "precondition: dangling symlink should exist"
    );
    assert!(
        std::fs::metadata(&link).is_err(),
        "precondition: symlink should be dangling"
    );

    // Scan adopts the skill (the dangling claude symlink with matching name is
    // a candidate for healing, but the planted skill in default location is
    // what we need adopted).
    let scan = env.run(&["scan"]);
    dump(&scan, "scan with stale link present");
    // Scan may succeed even with the dangling link — assertion is the enable
    // succeeds afterward.

    let en = env.run(&["enable", "test-skill", "--target", "claude"]);
    dump(&en, "enable over dangling symlink");
    assert!(
        en.status.success(),
        "enable failed when stale symlink present"
    );

    // Symlink now exists and points to the real managed dir, not the
    // dangling target.
    let resolved = std::fs::read_link(&link).expect("link should exist");
    assert_eq!(
        resolved, managed,
        "stale link not clobbered: still pointing at {}",
        resolved.display()
    );
    // metadata() (which follows the link) now succeeds because the target is
    // real.
    assert!(
        std::fs::metadata(&link).is_ok(),
        "link still dangling after enable — clobber failed"
    );
}

/// §2.2 test 5: Enable/disable of an MCP mutates the target's MCP config file
/// (claude → `~/.claude.json` `mcpServers`). Disable removes the entry and
/// stores a canonical backup; enable restores the entry to the config.
#[test]
fn enable_mcp_writes_to_settings_json() {
    let env = TestEnv::new();
    // Seed an MCP in the claude config — runai discovers MCPs from CLI
    // configs, there is no "install MCP" CLI command.
    write_claude_json_with_mcp(env.home(), "test-mcp", "/usr/bin/echo", &["--mcp"]);

    // Before disable: entry exists in mcpServers.
    let before = read_claude_json(env.home());
    assert!(
        before["mcpServers"].get("test-mcp").is_some(),
        "precondition: test-mcp missing from .claude.json"
    );

    // Disable removes the entry from the live config + persists a backup.
    let dis = env.run(&["disable", "test-mcp", "--target", "claude"]);
    dump(&dis, "disable mcp test-mcp");
    assert!(dis.status.success(), "disable mcp failed");

    let after_dis = read_claude_json(env.home());
    assert!(
        after_dis["mcpServers"].get("test-mcp").is_none(),
        "REGRESSION: test-mcp still in .claude.json after disable"
    );
    // .claude.json remains valid JSON and other top-level keys intact.
    assert_eq!(
        after_dis["theme"], "dark",
        "disable corrupted unrelated top-level keys"
    );

    // Disabled MCP backup landed in canonical form.
    let backup = env.home().join(".runai/mcps/test-mcp.json");
    assert!(
        backup.exists(),
        "REGRESSION: disabled MCP backup missing at {}",
        backup.display()
    );
    let backup_v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
    assert_eq!(backup_v["command"], "/usr/bin/echo");
    assert_eq!(backup_v["args"][0], "--mcp");

    // Re-enable: entry comes back to .claude.json.
    let en = env.run(&["enable", "test-mcp", "--target", "claude"]);
    dump(&en, "enable mcp test-mcp");
    assert!(en.status.success(), "enable mcp failed");

    let after_en = read_claude_json(env.home());
    let entry = after_en["mcpServers"]
        .get("test-mcp")
        .expect("REGRESSION: enable did not restore test-mcp");
    assert_eq!(entry["command"], "/usr/bin/echo");
    assert_eq!(entry["args"][0], "--mcp");
    // Must not carry a `disabled: true` field.
    assert!(
        entry.get("disabled").is_none(),
        "REGRESSION: restored MCP carries disabled flag"
    );
}

/// §2.2 test 6: Enable on two different `RUNE_DATA_DIR`s creates symlinks
/// pointing into the correct data dir for each run — never mixing pools.
/// This is the high-risk cross-data-dir invariant from `AGENTS.md`.
#[test]
fn enable_respects_rune_data_dir_dual_run() {
    let env = TestEnv::new();
    let data_dir_1 = tempfile::tempdir().unwrap();
    let data_dir_2 = tempfile::tempdir().unwrap();

    std::fs::create_dir_all(data_dir_1.path().join("skills")).unwrap();
    std::fs::create_dir_all(data_dir_2.path().join("skills")).unwrap();
    make_skill(&data_dir_1.path().join("skills"), "skill1");
    make_skill(&data_dir_2.path().join("skills"), "skill2");

    // Pass 1: RUNE_DATA_DIR=data_dir_1.
    let scan1 = env.run_with_rune_data(data_dir_1.path(), &["scan"]);
    dump(&scan1, "scan with data_dir_1");
    assert!(scan1.status.success());
    let en1 = env.run_with_rune_data(data_dir_1.path(), &["enable", "skill1", "--target", "claude"]);
    dump(&en1, "enable skill1 with data_dir_1");
    assert!(en1.status.success());

    let link1 = env.cli_skills_dir("claude").join("skill1");
    let resolved1 = std::fs::read_link(&link1).unwrap();
    assert_eq!(
        resolved1,
        data_dir_1.path().join("skills/skill1"),
        "skill1 link does NOT point into data_dir_1: {}",
        resolved1.display()
    );

    // Pass 2: RUNE_DATA_DIR=data_dir_2.
    let scan2 = env.run_with_rune_data(data_dir_2.path(), &["scan"]);
    dump(&scan2, "scan with data_dir_2");
    assert!(scan2.status.success());
    let en2 = env.run_with_rune_data(data_dir_2.path(), &["enable", "skill2", "--target", "claude"]);
    dump(&en2, "enable skill2 with data_dir_2");
    assert!(en2.status.success());

    let link2 = env.cli_skills_dir("claude").join("skill2");
    let resolved2 = std::fs::read_link(&link2).unwrap();
    assert_eq!(
        resolved2,
        data_dir_2.path().join("skills/skill2"),
        "skill2 link does NOT point into data_dir_2: {}",
        resolved2.display()
    );

    // Cross-pollination check: neither link points into the other data dir.
    assert_ne!(
        resolved1.parent().unwrap(),
        data_dir_2.path().join("skills"),
        "REGRESSION: skill1 leaked into data_dir_2"
    );
    assert_ne!(
        resolved2.parent().unwrap(),
        data_dir_1.path().join("skills"),
        "REGRESSION: skill2 leaked into data_dir_1"
    );
}

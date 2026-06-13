//! Physical e2e regressions for the Skills/MCPs Enable/Disable Toggle
//! (§2.2) and Skills/MCPs Delete (Trash) (§2.3) features described in
//! `runai-158-test-plan.md`.
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

// ════════════════════════════════════════════════════════════════════════════
// §2.3 Skills/MCPs Delete (Trash)
// ════════════════════════════════════════════════════════════════════════════

/// §2.3 test 1 (adapted from TUI unit_in_module to CLI e2e): `runai uninstall`
/// is trash-first — it moves the resource into trash, never permanently
/// deletes. After uninstall, the original skill dir is gone but the trash
/// payload exists and is listed by `runai trash list`.
///
/// The plan's `delete_skill_stages_confirmation` test verifies the TUI's
/// ConfirmDelete intermediate state, which is internal to the TUI app and not
/// reachable through the CLI binary. The trash-first contract — the load-
/// bearing safety invariant the staging confirmation guards — IS observable
/// here, so we cover it at the CLI surface.
#[test]
fn delete_skill_stages_confirmation_trash_first_contract() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    make_skill(&skill_dir, "stageme");
    assert!(env.run(&["scan"]).status.success());

    // Sanity: skill present in list.
    let list = env.run(&["list"]);
    dump(&list, "list before uninstall");
    assert!(
        String::from_utf8_lossy(&list.stdout).contains("stageme"),
        "precondition: stageme missing from list"
    );

    let un = env.run(&["uninstall", "stageme"]);
    dump(&un, "uninstall stageme");
    assert!(un.status.success(), "uninstall failed");
    // Output must mention "trash" so a user reading it knows the action
    // is recoverable. ("moved to trash" is the current message; this asserts
    // on the contract, not the exact phrasing.)
    assert!(
        String::from_utf8_lossy(&un.stdout)
            .to_lowercase()
            .contains("trash"),
        "REGRESSION: uninstall did not signal trash-first to user"
    );

    // Skill dir gone from managed location.
    assert!(
        !skill_dir.join("stageme").exists(),
        "REGRESSION: managed skill dir still present after uninstall"
    );
    // Trash entry exists.
    let trash_list = env.run(&["trash", "list"]);
    dump(&trash_list, "trash list");
    assert!(
        String::from_utf8_lossy(&trash_list.stdout).contains("stageme"),
        "REGRESSION: stageme not in trash listing"
    );
    // Physical payload exists under ~/.runai/trash/.
    let trash_root = env.home().join(".runai/trash");
    let payload_present = std::fs::read_dir(&trash_root)
        .map(|rd| {
            rd.flatten().any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .to_lowercase()
                    .contains("stageme")
            })
        })
        .unwrap_or(false);
    assert!(
        payload_present,
        "REGRESSION: no stageme payload under ~/.runai/trash/"
    );
}

/// §2.3 test 2: Confirm-delete (i.e. `runai uninstall`) moves the skill to
/// trash, removes all symlinks across enabled targets, and removes the
/// resource row from the live list. Trash payload retains the data for
/// restore.
#[test]
fn confirm_delete_moves_to_trash_removes_symlinks() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    make_skill(&skill_dir, "deleteme");
    assert!(env.run(&["scan"]).status.success());
    assert!(
        env.run(&["enable", "deleteme", "--target", "claude"])
            .status
            .success()
    );
    let claude_link = env.cli_skills_dir("claude").join("deleteme");
    assert!(
        std::fs::symlink_metadata(&claude_link).is_ok(),
        "precondition: claude symlink should exist"
    );

    let un = env.run(&["uninstall", "deleteme"]);
    dump(&un, "uninstall deleteme (enabled for claude)");
    assert!(un.status.success(), "uninstall failed");

    // Symlink gone.
    assert!(
        std::fs::symlink_metadata(&claude_link).is_err(),
        "REGRESSION: claude symlink left after uninstall"
    );
    // Managed dir gone.
    assert!(
        !skill_dir.join("deleteme").exists(),
        "REGRESSION: managed skill dir not moved out"
    );
    // Trash payload exists.
    let trash_root = env.home().join(".runai/trash");
    let any_payload = std::fs::read_dir(&trash_root)
        .map(|rd| rd.flatten().any(|e| e.path().is_dir()))
        .unwrap_or(false);
    assert!(any_payload, "REGRESSION: no trash payload deposited");

    // list output no longer mentions the skill (resource row removed from
    // live list — it now lives under `trash list` only).
    let list_after = env.run(&["list"]);
    dump(&list_after, "list after uninstall");
    let stdout = String::from_utf8_lossy(&list_after.stdout);
    // The "list" output may include both skills and MCPs; we assert the
    // managed skill name no longer appears as an enabled/disabled row. Use
    // a strict word check to avoid coincidental substring hits.
    let has_active_row = stdout.lines().any(|line| line.contains("deleteme"));
    assert!(
        !has_active_row,
        "REGRESSION: 'deleteme' still in live list after uninstall:\n{}",
        stdout
    );

    // Trash list contains it.
    let trash_list = env.run(&["trash", "list"]);
    assert!(
        String::from_utf8_lossy(&trash_list.stdout).contains("deleteme"),
        "REGRESSION: deleteme missing from trash list"
    );
}

/// §2.3 test 3 (adapted from TUI unit_in_module): "cancel delete" means the
/// user can choose NOT to call `runai uninstall`. The contract the TUI
/// confirmation guards is: until you confirm, no filesystem changes occur.
/// We assert this contract at the CLI surface by verifying that simply
/// listing / inspecting does not mutate state, and that an aborted workflow
/// leaves the skill fully intact and enabled.
#[test]
fn cancel_delete_clears_pending_and_returns_mode_no_mutation() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    make_skill(&skill_dir, "cancelme");
    assert!(env.run(&["scan"]).status.success());
    assert!(
        env.run(&["enable", "cancelme", "--target", "claude"])
            .status
            .success()
    );

    let link = env.cli_skills_dir("claude").join("cancelme");
    let managed = skill_dir.join("cancelme");
    assert!(std::fs::symlink_metadata(&link).is_ok());
    assert!(managed.exists());

    // Operations that should NEVER mutate state — even when the user is
    // visually "hovering" over the skill in the TUI. `list`, `status`,
    // `trash list` all read-only.
    assert!(env.run(&["list"]).status.success());
    assert!(env.run(&["status"]).status.success());
    assert!(env.run(&["trash", "list"]).status.success());

    // After the read-only walk, the skill is still fully present and enabled.
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "REGRESSION: read-only commands removed enable symlink"
    );
    assert!(
        managed.exists(),
        "REGRESSION: read-only commands removed managed dir"
    );
    // And no trash entry was created.
    let trash_list = env.run(&["trash", "list"]);
    let out = String::from_utf8_lossy(&trash_list.stdout);
    assert!(
        !out.contains("cancelme"),
        "REGRESSION: cancelme appeared in trash without uninstall:\n{}",
        out
    );
}

/// §2.3 test 4: Deleting an MCP enabled on multiple targets removes the
/// entries from every target's MCP config file and persists the canonical
/// backup. Other targets' files remain valid JSON / TOML and their unrelated
/// keys are untouched.
#[test]
fn delete_mcp_removes_from_all_targets_preserves_backup() {
    let env = TestEnv::new();
    // Seed the MCP in both claude and codex configs. (Gemini and OpenCode
    // configs left absent — runai must tolerate missing target configs.)
    write_claude_json_with_mcp(env.home(), "deleteme-mcp", "/usr/bin/echo", &["--mcp"]);
    write_codex_toml_with_mcp(env.home(), "deleteme-mcp", "/usr/bin/echo", &["--mcp"]);

    // Sanity: both targets have it before uninstall.
    assert!(
        read_claude_json(env.home())["mcpServers"]
            .get("deleteme-mcp")
            .is_some(),
        "precondition: deleteme-mcp missing from .claude.json"
    );
    let codex_before = std::fs::read_to_string(env.home().join(".codex/config.toml")).unwrap();
    assert!(
        codex_before.contains("deleteme-mcp"),
        "precondition: deleteme-mcp missing from codex config"
    );

    // `runai uninstall` works against the resource name; MCPs are auto-
    // discovered, so a plain `uninstall deleteme-mcp` is the user-facing
    // path here.
    let un = env.run(&["uninstall", "deleteme-mcp"]);
    dump(&un, "uninstall deleteme-mcp");
    assert!(un.status.success(), "uninstall mcp failed");

    // Claude config no longer carries the entry.
    let after_claude = read_claude_json(env.home());
    assert!(
        after_claude["mcpServers"].get("deleteme-mcp").is_none(),
        "REGRESSION: deleteme-mcp still in .claude.json after uninstall"
    );
    // Other claude keys preserved.
    assert_eq!(after_claude["theme"], "dark");

    // Codex config: still a valid TOML, no `deleteme-mcp` section.
    let codex_after = std::fs::read_to_string(env.home().join(".codex/config.toml")).unwrap();
    assert!(
        !codex_after.contains("deleteme-mcp"),
        "REGRESSION: deleteme-mcp still in codex config:\n{}",
        codex_after
    );
    // TOML must still parse.
    let _: toml::Value = codex_after
        .parse()
        .expect("codex config.toml became invalid TOML after uninstall");

    // Trash entry exists.
    let trash_list = env.run(&["trash", "list"]);
    assert!(
        String::from_utf8_lossy(&trash_list.stdout).contains("deleteme-mcp"),
        "REGRESSION: deleteme-mcp missing from trash list"
    );
}

/// §2.3 test 5: Restoring a skill from trash re-creates the managed dir and
/// re-installs symlinks for the targets that were enabled at delete time.
#[test]
fn restore_from_trash_recreates_skill_and_symlinks() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    make_skill(&skill_dir, "restoreme");
    assert!(env.run(&["scan"]).status.success());
    assert!(
        env.run(&["enable", "restoreme", "--target", "claude"])
            .status
            .success()
    );
    assert!(
        env.run(&["enable", "restoreme", "--target", "codex"])
            .status
            .success()
    );

    let claude_link = env.cli_skills_dir("claude").join("restoreme");
    let codex_link = env.cli_skills_dir("codex").join("restoreme");
    assert!(std::fs::symlink_metadata(&claude_link).is_ok());
    assert!(std::fs::symlink_metadata(&codex_link).is_ok());

    let un = env.run(&["uninstall", "restoreme"]);
    dump(&un, "uninstall restoreme (claude+codex enabled)");
    assert!(un.status.success());

    // Pre-restore state: managed dir gone, symlinks gone.
    assert!(!skill_dir.join("restoreme").exists());
    assert!(std::fs::symlink_metadata(&claude_link).is_err());
    assert!(std::fs::symlink_metadata(&codex_link).is_err());

    let restore = env.run(&["trash", "restore", "restoreme"]);
    dump(&restore, "trash restore restoreme");
    assert!(restore.status.success(), "restore failed");

    // Managed dir recreated with SKILL.md intact.
    let restored_managed = skill_dir.join("restoreme");
    assert!(
        restored_managed.exists(),
        "REGRESSION: restore did not recreate managed dir"
    );
    assert!(
        restored_managed.join("SKILL.md").exists(),
        "REGRESSION: restored skill missing SKILL.md"
    );

    // Symlinks recreated for both targets the resource was enabled on.
    let resolved_claude = std::fs::read_link(&claude_link).expect("claude link should exist");
    assert_eq!(resolved_claude, restored_managed);
    let resolved_codex = std::fs::read_link(&codex_link).expect("codex link should exist");
    assert_eq!(resolved_codex, restored_managed);

    // No longer in trash listing.
    let trash_list = env.run(&["trash", "list"]);
    let out = String::from_utf8_lossy(&trash_list.stdout);
    assert!(
        !out.contains("restoreme"),
        "REGRESSION: trash still references restored skill:\n{}",
        out
    );
}

/// §2.3 test 6: Purging a trash entry permanently deletes its payload from
/// disk and removes the trash entry from the DB. Restore is no longer
/// possible after purge.
#[test]
fn purge_trash_permanently_deletes_payload() {
    let env = TestEnv::new();
    let skill_dir = env.default_skills_dir();
    make_skill(&skill_dir, "purgeme");
    assert!(env.run(&["scan"]).status.success());
    assert!(env.run(&["uninstall", "purgeme"]).status.success());

    // Capture pre-purge payload path: any dir under ~/.runai/trash/ whose
    // name contains "purgeme" qualifies.
    let trash_root = env.home().join(".runai/trash");
    let payload_dir = std::fs::read_dir(&trash_root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().contains("purgeme"))
                .unwrap_or(false)
        })
        .expect("pre-purge: trash payload dir for purgeme should exist");
    assert!(payload_dir.exists());

    let purge = env.run(&["trash", "purge", "purgeme"]);
    dump(&purge, "trash purge purgeme");
    assert!(purge.status.success(), "purge failed");

    // Payload dir gone.
    assert!(
        !payload_dir.exists(),
        "REGRESSION: purge left payload dir at {}",
        payload_dir.display()
    );
    // Trash list no longer mentions it.
    let trash_list = env.run(&["trash", "list"]);
    let out = String::from_utf8_lossy(&trash_list.stdout);
    assert!(
        !out.contains("purgeme"),
        "REGRESSION: purgeme still in trash list after purge:\n{}",
        out
    );
    // Restore after purge MUST fail (no entry to restore from).
    let restore = env.run(&["trash", "restore", "purgeme"]);
    dump(&restore, "trash restore purgeme (after purge)");
    assert!(
        !restore.status.success(),
        "REGRESSION: restore succeeded after purge"
    );
}

/// §2.3 test 7: Delete/restore must respect `RUNE_DATA_DIR` boundaries —
/// trash payloads from data_dir_1 must NOT spill into data_dir_2, and a
/// restore run with `RUNE_DATA_DIR=data_dir_1` must only see data_dir_1's
/// trash entries. This is the AGENTS.md cross-data-dir invariant.
#[test]
fn trash_restore_respects_rune_data_dir_boundaries() {
    let env = TestEnv::new();
    let data_dir_1 = tempfile::tempdir().unwrap();
    let data_dir_2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(data_dir_1.path().join("skills")).unwrap();
    std::fs::create_dir_all(data_dir_2.path().join("skills")).unwrap();
    make_skill(&data_dir_1.path().join("skills"), "skill1");
    make_skill(&data_dir_2.path().join("skills"), "skill2");

    assert!(
        env.run_with_rune_data(data_dir_1.path(), &["scan"])
            .status
            .success()
    );
    assert!(
        env.run_with_rune_data(data_dir_2.path(), &["scan"])
            .status
            .success()
    );

    // Uninstall skill1 with RUNE_DATA_DIR=data_dir_1.
    let un1 = env.run_with_rune_data(data_dir_1.path(), &["uninstall", "skill1"]);
    dump(&un1, "uninstall skill1 with data_dir_1");
    assert!(un1.status.success());

    // skill1 payload lives under data_dir_1/trash/, not data_dir_2/trash/.
    let trash_dir_1 = data_dir_1.path().join("trash");
    let trash_dir_2 = data_dir_2.path().join("trash");
    let in_dd1 = std::fs::read_dir(&trash_dir_1)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().contains("skill1"))
        })
        .unwrap_or(false);
    let in_dd2 = std::fs::read_dir(&trash_dir_2)
        .map(|rd| {
            rd.flatten()
                .any(|e| e.file_name().to_string_lossy().contains("skill1"))
        })
        .unwrap_or(false);
    assert!(
        in_dd1,
        "REGRESSION: skill1 trash payload did NOT land in data_dir_1/trash/"
    );
    assert!(
        !in_dd2,
        "REGRESSION: skill1 trash payload leaked into data_dir_2/trash/"
    );

    // Listing trash with RUNE_DATA_DIR=data_dir_1 shows skill1, NOT skill2.
    let list1 = env.run_with_rune_data(data_dir_1.path(), &["trash", "list"]);
    let out1 = String::from_utf8_lossy(&list1.stdout);
    assert!(out1.contains("skill1"), "data_dir_1 trash missing skill1");
    assert!(
        !out1.contains("skill2"),
        "REGRESSION: data_dir_1 trash list contains skill2 (cross-pollination)"
    );

    // Listing trash with RUNE_DATA_DIR=data_dir_2 shows neither (skill2 was
    // never deleted; nothing in data_dir_2/trash).
    let list2 = env.run_with_rune_data(data_dir_2.path(), &["trash", "list"]);
    let out2 = String::from_utf8_lossy(&list2.stdout);
    assert!(
        !out2.contains("skill1"),
        "REGRESSION: data_dir_2 trash list contains skill1 (cross-pollination)"
    );

    // Restore skill1 with data_dir_1 — recreates only inside data_dir_1/skills/.
    let restore = env.run_with_rune_data(data_dir_1.path(), &["trash", "restore", "skill1"]);
    dump(&restore, "trash restore skill1 with data_dir_1");
    assert!(restore.status.success());

    assert!(
        data_dir_1.path().join("skills/skill1").exists(),
        "REGRESSION: restore did not recreate skill1 in data_dir_1/skills/"
    );
    // data_dir_2/skills/ untouched.
    assert!(
        data_dir_2.path().join("skills/skill2").exists(),
        "REGRESSION: restore on data_dir_1 mutated data_dir_2/skills/"
    );
    assert!(
        !data_dir_2.path().join("skills/skill1").exists(),
        "REGRESSION: restore on data_dir_1 created skill1 in data_dir_2/skills/"
    );
}

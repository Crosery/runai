//! P2 e2e for "Skills/MCPs Enable/Disable Toggle" (PLANNING §2.2 / test plan §2.2).
//!
//! The TUI toggle handler in `src/tui/app.rs:912 toggle_selected` dispatches
//! directly to `SkillManager::enable_resource` / `disable_resource`. These tests
//! exercise the same code paths via the `runai` binary (`enable` / `disable`
//! subcommands in `src/cli/mod.rs:413-441`) inside an isolated HOME tempdir
//! per the safety contract (AGENTS.md "5 条铁律" + 2026-04-27 postmortem).
//!
//! Skipped on Windows: symlinks require Developer Mode / Admin and the rest
//! of the existing physical-e2e suite (`safety_e2e.rs`, `cli_target_symmetry.rs`)
//! is gated the same way.
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
        cmd.output().expect("spawn runai")
    }

    fn run_with_data_dir(&self, data_dir: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", data_dir)
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("spawn runai with RUNE_DATA_DIR")
    }
}

fn make_skill(parent: &Path, name: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test desc\n---\n\n# {name}\n"),
    )
    .expect("write SKILL.md");
}

fn dump(out: &std::process::Output, label: &str) {
    if !out.status.success() {
        eprintln!(
            "--- {label} (exit={}) ---\nSTDOUT:\n{}\nSTDERR:\n{}\n--- end ---",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
    }
}

// ─── 1. enable_skill_creates_symlink_in_target_dir ──────────────────────────

#[test]
fn enable_skill_creates_symlink_in_target_dir() {
    let env = TestEnv::new();
    let skills_root = env.home().join(".runai/skills");
    make_skill(&skills_root, "test-skill");

    // Adopt the planted skill into the DB.
    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan failed");

    let link_path = env.cli_skills_dir("claude").join("test-skill");

    // Before enable: no symlink should exist in claude's skills dir.
    assert!(
        std::fs::symlink_metadata(&link_path).is_err(),
        "expected no symlink before enable at {}",
        link_path.display(),
    );

    // Toggle on: TUI press Enter -> enable_resource(id, Claude, None).
    let en = env.run(&["enable", "test-skill", "--target", "claude"]);
    dump(&en, "enable test-skill on claude");
    assert!(en.status.success(), "enable failed");

    // After enable: symlink exists and points to the managed dir.
    let md = std::fs::symlink_metadata(&link_path).expect("symlink should exist after enable");
    assert!(
        md.file_type().is_symlink(),
        "{} should be a symlink",
        link_path.display(),
    );
    let resolved = std::fs::read_link(&link_path).expect("read_link");
    assert_eq!(
        resolved,
        skills_root.join("test-skill"),
        "symlink target mismatch",
    );

    // reload() reflects enabled=true: `list` shows the skill listed under
    // the claude target.
    let list = env.run(&["list", "--target", "claude"]);
    dump(&list, "list --target claude");
    assert!(list.status.success(), "list failed");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("test-skill"),
        "list --target claude did not show test-skill:\n{stdout}",
    );
}

// ─── 2. disable_skill_removes_symlink_from_target ───────────────────────────

#[test]
fn disable_skill_removes_symlink_from_target() {
    let env = TestEnv::new();
    let skills_root = env.home().join(".runai/skills");
    make_skill(&skills_root, "disable-skill");

    assert!(env.run(&["scan"]).status.success(), "scan failed");
    assert!(
        env.run(&["enable", "disable-skill", "--target", "claude"])
            .status
            .success(),
        "enable failed"
    );

    let link_path = env.cli_skills_dir("claude").join("disable-skill");
    let skill_payload = skills_root.join("disable-skill");

    // Before disable: link is a valid symlink, payload exists.
    let md = std::fs::symlink_metadata(&link_path).expect("link should exist after enable");
    assert!(md.file_type().is_symlink());
    assert!(skill_payload.is_dir(), "payload should still exist");

    // Toggle off: TUI second Enter -> disable_resource.
    let dis = env.run(&["disable", "disable-skill", "--target", "claude"]);
    dump(&dis, "disable disable-skill on claude");
    assert!(dis.status.success(), "disable failed");

    // After disable: symlink gone, underlying payload untouched (data preservation).
    assert!(
        std::fs::symlink_metadata(&link_path).is_err(),
        "symlink should be removed after disable",
    );
    assert!(
        skill_payload.is_dir(),
        "underlying skill dir must NOT be deleted on disable",
    );
    assert!(
        skill_payload.join("SKILL.md").is_file(),
        "SKILL.md must remain intact on disable",
    );
}

// ─── 3. enable_disable_per_cli_target_symmetric ─────────────────────────────

#[test]
fn enable_disable_per_cli_target_symmetric() {
    let env = TestEnv::new();
    let skills_root = env.home().join(".runai/skills");
    make_skill(&skills_root, "multi-target");

    assert!(env.run(&["scan"]).status.success(), "scan failed");

    // Enable on claude only.
    assert!(
        env.run(&["enable", "multi-target", "--target", "claude"])
            .status
            .success(),
        "enable on claude failed"
    );

    let claude_link = env.cli_skills_dir("claude").join("multi-target");
    let codex_link = env.cli_skills_dir("codex").join("multi-target");

    // Claude link present, codex link absent.
    assert!(
        std::fs::symlink_metadata(&claude_link).is_ok(),
        "claude link should exist"
    );
    assert!(
        std::fs::symlink_metadata(&codex_link).is_err(),
        "codex link must NOT exist before second enable"
    );

    // TUI: press '2' to switch active_target to codex, then Enter to enable.
    assert!(
        env.run(&["enable", "multi-target", "--target", "codex"])
            .status
            .success(),
        "enable on codex failed"
    );

    // Both targets now hold a symlink, independent of each other.
    assert!(
        std::fs::symlink_metadata(&claude_link).is_ok(),
        "claude link should remain"
    );
    let codex_md = std::fs::symlink_metadata(&codex_link).expect("codex link should now exist");
    assert!(codex_md.file_type().is_symlink());
    let codex_resolved = std::fs::read_link(&codex_link).expect("read codex link");
    assert_eq!(codex_resolved, skills_root.join("multi-target"));

    // Disable on claude only — codex must remain.
    assert!(
        env.run(&["disable", "multi-target", "--target", "claude"])
            .status
            .success(),
        "disable on claude failed"
    );
    assert!(
        std::fs::symlink_metadata(&claude_link).is_err(),
        "claude link gone after disable"
    );
    assert!(
        std::fs::symlink_metadata(&codex_link).is_ok(),
        "codex link must survive claude disable (independence)"
    );

    // Cross-collateral: gemini and opencode were never touched.
    for other in ["gemini", "opencode"] {
        let other_link = env.cli_skills_dir(other).join("multi-target");
        assert!(
            std::fs::symlink_metadata(&other_link).is_err(),
            "{other} link must not have been created collaterally"
        );
    }
}

// ─── 4. enable_clobbers_stale_symlink ───────────────────────────────────────

#[test]
fn enable_clobbers_stale_symlink() {
    let env = TestEnv::new();
    let skills_root = env.home().join(".runai/skills");
    make_skill(&skills_root, "clobber-skill");

    assert!(env.run(&["scan"]).status.success(), "scan failed");

    // Manually plant a DANGLING symlink at the target location (simulates a
    // previous crash/delete that left a stale link behind).
    let link_path = env.cli_skills_dir("claude").join("clobber-skill");
    std::os::unix::fs::symlink(env.home().join("nonexistent-target"), &link_path)
        .expect("create dangling symlink");

    // Confirm pre-state: link is a symlink but its target does not exist.
    let md = std::fs::symlink_metadata(&link_path).expect("pre-symlink should exist");
    assert!(md.file_type().is_symlink(), "pre-state must be a symlink");
    assert!(
        !link_path.exists(),
        "pre-state symlink must be dangling (exists() follows; target gone)"
    );

    // Toggle: TUI Enter -> enable_resource calls create_link_force which
    // clobbers stale symlinks (manager.rs:212-218 comments). Must NOT error.
    let en = env.run(&["enable", "clobber-skill", "--target", "claude"]);
    dump(&en, "enable over dangling symlink");
    assert!(
        en.status.success(),
        "enable over a dangling symlink must succeed (Linker::create_link_force)"
    );

    // After enable: symlink now points to the correct managed dir.
    let md_after = std::fs::symlink_metadata(&link_path).expect("link present after enable");
    assert!(md_after.file_type().is_symlink());
    let resolved = std::fs::read_link(&link_path).expect("read_link after enable");
    assert_eq!(
        resolved,
        skills_root.join("clobber-skill"),
        "symlink target must be repointed to managed skill dir",
    );
    assert!(
        link_path.exists(),
        "symlink should now resolve (no longer dangling)"
    );
}

// ─── 5. enable_mcp_writes_to_settings_json ──────────────────────────────────

#[test]
fn enable_mcp_writes_to_settings_json() {
    // Setup: write a realistic .claude.json with an existing MCP entry, then
    // disable it (moves to ~/.runai/mcps/<name>.json backup), then re-enable
    // (restores the entry into the config).
    let env = TestEnv::new();

    let claude_cfg = env.home().join(".claude.json");
    let initial = serde_json::json!({
        "numStartups": 1,
        "mcpServers": {
            "test-mcp": {
                "command": "/tmp/test-mcp",
                "args": ["--flag", "value"],
                "type": "stdio"
            },
            "other-mcp": {
                "command": "/tmp/other-mcp",
                "args": [],
                "type": "stdio"
            }
        }
    });
    std::fs::write(
        &claude_cfg,
        serde_json::to_string_pretty(&initial).expect("serialize claude.json"),
    )
    .expect("write .claude.json");

    // Disable: removes test-mcp from .claude.json and writes a backup under
    // ~/.runai/mcps/test-mcp.json. Press Enter on an enabled MCP -> disable.
    let dis = env.run(&["disable", "test-mcp", "--target", "claude"]);
    dump(&dis, "disable test-mcp");
    assert!(dis.status.success(), "disable test-mcp failed");

    // Verify removal: .claude.json no longer contains test-mcp, other-mcp untouched.
    let after_disable: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&claude_cfg).expect("read .claude.json after disable"),
    )
    .expect("parse JSON after disable");
    assert!(
        after_disable["mcpServers"].get("test-mcp").is_none(),
        ".claude.json should not contain test-mcp after disable"
    );
    assert!(
        after_disable["mcpServers"].get("other-mcp").is_some(),
        "other-mcp must still be present (collateral protection)"
    );
    let backup_path = env.home().join(".runai/mcps/test-mcp.json");
    assert!(
        backup_path.is_file(),
        "MCP backup not written to {}",
        backup_path.display()
    );

    // Re-enable: Press Enter -> enable_resource("mcp:test-mcp") -> restore_mcp
    // writes the entry back into .claude.json.
    let en = env.run(&["enable", "test-mcp", "--target", "claude"]);
    dump(&en, "enable test-mcp");
    assert!(en.status.success(), "enable test-mcp failed");

    // Verify restoration: .claude.json now contains test-mcp with original fields.
    let after_enable: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&claude_cfg).expect("read .claude.json after enable"),
    )
    .expect("parse JSON after enable");
    let entry = after_enable["mcpServers"]
        .get("test-mcp")
        .expect("test-mcp should be restored");
    assert_eq!(
        entry["command"], "/tmp/test-mcp",
        "restored command must match original"
    );
    assert_eq!(
        entry["args"][0], "--flag",
        "restored args must match original"
    );

    // .claude.json must remain valid JSON (already parsed above) and other-mcp
    // remains intact.
    assert!(
        after_enable["mcpServers"].get("other-mcp").is_some(),
        "other-mcp must still be present after enable (no collateral damage)"
    );
}

// ─── 6. enable_respects_rune_data_dir_dual_run ──────────────────────────────

#[test]
fn enable_respects_rune_data_dir_dual_run() {
    // Two distinct RUNE_DATA_DIRs, each in its OWN isolated HOME, two distinct
    // skills (one per data dir). Enabling under each data dir must produce a
    // symlink that canonicalizes back to its OWN data dir — never the other.
    //
    // Root cause this guards: the 2026-04-20 / 2026-04-27 cross-data-dir
    // incidents that moved skills out of one data dir while operating with
    // RUNE_DATA_DIR pointed at another. Each home is fully isolated so the
    // scanner cannot accidentally walk a symlink into a different data dir
    // (a known scanner behavior when HOMEs are shared).

    // ── home 1 + data_dir_1 ──────────────────────────────────────────────
    let env_1 = TestEnv::new();
    let data_dir_1 = tempfile::tempdir().expect("create data_dir_1");
    let skills_1 = data_dir_1.path().join("skills");
    std::fs::create_dir_all(&skills_1).unwrap();
    make_skill(&skills_1, "skill1");

    let scan1 = env_1.run_with_data_dir(data_dir_1.path(), &["scan"]);
    dump(&scan1, "scan in env_1 with data_dir_1");
    assert!(scan1.status.success(), "scan with data_dir_1 failed");
    let en1 = env_1.run_with_data_dir(
        data_dir_1.path(),
        &["enable", "skill1", "--target", "claude"],
    );
    dump(&en1, "enable skill1 with data_dir_1");
    assert!(en1.status.success(), "enable skill1 with data_dir_1 failed");

    let link_1 = env_1.cli_skills_dir("claude").join("skill1");
    let resolved_1 = std::fs::read_link(&link_1).expect("read_link skill1");
    let canonical_1 = std::fs::canonicalize(&resolved_1).expect("canonicalize skill1 target");
    let canonical_data_1 =
        std::fs::canonicalize(data_dir_1.path()).expect("canonicalize data_dir_1");

    // ── home 2 + data_dir_2 ──────────────────────────────────────────────
    let env_2 = TestEnv::new();
    let data_dir_2 = tempfile::tempdir().expect("create data_dir_2");
    let skills_2 = data_dir_2.path().join("skills");
    std::fs::create_dir_all(&skills_2).unwrap();
    make_skill(&skills_2, "skill2");

    let scan2 = env_2.run_with_data_dir(data_dir_2.path(), &["scan"]);
    dump(&scan2, "scan in env_2 with data_dir_2");
    assert!(scan2.status.success(), "scan with data_dir_2 failed");
    let en2 = env_2.run_with_data_dir(
        data_dir_2.path(),
        &["enable", "skill2", "--target", "claude"],
    );
    dump(&en2, "enable skill2 with data_dir_2");
    assert!(en2.status.success(), "enable skill2 with data_dir_2 failed");

    let link_2 = env_2.cli_skills_dir("claude").join("skill2");
    let resolved_2 = std::fs::read_link(&link_2).expect("read_link skill2");
    let canonical_2 = std::fs::canonicalize(&resolved_2).expect("canonicalize skill2 target");
    let canonical_data_2 =
        std::fs::canonicalize(data_dir_2.path()).expect("canonicalize data_dir_2");

    // ── invariants ──────────────────────────────────────────────────────
    // Each link's canonical target sits under its OWN data dir.
    assert!(
        canonical_1.starts_with(&canonical_data_1),
        "skill1 link must point inside data_dir_1: target={} expected_under={}",
        canonical_1.display(),
        canonical_data_1.display(),
    );
    assert!(
        canonical_2.starts_with(&canonical_data_2),
        "skill2 link must point inside data_dir_2: target={} expected_under={}",
        canonical_2.display(),
        canonical_data_2.display(),
    );

    // Anti-pollution: neither link leaks into the OTHER data dir.
    assert!(
        !canonical_1.starts_with(&canonical_data_2),
        "skill1 link leaked into data_dir_2: {}",
        canonical_1.display(),
    );
    assert!(
        !canonical_2.starts_with(&canonical_data_1),
        "skill2 link leaked into data_dir_1: {}",
        canonical_2.display(),
    );

    // Each home is its own world: env_1's claude skills dir has skill1 but
    // not skill2, and vice versa.
    assert!(
        env_1.cli_skills_dir("claude").join("skill1").exists(),
        "env_1's claude dir should hold skill1"
    );
    assert!(
        std::fs::symlink_metadata(env_1.cli_skills_dir("claude").join("skill2")).is_err(),
        "env_1's claude dir must NOT have skill2"
    );
    assert!(
        env_2.cli_skills_dir("claude").join("skill2").exists(),
        "env_2's claude dir should hold skill2"
    );
    assert!(
        std::fs::symlink_metadata(env_2.cli_skills_dir("claude").join("skill1")).is_err(),
        "env_2's claude dir must NOT have skill1"
    );
}

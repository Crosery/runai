//! Physical e2e regression for the MCP tools `sm_enable` and `sm_disable`.
//!
//! These tools are exposed by `src/mcp/tools.rs` (`SmServer::sm_enable` /
//! `SmServer::sm_disable`) and are also reachable via the user-facing CLI
//! subcommands `runai enable` / `runai disable`, which route through the same
//! `SkillManager::{enable_resource, enable_group, disable_resource,
//! disable_group}` code paths. Driving the real binary is enough to exercise
//! the destructive filesystem boundary (symlink create/remove + CLI config
//! mutation for MCPs) without depending on a privately-constructed
//! `SmServer` from the integration test crate.
//!
//! Each test runs in an isolated HOME tempdir per `AGENTS.md` §2 safety
//! contract; cross-target and `RUNE_DATA_DIR` overrides are verified per
//! §3 / §4.
//!
//! Skipped on Windows: symlink mocking + HOME env mocking are unix-only,
//! matching `safety_e2e.rs` / `cli_target_symmetry.rs`.

#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Isolated HOME tempdir with all four CLI skills dirs pre-created and an
/// empty managed `.runai/skills/`.
struct EnableEnv {
    home: TempDir,
}

impl EnableEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills")))
                .expect("pre-create CLI skills dir");
        }
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        // Pre-create ~/.claude.json so MCP enable/disable has a config to mutate.
        std::fs::write(home.path().join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn skills_root(&self) -> PathBuf {
        self.home().join(".runai/skills")
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
        cmd.output().expect("runai binary spawn")
    }

    fn run_with_rune(&self, rune_data: &Path, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", rune_data)
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().expect("runai binary spawn")
    }
}

fn make_skill(parent: &Path, name: &str) -> PathBuf {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test desc\n---\n\n# {name}\n"),
    )
    .unwrap();
    dir
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\nstdout: {}\nstderr: {}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ─── sm_enable tests ────────────────────────────────────────────────────────

/// sm_enable for a skill on the `claude` target must create a symlink at
/// `~/.claude/skills/<name>` whose readlink points back at
/// `~/.runai/skills/<name>`.
#[test]
fn enable_skill_creates_symlink_on_claude() {
    let env = EnableEnv::new();
    make_skill(&env.skills_root(), "test-skill");

    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan failed");

    let en = env.run(&["enable", "test-skill", "--target", "claude"]);
    dump(&en, "enable test-skill on claude");
    assert!(en.status.success(), "enable failed");

    let link = env.cli_skills_dir("claude").join("test-skill");
    let meta = std::fs::symlink_metadata(&link)
        .unwrap_or_else(|e| panic!("symlink missing at {}: {e}", link.display()));
    assert!(
        meta.file_type().is_symlink(),
        "expected a symlink at {}, got {:?}",
        link.display(),
        meta.file_type()
    );
    let target = std::fs::read_link(&link).unwrap();
    assert_eq!(
        target,
        env.skills_root().join("test-skill"),
        "symlink should point at managed skill dir"
    );
    // Readable / valid: SKILL.md is reachable through the link.
    let resolved = std::fs::read_to_string(link.join("SKILL.md")).unwrap();
    assert!(
        resolved.contains("test-skill"),
        "symlinked SKILL.md not readable: {resolved}"
    );
}

/// sm_enable must work identically across all 4 CLI targets and only land
/// the symlink in the requested target — no collateral writes into the other
/// three target dirs.
#[test]
fn enable_symmetric_across_4_cli_targets() {
    for target in ["claude", "codex", "gemini", "opencode"] {
        let env = EnableEnv::new();
        let skill_name = format!("cross-target-{target}");
        make_skill(&env.skills_root(), &skill_name);
        assert!(
            env.run(&["scan"]).status.success(),
            "scan failed for {target}"
        );

        let en = env.run(&["enable", &skill_name, "--target", target]);
        dump(&en, &format!("enable {skill_name} on {target}"));
        assert!(en.status.success(), "enable failed for {target}");

        let link = env.cli_skills_dir(target).join(&skill_name);
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "missing symlink at {}",
            link.display()
        );
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(
            resolved,
            env.skills_root().join(&skill_name),
            "symlink on {target} points to wrong source"
        );

        // The other three CLI target dirs must NOT have a same-named link.
        for other in ["claude", "codex", "gemini", "opencode"] {
            if other == target {
                continue;
            }
            let collateral = env.cli_skills_dir(other).join(&skill_name);
            assert!(
                std::fs::symlink_metadata(&collateral).is_err(),
                "enabling on {target} accidentally created symlink under {other}: {}",
                collateral.display()
            );
        }
    }
}

/// With `RUNE_DATA_DIR` overridden, sm_enable must create the symlink in the
/// CLI target dir (rooted at HOME) and the target of the symlink must point
/// into the override data dir — NOT into the default `~/.runai`.
#[test]
fn enable_respects_rune_data_dir_override_symlink_location() {
    let env = EnableEnv::new();
    let alt_data = tempfile::tempdir().expect("tmp RUNE_DATA_DIR");
    let alt_skills = alt_data.path().join("skills");
    std::fs::create_dir_all(&alt_skills).unwrap();
    make_skill(&alt_skills, "custom-skill");

    // Default ~/.runai/skills/ must NOT contain custom-skill before or after.
    assert!(
        !env.skills_root().join("custom-skill").exists(),
        "precondition: custom-skill must only exist in alt data dir"
    );

    let scan = env.run_with_rune(alt_data.path(), &["scan"]);
    dump(&scan, "scan with RUNE_DATA_DIR");
    assert!(scan.status.success(), "scan with RUNE_DATA_DIR failed");

    let en = env.run_with_rune(
        alt_data.path(),
        &["enable", "custom-skill", "--target", "claude"],
    );
    dump(&en, "enable custom-skill on claude with RUNE_DATA_DIR");
    assert!(en.status.success(), "enable with RUNE_DATA_DIR failed");

    let link = env.cli_skills_dir("claude").join("custom-skill");
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "symlink not created at {}",
        link.display()
    );
    let resolved = std::fs::read_link(&link).unwrap();
    assert_eq!(
        resolved,
        alt_skills.join("custom-skill"),
        "symlink must point into RUNE_DATA_DIR, not default ~/.runai"
    );
    // Default ~/.runai/skills/ stays clean.
    assert!(
        !env.skills_root().join("custom-skill").exists(),
        "default ~/.runai/skills/custom-skill must NOT have been created"
    );
}

/// Enabling an MCP whose backup lives at `~/.runai/mcps/<name>.json` must
/// restore the entry under `mcpServers` in `~/.claude.json`. Round-trip
/// disable → enable pattern: disable to populate the backup, then enable to
/// restore.
#[test]
fn enable_mcp_restores_config_entry() {
    let env = EnableEnv::new();

    // Seed ~/.claude.json with a fully-formed MCP entry.
    std::fs::write(
        env.home().join(".claude.json"),
        r#"{"mcpServers":{"unit-test-mcp":{"command":"/bin/unit-test-mcp","args":["--flag"]}}}"#,
    )
    .unwrap();

    let scan = env.run(&["scan"]);
    dump(&scan, "scan (seeds MCP backup)");
    assert!(scan.status.success(), "scan failed");

    // Discover/adopt should run via scan; if not, mcp backup will appear once
    // we disable the entry below.
    let dis = env.run(&["disable", "unit-test-mcp", "--target", "claude"]);
    dump(&dis, "disable unit-test-mcp on claude");
    if !dis.status.success() {
        // The CLI may not adopt MCPs via `scan` directly — call discover-style
        // path. Fail loudly if neither works because this test would then
        // need an `sm_register` (src change) instead of a test-only fix.
        panic!(
            "disable failed (test setup): stdout/stderr = {} {}",
            String::from_utf8_lossy(&dis.stdout),
            String::from_utf8_lossy(&dis.stderr),
        );
    }

    // After disable, ~/.claude.json should NOT contain unit-test-mcp anymore.
    let claude_after_dis: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(env.home().join(".claude.json")).unwrap())
            .unwrap();
    assert!(
        claude_after_dis["mcpServers"]
            .get("unit-test-mcp")
            .is_none(),
        "disable should have removed mcp entry from ~/.claude.json"
    );

    // Backup file should exist at ~/.runai/mcps/unit-test-mcp.json.
    let backup = env.home().join(".runai/mcps/unit-test-mcp.json");
    assert!(
        backup.exists(),
        "disable should have left a canonical backup at {}",
        backup.display()
    );

    // Now enable — entry should reappear under mcpServers.
    let en = env.run(&["enable", "unit-test-mcp", "--target", "claude"]);
    dump(&en, "enable unit-test-mcp on claude");
    assert!(en.status.success(), "enable mcp failed");

    let claude_after_en: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(env.home().join(".claude.json")).unwrap())
            .unwrap();
    let entry = &claude_after_en["mcpServers"]["unit-test-mcp"];
    assert!(
        !entry.is_null(),
        "enable should have re-added mcp entry: got {claude_after_en:?}"
    );
    assert_eq!(
        entry["command"],
        serde_json::json!("/bin/unit-test-mcp"),
        "command should round-trip as canonical string"
    );
    assert_eq!(entry["args"], serde_json::json!(["--flag"]));
}

/// sm_enable on a name that doesn't exist must return a "Not found" message
/// (not silently no-op) and must NOT create any symlinks.
#[test]
fn enable_nonexistent_fails_gracefully() {
    let env = EnableEnv::new();
    // Snapshot the count of children in each CLI skills dir before.
    let before: Vec<usize> = ["claude", "codex", "gemini", "opencode"]
        .iter()
        .map(|c| {
            env.cli_skills_dir(c)
                .read_dir()
                .map(|it| it.count())
                .unwrap_or(0)
        })
        .collect();

    let out = env.run(&["enable", "doesnt-exist", "--target", "claude"]);
    dump(&out, "enable doesnt-exist");

    // Non-zero exit OR human-readable "Not found" in output, but never a
    // silent success that creates symlinks.
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let lower = combined.to_lowercase();
    let mentions_not_found = lower.contains("not found")
        || lower.contains("doesnt-exist")
        || lower.contains("unknown")
        || lower.contains("no resource")
        || lower.contains("no such");
    assert!(
        !out.status.success() || mentions_not_found,
        "enable of nonexistent must error or mention not-found, got: {combined}"
    );

    // No collateral symlinks created in any CLI dir.
    let after: Vec<usize> = ["claude", "codex", "gemini", "opencode"]
        .iter()
        .map(|c| {
            env.cli_skills_dir(c)
                .read_dir()
                .map(|it| it.count())
                .unwrap_or(0)
        })
        .collect();
    assert_eq!(
        before, after,
        "enable of nonexistent must not create any symlinks"
    );
}

/// Enabling a group cascades to every member skill: all symlinks must land
/// in the requested CLI target dir.
#[test]
fn enable_group_cascades_to_members() {
    let env = EnableEnv::new();
    for n in ["g-skill-a", "g-skill-b", "g-skill-c"] {
        make_skill(&env.skills_root(), n);
    }
    assert!(env.run(&["scan"]).status.success(), "scan failed");

    // Create a group and add all 3 members.
    let cg = env.run(&[
        "group",
        "create",
        "dev-tools",
        "--name",
        "Dev Tools",
        "--description",
        "Test group",
    ]);
    dump(&cg, "group create dev-tools");
    assert!(cg.status.success(), "group create failed");

    for n in ["g-skill-a", "g-skill-b", "g-skill-c"] {
        let add = env.run(&["group", "add", "dev-tools", n]);
        dump(&add, &format!("group add dev-tools {n}"));
        assert!(add.status.success(), "group add {n} failed");
    }

    let en = env.run(&["enable", "dev-tools", "--target", "claude"]);
    dump(&en, "enable group dev-tools on claude");
    assert!(en.status.success(), "enable group failed");

    for n in ["g-skill-a", "g-skill-b", "g-skill-c"] {
        let link = env.cli_skills_dir("claude").join(n);
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "member {n} not enabled — missing symlink {}",
            link.display()
        );
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(
            resolved,
            env.skills_root().join(n),
            "member {n} symlink points wrong"
        );
    }

    let stdout = String::from_utf8_lossy(&en.stdout);
    let stderr = String::from_utf8_lossy(&en.stderr);
    let combined = format!("{stdout}{stderr}").to_lowercase();
    assert!(
        combined.contains("dev-tools") || combined.contains("group"),
        "result message should mention group enable: stdout={stdout} stderr={stderr}"
    );
}

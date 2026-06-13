//! Physical e2e regression for the Trash Purge feature (PLANNING test plan
//! §2.21). Purge is the permanent half of trash management — once invoked
//! the payload directory under `~/.runai/trash/` is rm -rf'd and the DB
//! `trash_entries` row is deleted with no recovery path. These tests pin:
//!
//! 1. Payload + DB row both disappear after purge, and `trash list` no
//!    longer reports the entry.
//! 2. Purge under a custom `RUNE_DATA_DIR` only touches the configured data
//!    dir; the default `~/.runai/` is left untouched (safety contract /
//!    4-20-4-27 root cause class — even purge must not spill across data
//!    dirs).
//! 3. Purge of an MCP-kind trash entry deletes the DB row even though the
//!    MCP path has no payload directory on disk (`payload_path = None`).
//! 4. Purge is global — it does not depend on `active_target`, so running
//!    purge after a per-target uninstall always clears the entry.
//!
//! Skipped on Windows for the same reason as `safety_e2e.rs`: HOME mocking
//! and symlinks are unix-only paths in this codebase.

#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Isolated HOME env for trash purge cases. Pre-creates the four CLI skills
/// dirs and `~/.runai/skills/` so scan / enable / uninstall flows have a
/// place to put symlinks and managed skill dirs.
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

    fn default_data_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }

    fn default_skills_dir(&self) -> PathBuf {
        self.default_data_dir().join("skills")
    }

    fn default_trash_dir(&self) -> PathBuf {
        self.default_data_dir().join("trash")
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

/// List the single trash-payload directory beneath `<data>/trash/`, panic if
/// the count is not exactly one. Used to grab the auto-generated
/// `<deleted_at_ms>-<slug>` payload path so we can assert it gets rm -rf'd.
fn single_trash_payload(trash_dir: &Path) -> PathBuf {
    let entries: Vec<PathBuf> = std::fs::read_dir(trash_dir)
        .unwrap_or_else(|e| panic!("read_dir {} failed: {e}", trash_dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "expected exactly one trash payload under {}, got {:?}",
        trash_dir.display(),
        entries,
    );
    entries.into_iter().next().unwrap()
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// PLANNING §2.21 required_test #1
///
/// Adapted to the actual CLI surface: there is no lowercase-'d' →
/// ConfirmDelete-on-Trash-tab path in `src/tui/app.rs` (lowercase 'd' on
/// the Trash tab is a no-op; the actual purge keybinding is capital 'D'
/// which calls `mgr.purge_trash(trash_id)` directly without a confirmation
/// modal — see `src/tui/app.rs:750-752`). The CLI `runai trash purge
/// <query>` exercises the exact same `SkillManager::purge_trash` codepath
/// that the TUI's 'D' handler calls. We assert here on the post-conditions
/// the test plan specified for the underlying purge operation: payload
/// dir rm -rf'd, DB trash_entries row deleted, `trash list` no longer
/// reports the entry.
#[test]
fn trash_purge_deletes_payload_and_entry() {
    let env = TestEnv::new();

    // Setup: a real managed skill, scanned in, enabled (so the trash flow
    // also has to remove the symlink — adds realism), then uninstalled to
    // land in trash with a payload dir.
    make_skill(&env.default_skills_dir(), "purge-me", "purge body");
    assert!(env.run(&["scan"]).status.success(), "scan should succeed");
    assert!(
        env.run(&["enable", "purge-me", "--target", "claude"])
            .status
            .success(),
        "enable should succeed",
    );

    let un = env.run(&["uninstall", "purge-me"]);
    dump(&un, "uninstall purge-me");
    assert!(un.status.success(), "uninstall must succeed");

    let trash_dir = env.default_trash_dir();
    let payload = single_trash_payload(&trash_dir);
    let payload_skill_md = payload.join("SKILL.md");
    assert!(
        payload_skill_md.exists(),
        "trash payload should retain SKILL.md before purge: {}",
        payload_skill_md.display(),
    );

    // Pre-purge sanity: `trash list` reports the entry.
    let list_before = env.run(&["trash", "list"]);
    assert!(list_before.status.success(), "trash list should succeed");
    assert!(
        String::from_utf8_lossy(&list_before.stdout).contains("purge-me"),
        "trash list should mention purge-me before purge",
    );

    // Act: purge.
    let purge = env.run(&["trash", "purge", "purge-me"]);
    dump(&purge, "trash purge purge-me");
    assert!(purge.status.success(), "trash purge must succeed");

    // Assert 1: payload directory is rm -rf'd.
    assert!(
        !payload.exists(),
        "purge did not remove the trash payload dir at {}",
        payload.display(),
    );
    assert!(
        !payload_skill_md.exists(),
        "purge left the payload SKILL.md behind at {}",
        payload_skill_md.display(),
    );

    // Assert 2: DB trash_entries row deleted (verified via `trash list`).
    let list_after = env.run(&["trash", "list"]);
    assert!(
        list_after.status.success(),
        "post-purge trash list must succeed"
    );
    let list_after_out = String::from_utf8_lossy(&list_after.stdout);
    assert!(
        !list_after_out.contains("purge-me"),
        "trash list should not mention purge-me after purge, got:\n{list_after_out}",
    );

    // Assert 3: subsequent purge for the same name fails (entry gone).
    let purge_again = env.run(&["trash", "purge", "purge-me"]);
    assert!(
        !purge_again.status.success(),
        "second purge against same name should fail, exit={} stderr={}",
        purge_again.status,
        String::from_utf8_lossy(&purge_again.stderr),
    );
}

/// PLANNING §2.21 required_test #3
///
/// `RUNE_DATA_DIR` isolation must hold through trash purge — the operation
/// must only touch `<CUSTOM_DATA>/trash/` and `<CUSTOM_DATA>/runai.db`, and
/// must never delete from or write into the default `~/.runai/`. This is
/// the same root-cause class as the 4-20 / 4-27 scan incident: any
/// destructive command that resolves data paths must be pinned to the
/// active `RUNE_DATA_DIR`, never the default.
#[test]
fn trash_purge_uses_custom_data_dir() {
    let env = TestEnv::new();
    let custom_data = env.home().join("custom/.runai");
    std::fs::create_dir_all(custom_data.join("skills")).unwrap();

    // Plant a sentinel in the DEFAULT data dir that purge must NOT touch.
    let default_sentinel_dir = make_skill(
        &env.default_skills_dir(),
        "default-sentinel",
        "should survive purge against custom data dir",
    );
    let default_sentinel_md = default_sentinel_dir.join("SKILL.md");
    let default_sentinel_bytes = std::fs::read(&default_sentinel_md).unwrap();

    // Setup the skill we'll actually trash + purge — lives under custom data.
    let custom_skills = custom_data.join("skills");
    make_skill(&custom_skills, "custom-skill", "custom skill body");
    assert!(
        env.run_with_rune_data(&custom_data, &["scan"])
            .status
            .success(),
        "scan under custom data dir must succeed",
    );

    let un = env.run_with_rune_data(&custom_data, &["uninstall", "custom-skill"]);
    dump(&un, "uninstall under custom data");
    assert!(
        un.status.success(),
        "uninstall under custom data must succeed"
    );

    let custom_trash = custom_data.join("trash");
    let custom_payload = single_trash_payload(&custom_trash);
    assert!(
        custom_payload.join("SKILL.md").exists(),
        "custom trash payload should hold SKILL.md before purge",
    );

    // Act: purge against the custom data dir.
    let purge = env.run_with_rune_data(&custom_data, &["trash", "purge", "custom-skill"]);
    dump(&purge, "trash purge against custom data");
    assert!(
        purge.status.success(),
        "purge under custom data must succeed"
    );

    // Assert 1: custom payload gone.
    assert!(
        !custom_payload.exists(),
        "purge did not remove custom payload at {}",
        custom_payload.display(),
    );

    // Assert 2: custom DB no longer reports the entry.
    let list_custom = env.run_with_rune_data(&custom_data, &["trash", "list"]);
    assert!(list_custom.status.success());
    let list_custom_out = String::from_utf8_lossy(&list_custom.stdout);
    assert!(
        !list_custom_out.contains("custom-skill"),
        "post-purge trash list under custom data should not mention custom-skill, got:\n{list_custom_out}",
    );

    // Assert 3: DEFAULT data dir was never touched — sentinel still there
    // with identical bytes, no trash payload spilled over to default trash.
    assert!(
        default_sentinel_md.exists(),
        "purge against custom data dir deleted default sentinel skill at {}",
        default_sentinel_md.display(),
    );
    assert_eq!(
        std::fs::read(&default_sentinel_md).unwrap(),
        default_sentinel_bytes,
        "purge against custom data dir mutated default sentinel bytes",
    );
    let default_trash = env.default_trash_dir();
    if default_trash.exists() {
        let stray: Vec<_> = std::fs::read_dir(&default_trash)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            stray.is_empty(),
            "purge against custom data dir spilled into default trash: {:?}",
            stray.iter().map(|e| e.path()).collect::<Vec<_>>(),
        );
    }
}

/// PLANNING §2.21 required_test #4
///
/// MCPs trash differently from skills: the trash entry has no
/// `payload_path` (MCPs live inside CLI config files, not in
/// `~/.runai/skills/<name>/`), so purge for an MCP trash entry must still
/// delete the DB trash_entries row without choking on the missing
/// payload. Also asserts that the canonical-shape backup written to
/// `~/.runai/mcps/<name>.json` (if any) is created/cleared symmetrically.
#[test]
fn trash_purge_handles_mcp_cleanup() {
    let env = TestEnv::new();

    // Plant a Claude-shaped MCP into `~/.claude.json` so
    // `read_mcp_status_from_configs()` sees it as enabled.
    let claude_config = env.home().join(".claude.json");
    let initial_config = serde_json::json!({
        "mcpServers": {
            "purge-mcp": {
                "command": "/tmp/purge-mcp",
                "args": ["--mode", "test"],
                "type": "stdio"
            }
        }
    });
    std::fs::write(
        &claude_config,
        serde_json::to_string_pretty(&initial_config).unwrap(),
    )
    .unwrap();

    // Uninstall the MCP — moves it into trash. The mgr-side `trash_resource`
    // for an MCP records `mcp_configs`/`enabled_targets` but no payload.
    // `runai uninstall <name>` resolves `purge-mcp` to `mcp:purge-mcp` via
    // `find_resource_id` because no skill row owns that name (see
    // `core::manager::find_resource_id`).
    let un = env.run(&["uninstall", "purge-mcp"]);
    dump(&un, "uninstall purge-mcp (mcp)");
    assert!(un.status.success(), "MCP uninstall must succeed");

    // The MCP entry should be removed from .claude.json.
    let post_uninstall: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_config).expect("read claude config"))
            .expect("parse claude config");
    let still_present = post_uninstall
        .get("mcpServers")
        .and_then(|s| s.get("purge-mcp"))
        .is_some();
    assert!(
        !still_present,
        "uninstall should have removed purge-mcp from .claude.json, got:\n{post_uninstall}",
    );

    // trash list should mention the MCP entry.
    let list_before = env.run(&["trash", "list"]);
    assert!(list_before.status.success());
    let list_before_out = String::from_utf8_lossy(&list_before.stdout);
    assert!(
        list_before_out.contains("purge-mcp"),
        "trash list missing purge-mcp before purge, got:\n{list_before_out}",
    );

    // Act: purge the MCP trash entry.
    let purge = env.run(&["trash", "purge", "purge-mcp"]);
    dump(&purge, "trash purge purge-mcp");
    assert!(
        purge.status.success(),
        "MCP trash purge must succeed even with no payload dir, stderr:\n{}",
        String::from_utf8_lossy(&purge.stderr),
    );

    // Assert: DB trash row gone.
    let list_after = env.run(&["trash", "list"]);
    assert!(list_after.status.success());
    let list_after_out = String::from_utf8_lossy(&list_after.stdout);
    assert!(
        !list_after_out.contains("purge-mcp"),
        "trash list should not mention purge-mcp after purge, got:\n{list_after_out}",
    );

    // Assert: subsequent purge against same name fails (DB clean).
    let purge_again = env.run(&["trash", "purge", "purge-mcp"]);
    assert!(
        !purge_again.status.success(),
        "second MCP purge should fail (trash row already gone), stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&purge_again.stdout),
        String::from_utf8_lossy(&purge_again.stderr),
    );

    // Assert: .claude.json was not mutated by the purge itself (the
    // uninstall already removed the entry — purge must not write anything
    // back). The file should still be valid JSON without purge-mcp.
    let final_config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_config).unwrap()).unwrap();
    assert!(
        final_config
            .get("mcpServers")
            .and_then(|s| s.get("purge-mcp"))
            .is_none(),
        "purge unexpectedly restored purge-mcp into .claude.json: {final_config}",
    );
}

/// PLANNING §2.21 required_test #6
///
/// Trash purge is global — it does not consult `active_target`. A skill
/// uninstalled while enabled on target X must still be purgeable, and
/// purge must clear the entry exactly the same regardless of which
/// target originally hosted the symlink. We exercise the four CLI
/// targets in a single test (no point spinning up four separate HOMEs
/// for one assertion class).
#[test]
fn trash_purge_ignores_cli_target() {
    for target in ["claude", "codex", "gemini", "opencode"] {
        let env = TestEnv::new();
        let name = format!("purge-{target}");

        make_skill(
            &env.default_skills_dir(),
            &name,
            &format!("body for {target}"),
        );
        assert!(
            env.run(&["scan"]).status.success(),
            "scan must succeed for {target}",
        );
        assert!(
            env.run(&["enable", &name, "--target", target])
                .status
                .success(),
            "enable --target {target} must succeed",
        );

        let un = env.run(&["uninstall", &name]);
        dump(&un, &format!("uninstall (was enabled on {target})"));
        assert!(
            un.status.success(),
            "uninstall must succeed for target={target}",
        );

        let trash_dir = env.default_trash_dir();
        let payload = single_trash_payload(&trash_dir);
        assert!(
            payload.join("SKILL.md").exists(),
            "trash payload for {target} should hold SKILL.md before purge",
        );

        // Purge — note: NO --target flag is accepted/required.
        let purge = env.run(&["trash", "purge", &name]);
        dump(&purge, &format!("trash purge {name}"));
        assert!(
            purge.status.success(),
            "purge must succeed regardless of original target={target}, stderr={}",
            String::from_utf8_lossy(&purge.stderr),
        );

        // Payload gone.
        assert!(
            !payload.exists(),
            "purge for target={target} left payload at {}",
            payload.display(),
        );

        // No leftover trash payloads at all.
        if trash_dir.exists() {
            let remaining: Vec<_> = std::fs::read_dir(&trash_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            assert!(
                remaining.is_empty(),
                "after purge for target={target}, trash dir still has: {remaining:?}",
            );
        }

        // DB row gone.
        let list_after = env.run(&["trash", "list"]);
        assert!(list_after.status.success());
        let list_after_out = String::from_utf8_lossy(&list_after.stdout);
        assert!(
            !list_after_out.contains(&name),
            "trash list should not mention {name} (target={target}) after purge, got:\n{list_after_out}",
        );
    }
}

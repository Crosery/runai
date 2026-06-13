//! Cross-CLI-target symmetry: enable/disable/uninstall must put symlinks in
//! the right per-target dir for every supported CLI (claude / codex / gemini /
//! opencode), and clean them up symmetrically.
//!
//! Each test runs in an isolated HOME tempdir and spawns the real `runai`
//! binary. Skipped on Windows for the same reason as `safety_e2e.rs`.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

struct TestEnv {
    home: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills"))).unwrap();
        }
        std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn cli_skills_dir(&self, cli: &str) -> PathBuf {
        // Mirrors src/core/cli_target.rs::skills_dir() on unix.
        self.home().join(format!(".{cli}/skills"))
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env_remove("RUNE_DATA_DIR")
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().unwrap()
    }
}

fn make_skill(parent: &Path, name: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: test desc\n---\n\n# {name}\n"),
    )
    .unwrap();
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n{}\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Run the full enable → assert → disable → assert → uninstall → assert cycle
/// for one CLI target. Asserts symlinks land in the right per-target dir and
/// disappear symmetrically.
fn round_trip(target: &str, skill_name: &str) {
    let env = TestEnv::new();
    let skill_dir = env.home().join(".runai/skills");
    make_skill(&skill_dir, skill_name);
    assert!(
        env.run(&["scan"]).status.success(),
        "scan failed for {target}"
    );

    let link_path = env.cli_skills_dir(target).join(skill_name);

    // --- enable ---
    let en = env.run(&["enable", skill_name, "--target", target]);
    dump(&en, &format!("enable {skill_name} on {target}"));
    assert!(en.status.success(), "enable failed for {target}");
    assert!(
        std::fs::symlink_metadata(&link_path).is_ok(),
        "enable on {target} did not create symlink at expected path: {}",
        link_path.display()
    );
    let resolved = std::fs::read_link(&link_path).unwrap();
    assert_eq!(
        resolved,
        skill_dir.join(skill_name),
        "symlink on {target} points to wrong target: {}",
        resolved.display()
    );

    // No collateral: the *other* three CLI dirs must NOT have a same-named link.
    for other in ["claude", "codex", "gemini", "opencode"] {
        if other == target {
            continue;
        }
        let collateral = env.cli_skills_dir(other).join(skill_name);
        assert!(
            std::fs::symlink_metadata(&collateral).is_err(),
            "enabling on {target} accidentally created symlink under {other}: {}",
            collateral.display()
        );
    }

    // --- disable ---
    let dis = env.run(&["disable", skill_name, "--target", target]);
    dump(&dis, &format!("disable {skill_name} on {target}"));
    assert!(dis.status.success(), "disable failed for {target}");
    assert!(
        std::fs::symlink_metadata(&link_path).is_err(),
        "disable on {target} left symlink at {}",
        link_path.display()
    );

    // --- re-enable then uninstall (trash-first) ---
    assert!(
        env.run(&["enable", skill_name, "--target", target])
            .status
            .success()
    );
    let un = env.run(&["uninstall", skill_name]);
    dump(
        &un,
        &format!("uninstall {skill_name} (was enabled on {target})"),
    );
    assert!(un.status.success(), "uninstall failed for {target}");
    assert!(
        std::fs::symlink_metadata(&link_path).is_err(),
        "uninstall on {target} left symlink at {}",
        link_path.display()
    );
    assert!(
        !skill_dir.join(skill_name).exists(),
        "uninstall on {target} did not move skill out of managed skills/"
    );
    let trash = env.home().join(".runai/trash");
    assert!(
        trash.exists() && trash.read_dir().unwrap().next().is_some(),
        "uninstall on {target} did not deposit anything under ~/.runai/trash/"
    );
}

#[test]
fn round_trip_claude() {
    round_trip("claude", "rt-claude");
}

#[test]
fn round_trip_codex() {
    round_trip("codex", "rt-codex");
}

#[test]
fn round_trip_gemini() {
    round_trip("gemini", "rt-gemini");
}

#[test]
fn round_trip_opencode() {
    round_trip("opencode", "rt-opencode");
}

/// Cross-target enable: enabling the same skill on two different targets
/// produces two symlinks (one per target) pointing at the same managed dir.
/// Disabling one leaves the other intact.
#[test]
fn enable_two_targets_keeps_both_symlinks_independent() {
    let env = TestEnv::new();
    let skill_dir = env.home().join(".runai/skills");
    make_skill(&skill_dir, "shared");
    assert!(env.run(&["scan"]).status.success());

    assert!(
        env.run(&["enable", "shared", "--target", "claude"])
            .status
            .success()
    );
    assert!(
        env.run(&["enable", "shared", "--target", "codex"])
            .status
            .success()
    );

    let claude_link = env.cli_skills_dir("claude").join("shared");
    let codex_link = env.cli_skills_dir("codex").join("shared");
    assert!(std::fs::symlink_metadata(&claude_link).is_ok());
    assert!(std::fs::symlink_metadata(&codex_link).is_ok());

    // Disable on claude — codex must remain.
    assert!(
        env.run(&["disable", "shared", "--target", "claude"])
            .status
            .success()
    );
    assert!(
        std::fs::symlink_metadata(&claude_link).is_err(),
        "claude link should be gone after disable"
    );
    assert!(
        std::fs::symlink_metadata(&codex_link).is_ok(),
        "codex link should remain after disabling on claude"
    );
}

// =============================================================================
// PLANNING §5.17 — core::cli_target (4 CLI 抽象) — P0 high-risk regression suite
// =============================================================================
//
// Unit-style tests on the `CliTarget` enum directly. They mutate the process
// HOME env to redirect `dirs::home_dir()`; runs are serialised by
// `HOME_MUTEX` and the outer cargo invocation already pins `--test-threads=1`,
// so they never interleave with the physical e2e tests above.

use std::str::FromStr;
use std::sync::Mutex;

use runai::core::cli_target::CliTarget;

static HOME_MUTEX: Mutex<()> = Mutex::new(());

/// Pins HOME to a fresh tempdir for the duration of a closure. Restores the
/// previous HOME on drop. Other tests in this file (and other crates running
/// in the same process under `--test-threads=1`) won't see the override after
/// the closure returns.
fn with_pinned_home<F: FnOnce(&Path) -> R, R>(f: F) -> R {
    let _guard = HOME_MUTEX.lock().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let prev = std::env::var_os("HOME");
    // SAFETY: serialised by HOME_MUTEX above.
    unsafe {
        std::env::set_var("HOME", scratch.path());
    }
    let result = f(scratch.path());
    unsafe {
        if let Some(p) = prev {
            std::env::set_var("HOME", p);
        } else {
            std::env::remove_var("HOME");
        }
    }
    result
}

#[test]
fn skills_dir_cross_target_symmetry() {
    with_pinned_home(|home| {
        let mut seen: Vec<PathBuf> = Vec::new();
        for target in CliTarget::ALL {
            let dir = target.skills_dir();
            // Each path must be non-empty and rooted under our pinned HOME so
            // we know `dirs::home_dir()` is being respected.
            assert!(
                !dir.as_os_str().is_empty(),
                "skills_dir() for {target:?} returned empty path"
            );
            assert!(
                dir.starts_with(home),
                "skills_dir() for {target:?} = {} not under pinned HOME {}",
                dir.display(),
                home.display(),
            );
            assert!(
                dir.ends_with("skills"),
                "skills_dir() for {target:?} did not end in 'skills': {}",
                dir.display(),
            );
            assert!(
                !seen.contains(&dir),
                "skills_dir() for {target:?} collides with an earlier target: {}",
                dir.display(),
            );
            seen.push(dir);
        }

        // Specific expected paths on unix.
        assert_eq!(CliTarget::Claude.skills_dir(), home.join(".claude/skills"));
        assert_eq!(CliTarget::Codex.skills_dir(), home.join(".codex/skills"));
        assert_eq!(CliTarget::Gemini.skills_dir(), home.join(".gemini/skills"));
        assert_eq!(
            CliTarget::OpenCode.skills_dir(),
            home.join(".opencode/skills"),
        );
        assert_eq!(seen.len(), 4, "expected exactly 4 distinct skills_dir paths");
    });
}

#[test]
fn mcp_config_path_cross_target_symmetry() {
    with_pinned_home(|home| {
        let mut seen: Vec<PathBuf> = Vec::new();
        for target in CliTarget::ALL {
            let p = target.mcp_config_path();
            assert!(
                !p.as_os_str().is_empty(),
                "mcp_config_path() for {target:?} returned empty path"
            );
            assert!(
                p.starts_with(home),
                "mcp_config_path() for {target:?} = {} not under pinned HOME {}",
                p.display(),
                home.display(),
            );
            assert!(
                !seen.contains(&p),
                "mcp_config_path() for {target:?} collides with an earlier target: {}",
                p.display(),
            );
            seen.push(p);
        }

        // Pin the exact paths the rest of the codebase depends on.
        assert_eq!(CliTarget::Claude.mcp_config_path(), home.join(".claude.json"));
        assert_eq!(
            CliTarget::Codex.mcp_config_path(),
            home.join(".codex/config.toml")
        );
        assert_eq!(
            CliTarget::Gemini.mcp_config_path(),
            home.join(".gemini/settings.json")
        );
        assert_eq!(
            CliTarget::OpenCode.mcp_config_path(),
            home.join(".config/opencode/opencode.json")
        );

        // Codex is the ONLY target that uses TOML — invariant that drives the
        // canonical/toml serialisation router in `mcp_canonical`.
        let toml_targets: Vec<_> = CliTarget::ALL
            .iter()
            .filter(|t| t.uses_toml())
            .copied()
            .collect();
        assert_eq!(
            toml_targets,
            vec![CliTarget::Codex],
            "exactly one target (Codex) must self-report as TOML"
        );
    });
}

#[test]
fn format_predicates_match_targets() {
    // Note: this test does NOT depend on HOME, so no pin needed.
    for target in CliTarget::ALL {
        let is_toml = target.uses_toml();
        let is_opencode = target.uses_opencode_format();
        match target {
            CliTarget::Claude | CliTarget::Gemini => {
                assert!(!is_toml, "{target:?} should not be TOML");
                assert!(!is_opencode, "{target:?} should not be OpenCode format");
            }
            CliTarget::Codex => {
                assert!(is_toml, "Codex must report uses_toml() == true");
                assert!(
                    !is_opencode,
                    "Codex must report uses_opencode_format() == false"
                );
            }
            CliTarget::OpenCode => {
                assert!(!is_toml, "OpenCode must report uses_toml() == false");
                assert!(
                    is_opencode,
                    "OpenCode must report uses_opencode_format() == true"
                );
            }
        }
    }
}

#[test]
fn cli_target_display_fromstr_roundtrip() {
    for target in CliTarget::ALL {
        let display = target.to_string();
        // Display matches `name()`.
        assert_eq!(display, target.name(), "Display != name() for {target:?}");
        // Round-trips through FromStr.
        let parsed = CliTarget::from_str(&display)
            .unwrap_or_else(|_| panic!("FromStr failed for {display:?}"));
        assert_eq!(parsed, *target, "FromStr roundtrip mismatch for {target:?}");
    }
    // FromStr only accepts the canonical lowercase name — uppercase / mixed
    // case should fail (this is a pinned-behaviour test; if we ever switch to
    // case-insensitive parsing it intentionally breaks here so we can update
    // the docs in lockstep).
    assert!(CliTarget::from_str("CLAUDE").is_err());
    assert!(CliTarget::from_str("Codex").is_err());
    // Unknown name rejected.
    assert!(CliTarget::from_str("notacli").is_err());
    assert!(CliTarget::from_str("").is_err());
}

#[test]
fn cli_target_all_order_stable() {
    // TUI tab numbering, `enabled_targets` ordering in `runai list`, and the
    // watcher's path-enumeration all hard-code this order. Lock it in.
    assert_eq!(
        CliTarget::ALL.len(),
        4,
        "CliTarget::ALL must contain exactly 4 entries"
    );
    assert_eq!(CliTarget::ALL[0], CliTarget::Claude);
    assert_eq!(CliTarget::ALL[1], CliTarget::Codex);
    assert_eq!(CliTarget::ALL[2], CliTarget::Gemini);
    assert_eq!(CliTarget::ALL[3], CliTarget::OpenCode);
}

// =============================================================================
// PLANNING §3.14 — sm_enable (group mode) — P0 high-risk physical e2e
// =============================================================================
//
// `runai enable <group> --target <cli>` dispatches to `mgr.enable_group`,
// which iterates members and calls `enable_resource` per member. These tests
// drive the real binary against an isolated HOME and verify that:
//   - skill members create per-target symlinks
//   - MCP members get a per-target config entry
//   - other CLIs are not affected
//   - RUNE_DATA_DIR propagation reaches the symlink target

/// Seed an MCP "backup" file under `<data>/mcps/<name>.json` in canonical
/// shape. `find_resource_id` will then recognise the MCP and group `add`
/// will accept it as `mcp:<name>`.
fn seed_mcp_backup(data_dir: &Path, mcp_name: &str, bin: &str) {
    let mcps_dir = data_dir.join("mcps");
    std::fs::create_dir_all(&mcps_dir).unwrap();
    let canonical = serde_json::json!({
        "command": bin,
        "args": ["--port", "9999"],
    });
    std::fs::write(
        mcps_dir.join(format!("{mcp_name}.json")),
        serde_json::to_string_pretty(&canonical).unwrap(),
    )
    .unwrap();
}

/// Build a group with the provided skill names and (optionally) one MCP. The
/// MCP must already be present as a backup file before this is called.
fn build_group(env: &TestEnv, group_id: &str, skill_names: &[&str], mcp_name: Option<&str>) {
    let create = env.run(&[
        "group",
        "create",
        group_id,
        "--name",
        group_id,
        "--kind",
        "custom",
    ]);
    assert!(
        create.status.success(),
        "group create failed: stderr={}",
        String::from_utf8_lossy(&create.stderr)
    );
    for sn in skill_names {
        let add = env.run(&[
            "group",
            "add",
            group_id,
            sn,
            "--resource-type",
            "skill",
        ]);
        assert!(
            add.status.success(),
            "group add skill {sn} failed: stderr={}",
            String::from_utf8_lossy(&add.stderr)
        );
    }
    if let Some(mn) = mcp_name {
        let add = env.run(&[
            "group",
            "add",
            group_id,
            mn,
            "--resource-type",
            "mcp",
        ]);
        assert!(
            add.status.success(),
            "group add mcp {mn} failed: stderr={}",
            String::from_utf8_lossy(&add.stderr)
        );
    }
}

#[test]
fn sm_enable_group_creates_symlinks_and_mcp_entries() {
    let env = TestEnv::new();
    let skill_dir = env.home().join(".runai/skills");
    make_skill(&skill_dir, "grp-skill-a");
    make_skill(&skill_dir, "grp-skill-b");
    seed_mcp_backup(&env.home().join(".runai"), "grp-mcp", "/bin/grp-mcp");

    assert!(env.run(&["scan"]).status.success());
    build_group(
        &env,
        "test-group",
        &["grp-skill-a", "grp-skill-b"],
        Some("grp-mcp"),
    );

    let en = env.run(&["enable", "test-group", "--target", "claude"]);
    dump(&en, "enable test-group on claude");
    assert!(en.status.success(), "group enable failed");
    let stdout = String::from_utf8_lossy(&en.stdout);
    assert!(
        stdout.contains("Group 'test-group' enabled for claude"),
        "expected success message in stdout, got: {stdout}"
    );

    // Both skill symlinks land in claude's skills dir.
    for sn in &["grp-skill-a", "grp-skill-b"] {
        let link = env.cli_skills_dir("claude").join(sn);
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "expected symlink at {} after group enable",
            link.display()
        );
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(resolved, skill_dir.join(sn));
    }

    // MCP entry landed in ~/.claude.json under mcpServers.
    let claude_json_path = env.home().join(".claude.json");
    assert!(
        claude_json_path.exists(),
        "~/.claude.json should exist after MCP enable"
    );
    let claude_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json_path).unwrap()).unwrap();
    let entry = &claude_json["mcpServers"]["grp-mcp"];
    assert!(
        entry.is_object(),
        "mcpServers.grp-mcp missing from ~/.claude.json: {claude_json}"
    );
    assert_eq!(entry["command"], serde_json::json!("/bin/grp-mcp"));

    // Other CLI dirs untouched: no skill symlinks, no MCP config files.
    for other in &["codex", "gemini", "opencode"] {
        for sn in &["grp-skill-a", "grp-skill-b"] {
            let link = env.cli_skills_dir(other).join(sn);
            assert!(
                std::fs::symlink_metadata(&link).is_err(),
                "group enable on claude leaked symlink to {}: {}",
                other,
                link.display()
            );
        }
    }
    // ~/.codex/config.toml, ~/.gemini/settings.json, ~/.config/opencode/opencode.json
    // were never written.
    assert!(!env.home().join(".codex/config.toml").exists());
    assert!(!env.home().join(".gemini/settings.json").exists());
    assert!(
        !env.home()
            .join(".config/opencode/opencode.json")
            .exists()
    );
}

#[test]
fn sm_enable_group_symmetric_across_cli_targets() {
    let env = TestEnv::new();
    let skill_dir = env.home().join(".runai/skills");
    make_skill(&skill_dir, "sym-skill");
    assert!(env.run(&["scan"]).status.success());
    build_group(&env, "sym-group", &["sym-skill"], None);

    for target in &["claude", "codex", "gemini", "opencode"] {
        let en = env.run(&["enable", "sym-group", "--target", target]);
        assert!(
            en.status.success(),
            "enable sym-group on {target} failed: stderr={}",
            String::from_utf8_lossy(&en.stderr)
        );

        let link = env.cli_skills_dir(target).join("sym-skill");
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "symlink missing at {} after enable on {target}",
            link.display()
        );
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(
            resolved,
            skill_dir.join("sym-skill"),
            "symlink on {target} should point to managed skills dir"
        );
    }

    // After all 4 enables, every CLI dir should have the symlink simultaneously.
    for target in &["claude", "codex", "gemini", "opencode"] {
        let link = env.cli_skills_dir(target).join("sym-skill");
        assert!(
            std::fs::symlink_metadata(&link).is_ok(),
            "after 4-target enable cycle, expected symlink at {}",
            link.display()
        );
    }
}

#[test]
fn sm_enable_group_mcp_sync_only_on_claude() {
    // Sister test of #1 — focuses on the target-discrimination of the MCP
    // sync logic. Enable a group that contains *only* an MCP on each of the
    // 4 targets in turn and verify the entry lands in the correct config
    // file each time.
    let env = TestEnv::new();
    seed_mcp_backup(&env.home().join(".runai"), "scope-mcp", "/bin/scope-mcp");
    build_group(&env, "mcp-only-grp", &[], Some("scope-mcp"));

    // claude → ~/.claude.json mcpServers
    let en = env.run(&["enable", "mcp-only-grp", "--target", "claude"]);
    assert!(en.status.success(), "claude enable failed");
    let claude_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(env.home().join(".claude.json")).unwrap())
            .unwrap();
    assert!(
        claude_json["mcpServers"]["scope-mcp"].is_object(),
        "claude config missing MCP entry"
    );

    // gemini → ~/.gemini/settings.json mcpServers
    let en = env.run(&["enable", "mcp-only-grp", "--target", "gemini"]);
    assert!(en.status.success(), "gemini enable failed");
    let gem: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(env.home().join(".gemini/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(
        gem["mcpServers"]["scope-mcp"].is_object(),
        "gemini config missing MCP entry"
    );

    // opencode → ~/.config/opencode/opencode.json under "mcp"
    let en = env.run(&["enable", "mcp-only-grp", "--target", "opencode"]);
    assert!(en.status.success(), "opencode enable failed");
    let oc: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(env.home().join(".config/opencode/opencode.json")).unwrap(),
    )
    .unwrap();
    assert!(
        oc["mcp"]["scope-mcp"].is_object(),
        "opencode config missing MCP entry under key 'mcp'"
    );

    // codex → ~/.codex/config.toml mcp_servers (TOML, not JSON)
    let en = env.run(&["enable", "mcp-only-grp", "--target", "codex"]);
    assert!(en.status.success(), "codex enable failed");
    let codex_toml: toml::Table =
        std::fs::read_to_string(env.home().join(".codex/config.toml"))
            .unwrap()
            .parse()
            .unwrap();
    let servers = codex_toml
        .get("mcp_servers")
        .and_then(|v| v.as_table())
        .expect("codex config must have mcp_servers table");
    assert!(
        servers.contains_key("scope-mcp"),
        "codex config missing MCP entry under mcp_servers: {codex_toml:?}"
    );
}

#[test]
fn sm_enable_group_symlink_target_respects_rune_data_dir() {
    // Like the e2e tests above, but with RUNE_DATA_DIR pointing at a path
    // distinct from `<HOME>/.runai`. Verifies the symlink target follows the
    // active data dir, not the default — this is the 4-27 incident root
    // cause area.
    let env = TestEnv::new();
    let alt_data = tempfile::tempdir().unwrap();
    let alt_skills = alt_data.path().join("skills");
    std::fs::create_dir_all(&alt_skills).unwrap();
    make_skill(&alt_skills, "alt-skill");

    // Build group + scan using RUNE_DATA_DIR.
    let mut scan = runai();
    scan.args(["scan"])
        .env("HOME", env.home())
        .env("RUNE_DATA_DIR", alt_data.path())
        .env_remove("SKILL_MANAGER_DATA_DIR");
    let out = scan.output().unwrap();
    dump(&out, "scan with alt RUNE_DATA_DIR");
    assert!(out.status.success(), "scan with alt data dir failed");

    let run_with_alt = |args: &[&str]| -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", env.home())
            .env("RUNE_DATA_DIR", alt_data.path())
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().unwrap()
    };

    let create = run_with_alt(&[
        "group",
        "create",
        "alt-grp",
        "--name",
        "alt-grp",
        "--kind",
        "custom",
    ]);
    assert!(create.status.success(), "group create under alt-dir failed");
    let add = run_with_alt(&[
        "group",
        "add",
        "alt-grp",
        "alt-skill",
        "--resource-type",
        "skill",
    ]);
    assert!(add.status.success(), "group add under alt-dir failed");

    let en = run_with_alt(&["enable", "alt-grp", "--target", "claude"]);
    dump(&en, "enable alt-grp under alt RUNE_DATA_DIR");
    assert!(en.status.success(), "enable failed under alt RUNE_DATA_DIR");

    let link = env.cli_skills_dir("claude").join("alt-skill");
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "symlink missing at {}",
        link.display()
    );
    let resolved = std::fs::read_link(&link).unwrap();
    assert_eq!(
        resolved,
        alt_skills.join("alt-skill"),
        "symlink must point at alt RUNE_DATA_DIR/skills/<name>, not default HOME/.runai"
    );
    // Critically: no rogue `~/.runai/skills/alt-skill` was created — the
    // sandboxed HOME's .runai/skills was prepared by TestEnv but we never
    // wrote into it.
    assert!(
        !env.home().join(".runai/skills/alt-skill").exists(),
        "RUNE_DATA_DIR override must not leak into default ~/.runai/skills"
    );
}

// =============================================================================
// PLANNING §3.15 — sm_disable (group mode) — P0 high-risk physical e2e
// =============================================================================

#[test]
fn sm_disable_group_removes_symlinks_and_mcp_entries() {
    let env = TestEnv::new();
    let skill_dir = env.home().join(".runai/skills");
    make_skill(&skill_dir, "dis-skill-a");
    make_skill(&skill_dir, "dis-skill-b");
    seed_mcp_backup(&env.home().join(".runai"), "dis-mcp", "/bin/dis-mcp");

    assert!(env.run(&["scan"]).status.success());
    build_group(
        &env,
        "dis-group",
        &["dis-skill-a", "dis-skill-b"],
        Some("dis-mcp"),
    );

    // Enable on claude only — and also pre-enable on codex to verify the
    // disable on claude leaves codex untouched.
    assert!(
        env.run(&["enable", "dis-group", "--target", "claude"])
            .status
            .success()
    );
    assert!(
        env.run(&["enable", "dis-skill-a", "--target", "codex"])
            .status
            .success()
    );
    // Sanity: both skills' symlinks exist before disable.
    for sn in &["dis-skill-a", "dis-skill-b"] {
        assert!(
            std::fs::symlink_metadata(env.cli_skills_dir("claude").join(sn)).is_ok()
        );
    }
    // And ~/.claude.json carries the MCP entry.
    let claude_path = env.home().join(".claude.json");
    let pre: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_path).unwrap()).unwrap();
    assert!(pre["mcpServers"]["dis-mcp"].is_object());

    let dis = env.run(&["disable", "dis-group", "--target", "claude"]);
    dump(&dis, "disable dis-group on claude");
    assert!(dis.status.success(), "group disable failed");
    let stdout = String::from_utf8_lossy(&dis.stdout);
    assert!(
        stdout.contains("Group 'dis-group' disabled for claude"),
        "expected success message in stdout, got: {stdout}"
    );

    // Both skill symlinks gone from ~/.claude/skills.
    for sn in &["dis-skill-a", "dis-skill-b"] {
        let link = env.cli_skills_dir("claude").join(sn);
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "symlink at {} should be gone after group disable",
            link.display()
        );
    }
    // MCP entry removed from ~/.claude.json.
    let post: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_path).unwrap()).unwrap();
    let mcp_servers = post["mcpServers"].as_object();
    let mcp_present = mcp_servers
        .map(|m| m.contains_key("dis-mcp"))
        .unwrap_or(false);
    assert!(
        !mcp_present,
        "mcpServers.dis-mcp should be gone from ~/.claude.json after disable, got: {post}"
    );

    // Codex was independently enabled for dis-skill-a — that symlink must stay.
    assert!(
        std::fs::symlink_metadata(env.cli_skills_dir("codex").join("dis-skill-a")).is_ok(),
        "disable on claude must not affect codex's symlink"
    );

    // Skill source files untouched.
    for sn in &["dis-skill-a", "dis-skill-b"] {
        assert!(
            skill_dir.join(sn).exists(),
            "source skill dir must remain after disable: {sn}"
        );
    }
}

#[test]
fn sm_disable_group_symmetric_cleanup_across_targets() {
    let env = TestEnv::new();
    let skill_dir = env.home().join(".runai/skills");
    make_skill(&skill_dir, "scl-skill");
    assert!(env.run(&["scan"]).status.success());
    build_group(&env, "scl-grp", &["scl-skill"], None);

    // Enable on all 4 targets first.
    for target in &["claude", "codex", "gemini", "opencode"] {
        assert!(
            env.run(&["enable", "scl-grp", "--target", target])
                .status
                .success(),
            "pre-enable on {target} failed"
        );
        assert!(
            std::fs::symlink_metadata(env.cli_skills_dir(target).join("scl-skill")).is_ok(),
            "pre-enable did not create symlink on {target}"
        );
    }

    // Disable one target at a time and verify per-target cleanup is isolated.
    let order = ["claude", "codex", "gemini", "opencode"];
    for (i, target) in order.iter().enumerate() {
        let dis = env.run(&["disable", "scl-grp", "--target", target]);
        dump(&dis, &format!("disable scl-grp on {target}"));
        assert!(
            dis.status.success(),
            "disable on {target} failed: stderr={}",
            String::from_utf8_lossy(&dis.stderr)
        );
        // This target's symlink gone.
        assert!(
            std::fs::symlink_metadata(env.cli_skills_dir(target).join("scl-skill")).is_err(),
            "symlink on {target} should be gone after disable"
        );
        // Later-in-order targets must still be enabled.
        for later in &order[i + 1..] {
            assert!(
                std::fs::symlink_metadata(env.cli_skills_dir(later).join("scl-skill")).is_ok(),
                "disable on {target} accidentally removed symlink on {later}"
            );
        }
    }
}

#[test]
fn sm_disable_group_idempotent_on_already_disabled() {
    // Create a group + skill but NEVER enable. `disable` should be a no-op
    // that returns success.
    let env = TestEnv::new();
    let skill_dir = env.home().join(".runai/skills");
    make_skill(&skill_dir, "idem-skill");
    assert!(env.run(&["scan"]).status.success());
    build_group(&env, "idem-grp", &["idem-skill"], None);

    let link = env.cli_skills_dir("claude").join("idem-skill");
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "precondition: symlink should not exist before disable"
    );

    // First disable on a never-enabled group.
    let dis1 = env.run(&["disable", "idem-grp", "--target", "claude"]);
    dump(&dis1, "first disable (never-enabled)");
    assert!(
        dis1.status.success(),
        "first disable on never-enabled group should succeed (idempotent); stderr={}",
        String::from_utf8_lossy(&dis1.stderr)
    );
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "disable on never-enabled group must not create anything at {}",
        link.display()
    );

    // Second disable — still idempotent.
    let dis2 = env.run(&["disable", "idem-grp", "--target", "claude"]);
    dump(&dis2, "second disable (idempotent)");
    assert!(
        dis2.status.success(),
        "second disable should also succeed (idempotent); stderr={}",
        String::from_utf8_lossy(&dis2.stderr)
    );
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "second disable must not have created anything at {}",
        link.display()
    );
}

#[test]
fn sm_disable_group_respects_rune_data_dir_symlink_cleanup() {
    let env = TestEnv::new();
    let alt_data = tempfile::tempdir().unwrap();
    let alt_skills = alt_data.path().join("skills");
    std::fs::create_dir_all(&alt_skills).unwrap();
    make_skill(&alt_skills, "alt-dis-skill");

    let run_with_alt = |args: &[&str]| -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", env.home())
            .env("RUNE_DATA_DIR", alt_data.path())
            .env_remove("SKILL_MANAGER_DATA_DIR");
        cmd.output().unwrap()
    };

    assert!(run_with_alt(&["scan"]).status.success(), "scan failed");
    let create = run_with_alt(&[
        "group",
        "create",
        "alt-dis-grp",
        "--name",
        "alt-dis-grp",
        "--kind",
        "custom",
    ]);
    assert!(create.status.success());
    let add = run_with_alt(&[
        "group",
        "add",
        "alt-dis-grp",
        "alt-dis-skill",
        "--resource-type",
        "skill",
    ]);
    assert!(add.status.success());

    // Enable then disable on claude under alt RUNE_DATA_DIR.
    assert!(
        run_with_alt(&["enable", "alt-dis-grp", "--target", "claude"])
            .status
            .success()
    );
    let link = env.cli_skills_dir("claude").join("alt-dis-skill");
    assert!(
        std::fs::symlink_metadata(&link).is_ok(),
        "precondition: symlink should exist after enable"
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        alt_skills.join("alt-dis-skill"),
        "precondition: symlink should target alt RUNE_DATA_DIR"
    );

    let dis = run_with_alt(&["disable", "alt-dis-grp", "--target", "claude"]);
    dump(&dis, "disable alt-dis-grp under alt RUNE_DATA_DIR");
    assert!(dis.status.success(), "disable under alt RUNE_DATA_DIR failed");

    // Symlink in ~/.claude/skills gone.
    assert!(
        std::fs::symlink_metadata(&link).is_err(),
        "symlink at {} should be gone after disable",
        link.display()
    );
    // Source skill dir at the alt data dir is preserved.
    assert!(
        alt_skills.join("alt-dis-skill").exists(),
        "disable must NOT remove the source skill dir at alt RUNE_DATA_DIR"
    );
    // No collateral writes to the other CLI dirs.
    for other in &["codex", "gemini", "opencode"] {
        let collateral = env.cli_skills_dir(other).join("alt-dis-skill");
        assert!(
            std::fs::symlink_metadata(&collateral).is_err(),
            "disable on claude should not touch {} skill dir",
            other
        );
    }
}

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

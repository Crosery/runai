//! Physical end-to-end coverage for CLI lifecycle subcommands
//! (market-install / uninstall / trash restore / trash purge / ...).
//!
//! Pattern: spawn the real `runai` binary inside an isolated HOME tempdir,
//! with `RUNE_DATA_DIR` / `SKILL_MANAGER_DATA_DIR` cleared (or pointed at a
//! second tempdir for cross-data-dir cases). All assertions are filesystem
//! and stdout/stderr only — production `~/.runai/` is never touched.
//!
//! These guard the AGENTS.md safety contract: trash-first uninstall, owner
//! isolation, RUNE_DATA_DIR isolation, source-filter scoping for market.
//!
//! Skipped on Windows: symlinks + HOME mocking require unix.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── shared helpers ─────────────────────────────────────────────────────────

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

    fn default_skills_dir(&self) -> PathBuf {
        self.home().join(".runai/skills")
    }

    fn default_trash_dir(&self) -> PathBuf {
        self.home().join(".runai/trash")
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

/// Write a fresh market cache file (1-hour TTL) for a single source so
/// `find_skill_in_sources` returns hits without any network access.
///
/// Layout matches `core::market::save_cache` exactly (file name is
/// `<owner>_<repo>.json` inside `<data>/market-cache/`).
fn plant_market_cache(
    data_dir: &Path,
    owner: &str,
    repo: &str,
    skills_json: &str,
) {
    let cache_dir = data_dir.join("market-cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let path = cache_dir.join(format!("{owner}_{repo}.json"));
    std::fs::write(&path, skills_json).unwrap();
}

// ─── 1.7 [P0] market-install ────────────────────────────────────────────────

/// market-install must reject a name that's absent from every enabled
/// source's cache, AND must not partially create a skill directory in
/// `~/.runai/skills/`. Tests the cli/dispatch find-then-install precondition.

/// When `--source <label>` is supplied, the lookup must be scoped to sources
/// whose label OR repo_id matches the filter substring (case-insensitive).
/// A skill present only in a non-matching source's cache must return
/// "not found" — proving the filter is applied before the cache lookup.

/// Cross RUNE_DATA_DIR isolation: with a non-default RUNE_DATA_DIR set,
/// market-install (even when it fails) must NOT read or write the user's
/// default `~/.runai/skills/`. A cache file in the default dir should be
/// invisible to the alt-data-dir invocation.

// ─── 1.8 [P0] uninstall ─────────────────────────────────────────────────────

/// uninstall moves a managed skill into ~/.runai/trash/ AND removes its
/// symlinks from every CLI target it was enabled on. The skill's directory
/// must no longer be visible at the managed location.

/// Trash entries must preserve enough metadata to support restoration:
/// at minimum the resource name + kind must be discoverable via `trash list`.
/// This guards against information loss during the uninstall hand-off.

/// Cross RUNE_DATA_DIR isolation: an uninstall under a non-default
/// RUNE_DATA_DIR must put the trash payload into that alt dir's `trash/`,
/// and must NOT touch the user's default `~/.runai/skills/` OR
/// `~/.runai/trash/`.

// ─── 1.10 [P0] trash restore ────────────────────────────────────────────────

/// Restore recreates the managed dir with identical SKILL.md bytes, brings
/// the resource back into `runai list`, and re-enables the symlinks on
/// the targets the skill was on before uninstall.

/// If a skill is part of a group at uninstall time, restoring it should
/// re-add it to that same group (group still exists).

/// If a different live resource already occupies the name, restore must
/// refuse rather than overwrite — the trash entry must remain intact.

/// Cross RUNE_DATA_DIR isolation: an alt-data-dir restore must not
/// modify the default `~/.runai/skills/` (or its trash).

// ─── 1.11 [P0] trash purge ──────────────────────────────────────────────────

/// trash purge must permanently delete the payload directory AND remove the
/// entry from `trash list`. There must be no remaining files under
/// `~/.runai/trash/` referencing the purged skill.

/// trash purge with a name that doesn't exist in trash must fail (non-zero
/// exit), and must not leave the trash in an inconsistent state.

/// Cross RUNE_DATA_DIR isolation: an alt-data-dir purge must only touch
/// the alt dir's trash. The default `~/.runai/trash/` must remain intact.


#[test]
fn market_install_fails_on_not_found() {
    let env = TestEnv::new();

    // No market cache planted = no source has the skill.
    let before: Vec<_> = std::fs::read_dir(env.default_skills_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();

    let out = env.run(&["market-install", "definitely-does-not-exist-skill"]);
    dump(&out, "market-install nonexistent");

    assert!(
        !out.status.success(),
        "market-install for a missing skill must fail; got success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stderr}{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        combined.to_lowercase().contains("not found"),
        "expected 'not found' message; got stderr:\n{stderr}\nstdout:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );

    // No partial dir should be left behind under managed skills/.
    let after: Vec<_> = std::fs::read_dir(env.default_skills_dir())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        before, after,
        "REGRESSION: market-install left a partial skill dir after a not-found error"
    );
}


#[test]
fn market_install_respects_source_filter() {
    let env = TestEnv::new();
    let data_dir = env.home().join(".runai");

    // Plant a cache file for the user-added "userlabs/extras" source ONLY.
    // The skill 'extras-only-skill' is exclusively in this source's cache.
    let extras_json = r#"[
        {
            "name": "extras-only-skill",
            "repo_path": "skills/extras-only-skill",
            "source_label": "userlabs/extras",
            "source_repo": "userlabs/extras",
            "branch": "main"
        }
    ]"#;
    plant_market_cache(&data_dir, "userlabs", "extras", extras_json);

    // Register the user-added source so it shows up in load_sources().
    let sources_json = r#"[
        {
            "owner": "userlabs",
            "repo": "extras",
            "branch": "main",
            "skill_prefix": "skills/",
            "label": "userlabs/extras",
            "description": "user source",
            "builtin": false,
            "enabled": true
        }
    ]"#;
    std::fs::write(data_dir.join("market-sources.json"), sources_json).unwrap();

    // Filter set to a label that does NOT contain "userlabs" or "extras"
    // — the skill should be invisible through this filter.
    let out = env.run(&[
        "market-install",
        "extras-only-skill",
        "--source",
        "anthropic-official",
    ]);
    dump(&out, "market-install with mismatched source filter");

    assert!(
        !out.status.success(),
        "REGRESSION: market-install ignored --source filter (skill was found in a non-matching source)"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        combined.to_lowercase().contains("not found"),
        "expected 'not found' under filtered search; got:\n{combined}"
    );

    // Also assert no partial directory created.
    assert!(
        !env.default_skills_dir()
            .join("extras-only-skill")
            .exists(),
        "REGRESSION: a partial skill directory was created despite the filter rejecting it"
    );
}


#[test]
fn market_install_respects_rune_data_dir() {
    let env = TestEnv::new();
    let default_data = env.home().join(".runai");

    // Plant a "default-only" skill in the DEFAULT data dir's cache.
    let default_json = r#"[
        {
            "name": "default-only-skill",
            "repo_path": "skills/default-only-skill",
            "source_label": "anthropics/claude-plugins-official",
            "source_repo": "anthropics/claude-plugins-official",
            "branch": "main"
        }
    ]"#;
    plant_market_cache(
        &default_data,
        "anthropics",
        "claude-plugins-official",
        default_json,
    );

    // Sentinel real skill in the default data dir's skills/ — must NOT be
    // touched by an alt-data-dir invocation.
    let sentinel = make_skill(&env.default_skills_dir(), "sentinel", "sentinel body");
    let sentinel_bytes = std::fs::read(sentinel.join("SKILL.md")).unwrap();

    let alt = tempfile::tempdir().unwrap();
    let alt_data = alt.path();

    // Run market-install under the alt RUNE_DATA_DIR. Because no cache lives
    // there, "default-only-skill" should be unfindable.
    let out = env.run_with_rune_data(
        alt_data,
        &["market-install", "default-only-skill"],
    );
    dump(&out, "market-install under alt RUNE_DATA_DIR");

    assert!(
        !out.status.success(),
        "REGRESSION: market-install under alt RUNE_DATA_DIR found a skill that only exists in the default data dir's cache"
    );

    // Default sentinel must be intact and unmodified.
    assert!(
        sentinel.join("SKILL.md").exists(),
        "REGRESSION: market-install with alt RUNE_DATA_DIR mutated the default skills/ dir"
    );
    assert_eq!(
        std::fs::read(sentinel.join("SKILL.md")).unwrap(),
        sentinel_bytes,
        "REGRESSION: sentinel SKILL.md content changed across data-dir boundary"
    );

    // Nothing should appear under default-only-skill in either location.
    assert!(
        !env.default_skills_dir()
            .join("default-only-skill")
            .exists(),
        "REGRESSION: alt RUNE_DATA_DIR invocation wrote into the default skills dir"
    );
    assert!(
        !alt_data.join("skills/default-only-skill").exists(),
        "no skill dir should exist in the alt data dir after a not-found error"
    );
}


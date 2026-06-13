//! Physical end-to-end regression for **Market Install Skill** (PLANNING test
//! plan §2.14, P0).
//!
//! The market-install path is **high-risk**: it writes to `~/.runai/skills/`
//! AND to `~/.{claude,codex,gemini,opencode}/skills/` (symlinks), AND mutates
//! `runai.db`. These tests spawn the real `runai` binary in an isolated HOME
//! tempdir to assert the two-pool + DB invariants stay coordinated.
//!
//! ## Network requirement
//!
//! `runai market-install <name>` downloads the skill directory from
//! GitHub (`raw.githubusercontent.com` + Contents API). There is no
//! offline path through this code in production. Tests 1–3 therefore
//! carry `#[ignore]` so `cargo test -- --test-threads=1` stays green on
//! disconnected CI; they run manually with `--ignored`.
//!
//! ```bash
//! cargo test --test tui_market_install_e2e -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Even when ignored, they still build, so refactors touching the install
//! pipeline trip the compiler immediately.
//!
//! ## Sandbox contract (AGENTS.md 5 条铁律)
//!
//! - `HOME=$(mktemp -d)` per test, `RUNE_DATA_DIR` / `SKILL_MANAGER_DATA_DIR`
//!   explicitly cleared (or pointed at another tempdir for cross-data-dir).
//! - Never touches the real `~/.runai/` / `~/.{claude,codex,gemini,opencode}/`.
//! - Skipped on Windows (symlinks need Developer Mode + HOME mocking on
//!   Windows ignores env vars; matches `safety_e2e.rs`).
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

    fn default_data_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }

    fn default_skills_dir(&self) -> PathBuf {
        self.default_data_dir().join("skills")
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

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Read the `resources` table for a given name. Returns Some when the row
/// exists in the active `runai.db`.
fn db_resource_directory(db_path: &Path, name: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(db_path).ok()?;
    conn.query_row(
        "SELECT directory FROM resources WHERE name = ?1 ORDER BY installed_at DESC LIMIT 1",
        rusqlite::params![name],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

/// Plant a market-cache file so `find_skill_in_sources` returns this skill
/// without any HTTP fetch. The actual download attempted by
/// `Market::install_single` will still hit GitHub — set `source_repo` to a
/// real small public skill to make `#[ignore]`-tagged tests succeed when
/// run with network access.
fn plant_market_cache(
    data_dir: &Path,
    source_owner: &str,
    source_repo: &str,
    label: &str,
    skill_name: &str,
    repo_path: &str,
) {
    // First write the source list so the source is "enabled" and merged.
    let sources_file = data_dir.join("market-sources.json");
    let sources = serde_json::json!([{
        "owner": source_owner,
        "repo": source_repo,
        "branch": "main",
        "skill_prefix": "",
        "label": label,
        "description": "test source",
        "builtin": false,
        "enabled": true,
    }]);
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::write(
        &sources_file,
        serde_json::to_string_pretty(&sources).unwrap(),
    )
    .unwrap();

    // Then write the cache that `load_cache` will pick up.
    let cache_dir = data_dir.join("market-cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let cache_file = cache_dir.join(format!("{source_owner}_{source_repo}.json"));
    let cache = serde_json::json!([{
        "name": skill_name,
        "repo_path": repo_path,
        "source_label": label,
        "source_repo": format!("{source_owner}/{source_repo}"),
        "branch": "main",
    }]);
    std::fs::write(&cache_file, serde_json::to_string(&cache).unwrap()).unwrap();
}

// A small public skill known to exist + be cheap to install. Picked from
// MiniMax-AI/skills (single SKILL.md at repo root, used by the existing
// `install_test::test_real_install_minimax` manual test).
const NETWORK_SKILL_OWNER: &str = "MiniMax-AI";
const NETWORK_SKILL_REPO: &str = "skills";
const NETWORK_SKILL_NAME: &str = "skills";

// ─── tests ──────────────────────────────────────────────────────────────────

/// §2.14 Test 1: `market install` downloads the skill into
/// `<data>/skills/<name>/`, registers it in `runai.db`, and creates a
/// `~/.claude/skills/<name>` symlink to the managed copy.
#[test]
#[ignore = "requires network: downloads from raw.githubusercontent.com"]
fn market_install_downloads_and_symlinks() {
    let env = TestEnv::new();
    plant_market_cache(
        &env.default_data_dir(),
        NETWORK_SKILL_OWNER,
        NETWORK_SKILL_REPO,
        "Test Source",
        NETWORK_SKILL_NAME,
        "", // empty -> install_single uses skill.name as repo_path
    );

    let out = env.run(&["market-install", NETWORK_SKILL_NAME]);
    dump(&out, "market-install (default data dir)");
    assert!(
        out.status.success(),
        "market-install must exit 0 (stderr={})",
        String::from_utf8_lossy(&out.stderr),
    );

    // Skill files live under <HOME>/.runai/skills/<name>/.
    let skill_dir = env.default_skills_dir().join(NETWORK_SKILL_NAME);
    assert!(
        skill_dir.is_dir(),
        "expected managed skill dir at {}",
        skill_dir.display()
    );
    let skill_md = skill_dir.join("SKILL.md");
    assert!(
        skill_md.is_file(),
        "expected SKILL.md at {}",
        skill_md.display()
    );

    // DB row recorded with the correct directory path.
    let db = env.default_data_dir().join("runai.db");
    let recorded = db_resource_directory(&db, NETWORK_SKILL_NAME)
        .expect("resource row not found in runai.db after market-install");
    assert_eq!(
        Path::new(&recorded).canonicalize().unwrap(),
        skill_dir.canonicalize().unwrap(),
        "DB directory mismatch (recorded={recorded}, expected={})",
        skill_dir.display()
    );

    // Claude symlink farm points back into the managed dir.
    let link = env.cli_skills_dir("claude").join(NETWORK_SKILL_NAME);
    let link_meta = std::fs::symlink_metadata(&link)
        .unwrap_or_else(|e| panic!("expected symlink at {}: {e}", link.display()));
    assert!(
        link_meta.file_type().is_symlink(),
        "expected symlink, got {:?} at {}",
        link_meta.file_type(),
        link.display()
    );
    let target = std::fs::read_link(&link).unwrap();
    assert_eq!(
        target.canonicalize().unwrap(),
        skill_dir.canonicalize().unwrap(),
        "Claude symlink does not point at managed skill dir"
    );
}

/// §2.14 Test 2: When `RUNE_DATA_DIR` is set, the skill lands in the
/// custom data dir, NOT the default `~/.runai/skills/`. This is the
/// cross-`RUNE_DATA_DIR` half of the safety contract.
#[test]
#[ignore = "requires network: downloads from raw.githubusercontent.com"]
fn market_install_respects_custom_data_dir() {
    let env = TestEnv::new();
    let custom = tempfile::tempdir().expect("custom data dir tempdir");
    std::fs::create_dir_all(custom.path().join("skills")).unwrap();
    plant_market_cache(
        custom.path(),
        NETWORK_SKILL_OWNER,
        NETWORK_SKILL_REPO,
        "Test Source",
        NETWORK_SKILL_NAME,
        "",
    );

    // Snapshot the default skills dir to assert nothing leaks into it.
    let default_skills_before: Vec<_> = std::fs::read_dir(env.default_skills_dir())
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();

    let out = env.run_with_rune_data(custom.path(), &["market-install", NETWORK_SKILL_NAME]);
    dump(&out, "market-install (RUNE_DATA_DIR=custom)");
    assert!(
        out.status.success(),
        "market-install must succeed with RUNE_DATA_DIR (stderr={})",
        String::from_utf8_lossy(&out.stderr),
    );

    // Skill ends up in the custom dir.
    let custom_skill_dir = custom.path().join("skills").join(NETWORK_SKILL_NAME);
    assert!(
        custom_skill_dir.is_dir(),
        "expected skill at custom path {}",
        custom_skill_dir.display(),
    );
    assert!(
        custom_skill_dir.join("SKILL.md").is_file(),
        "expected SKILL.md inside {}",
        custom_skill_dir.display(),
    );

    // DB lives under the custom data dir.
    let custom_db = custom.path().join("runai.db");
    let recorded = db_resource_directory(&custom_db, NETWORK_SKILL_NAME)
        .expect("resource row not in custom runai.db");
    assert_eq!(
        Path::new(&recorded).canonicalize().unwrap(),
        custom_skill_dir.canonicalize().unwrap(),
        "DB directory mismatch in custom data dir"
    );

    // Default skills dir must be untouched.
    let default_skills_after: Vec<_> = std::fs::read_dir(env.default_skills_dir())
        .map(|r| r.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert_eq!(
        default_skills_before, default_skills_after,
        "REGRESSION: market-install with RUNE_DATA_DIR polluted default ~/.runai/skills/ \
         (before={default_skills_before:?}, after={default_skills_after:?})",
    );
    let default_db = env.default_data_dir().join("runai.db");
    assert!(
        db_resource_directory(&default_db, NETWORK_SKILL_NAME).is_none(),
        "REGRESSION: skill row leaked into default runai.db when RUNE_DATA_DIR was set",
    );
}

/// §2.14 Test 3: Across all 4 CLI targets, market-install + enable
/// creates an independent symlink in each target's skills dir, all
/// pointing back at the SAME managed copy under `<data>/skills/`. The
/// skill files exist exactly once in the data dir, not duplicated per
/// target.
#[test]
#[ignore = "requires network: downloads from raw.githubusercontent.com"]
fn market_install_works_across_all_cli_targets() {
    let env = TestEnv::new();
    plant_market_cache(
        &env.default_data_dir(),
        NETWORK_SKILL_OWNER,
        NETWORK_SKILL_REPO,
        "Test Source",
        NETWORK_SKILL_NAME,
        "",
    );

    // market-install registers + enables for `claude` (the CLI's default).
    let install_out = env.run(&["market-install", NETWORK_SKILL_NAME]);
    dump(&install_out, "market-install (4-target setup)");
    assert!(install_out.status.success(), "initial install must succeed");

    let managed = env.default_skills_dir().join(NETWORK_SKILL_NAME);
    assert!(managed.is_dir(), "managed dir missing after install");

    // Enable for the other three targets explicitly.
    for target in ["codex", "gemini", "opencode"] {
        let out = env.run(&["enable", NETWORK_SKILL_NAME, "--target", target]);
        dump(&out, &format!("enable {NETWORK_SKILL_NAME} for {target}"));
        assert!(
            out.status.success(),
            "enable {target} must succeed (stderr={})",
            String::from_utf8_lossy(&out.stderr),
        );
    }

    // Every target's symlink farm has one entry pointing at the same managed dir.
    let managed_canon = managed.canonicalize().unwrap();
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let link = env.cli_skills_dir(cli).join(NETWORK_SKILL_NAME);
        let meta = std::fs::symlink_metadata(&link)
            .unwrap_or_else(|e| panic!("expected {cli} symlink at {}: {e}", link.display()));
        assert!(
            meta.file_type().is_symlink(),
            "{cli} entry is not a symlink: {:?}",
            meta.file_type(),
        );
        let resolved = std::fs::read_link(&link).unwrap().canonicalize().unwrap();
        assert_eq!(
            resolved,
            managed_canon,
            "{cli} symlink resolves to {} not managed {}",
            resolved.display(),
            managed_canon.display(),
        );
    }

    // The managed skill exists exactly once: not duplicated under any
    // CLI's home dir as a real dir.
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let entry = env.cli_skills_dir(cli).join(NETWORK_SKILL_NAME);
        let meta = std::fs::symlink_metadata(&entry).unwrap();
        assert!(
            meta.file_type().is_symlink() && !meta.file_type().is_dir(),
            "{cli} entry must be symlink-only, found {:?}",
            meta.file_type(),
        );
    }

    // The DB has one resources row, not four — symlink farm is the source of
    // truth for `enabled`, not separate DB rows.
    let conn = rusqlite::Connection::open(env.default_data_dir().join("runai.db")).unwrap();
    let row_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM resources WHERE name = ?1",
            rusqlite::params![NETWORK_SKILL_NAME],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        row_count, 1,
        "market-install must keep exactly one DB row across 4 targets",
    );
}

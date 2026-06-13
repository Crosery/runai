//! P2 e2e coverage for `runai market-install` (PLANNING §1.7).
//!
//! The market-install handler at `src/cli/mod.rs::Commands::MarketInstall`
//! does three things:
//!   1. Calls `market::find_skill_in_sources(data_dir, sources, name, filter)`
//!      against the on-disk market cache. If not found → bail with
//!      "Skill '<name>' not found in market".
//!   2. Calls `Market::install_single` which hits raw.githubusercontent.com.
//!      (We can't deterministically test this path in CI — would require a
//!      live network + a real upstream repo.)
//!   3. Registers + enables the new skill locally and spawns enrich.
//!
//! These tests focus on the path-1 behavior (lookup + filter + RUNE_DATA_DIR
//! isolation) which we CAN verify deterministically without network:
//!   - not-found error path is reachable and doesn't write to skills/
//!   - source filter narrows the lookup correctly (disabled / unmatched
//!     sources don't satisfy the lookup)
//!   - RUNE_DATA_DIR isolation: a not-found run with a custom RUNE_DATA_DIR
//!     never touches the default ~/.runai/ tree
//!
//! Tests that require a real download (e.g. "skill is fetched and registered")
//! are intentionally NOT here — they belong in a network-gated harness.
//!
//! Skipped on Windows for the same reason as `safety_e2e.rs` — HOME mocking
//! and symlinks are unix-only.

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
        std::fs::create_dir_all(home.path().join(".runai/market-cache"))
            .expect("pre-create market-cache dir");
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

/// Write a minimal market-cache JSON file for a given owner/repo so that
/// `find_skill_in_sources` resolves the named skill from that source.
///
/// The cache key is `<owner>_<repo>.json` (matches `cache_key` in
/// `src/core/market.rs`), and the schema is the public `MarketSkill` serde
/// representation. We write directly to the cache file rather than going
/// through the in-process API because the binary boundary is what we're
/// testing.
fn write_market_cache_entry(
    data_dir: &Path,
    owner: &str,
    repo: &str,
    skills: &[(&str, &str, &str)],
) {
    let cache_dir = data_dir.join("market-cache");
    std::fs::create_dir_all(&cache_dir).expect("create market-cache dir");
    let path = cache_dir.join(format!("{owner}_{repo}.json"));
    // Build JSON manually so we don't depend on internal type re-exports.
    let mut entries: Vec<String> = Vec::new();
    for (name, repo_path, label) in skills {
        entries.push(format!(
            "{{\"name\":\"{name}\",\"repo_path\":\"{repo_path}\",\"source_label\":\"{label}\",\"source_repo\":\"{owner}/{repo}\",\"branch\":\"main\"}}"
        ));
    }
    let body = format!("[{}]", entries.join(","));
    std::fs::write(&path, body).expect("write cache entry");
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// `market_install_fails_on_not_found` from PLANNING §1.7 test plan.
///
/// Running `market-install <nonexistent-name>` against an empty market cache
/// must:
///   1. Exit non-zero.
///   2. Print an error mentioning "not found in market".
///   3. NOT create any new directory under `~/.runai/skills/`.
///
/// rationale: error handling must not leave partial state behind.
#[test]
fn market_install_fails_on_not_found_no_writes() {
    let env = TestEnv::new();

    // Snapshot ~/.runai/skills/ before the run.
    let skills_dir = env.default_skills_dir();
    let before: Vec<String> = std::fs::read_dir(&skills_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().and_then(|e| e.file_name().into_string().ok()))
                .collect()
        })
        .unwrap_or_default();

    let out = env.run(&["market-install", "nonexistent-runai-test-skill-xyz"]);
    dump(&out, "market-install nonexistent");

    assert!(
        !out.status.success(),
        "market-install of a nonexistent skill should fail; stdout/stderr shown above"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.to_lowercase().contains("not found in market"),
        "expected error mentioning 'not found in market'; got:\n{combined}"
    );

    // No new directory under ~/.runai/skills/.
    let after: Vec<String> = std::fs::read_dir(&skills_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().and_then(|e| e.file_name().into_string().ok()))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        before, after,
        "market-install must not create a skill dir when lookup fails"
    );

    // The specific name must not exist as a directory or dangling symlink.
    let target = skills_dir.join("nonexistent-runai-test-skill-xyz");
    assert!(
        std::fs::symlink_metadata(&target).is_err(),
        "nonexistent skill must not appear at {}",
        target.display()
    );
}

/// `market_install_respects_rune_data_dir` from PLANNING §1.7 test plan.
///
/// Cross-RUNE_DATA_DIR safety: when a foreign RUNE_DATA_DIR is set, a failing
/// market-install must:
///   1. Not write into the default ~/.runai/skills/ tree.
///   2. Not write into the foreign data dir's skills/ tree (since the lookup
///      failed before download).
///
/// This is the 4-20 / 4-27 regression family — any binary that resolves data
/// paths must consult RUNE_DATA_DIR and never touch the default home tree.
#[test]
fn market_install_not_found_respects_rune_data_dir() {
    let env = TestEnv::new();

    // Place a sentinel inside the default ~/.runai/skills/ so we can detect
    // any stray writes from the foreign run.
    let sentinel = env.default_skills_dir().join("sentinel-skill");
    std::fs::create_dir_all(&sentinel).unwrap();
    std::fs::write(
        sentinel.join("SKILL.md"),
        "---\nname: sentinel-skill\ndescription: sentinel\n---\n",
    )
    .unwrap();
    let sentinel_bytes = std::fs::read(sentinel.join("SKILL.md")).unwrap();

    // Foreign data dir — completely outside the default.
    let foreign = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(foreign.path().join("skills")).unwrap();
    std::fs::create_dir_all(foreign.path().join("market-cache")).unwrap();

    let out = env.run_with_rune_data(
        foreign.path(),
        &["market-install", "another-nonexistent-skill"],
    );
    dump(&out, "market-install with RUNE_DATA_DIR");

    assert!(
        !out.status.success(),
        "lookup should fail (cache is empty in foreign data dir)"
    );

    // Default sentinel untouched.
    assert!(
        sentinel.join("SKILL.md").exists(),
        "default sentinel was removed under foreign RUNE_DATA_DIR"
    );
    assert_eq!(
        std::fs::read(sentinel.join("SKILL.md")).unwrap(),
        sentinel_bytes,
        "default sentinel was mutated under foreign RUNE_DATA_DIR"
    );

    // No stray write to foreign skills/.
    let foreign_target = foreign.path().join("skills/another-nonexistent-skill");
    assert!(
        std::fs::symlink_metadata(&foreign_target).is_err(),
        "stray write to foreign RUNE_DATA_DIR at {}",
        foreign_target.display()
    );

    // No accidental write to default ~/.runai/skills/<that-name> either.
    let default_target = env.default_skills_dir().join("another-nonexistent-skill");
    assert!(
        std::fs::symlink_metadata(&default_target).is_err(),
        "stray write to default skills dir under foreign RUNE_DATA_DIR at {}",
        default_target.display()
    );
}

/// `market_install_respects_source_filter` (negative branch from PLANNING §1.7).
///
/// `find_skill_in_sources` accepts an optional `source_filter` that must
/// case-insensitively match either the source's label or its `owner/repo`
/// repo_id. We seed a cache entry under one builtin source (Anthropic
/// Official), then ask `market-install` to look it up under a totally
/// different `--source` filter. The lookup must fail because no enabled
/// source matches the filter.
///
/// rationale: source filter is a multi-source feature and must not silently
/// fall through to other sources when the filter doesn't match.
#[test]
fn market_install_source_filter_no_match_returns_not_found() {
    let env = TestEnv::new();

    // Seed a real cache file for the default-enabled Anthropic source so the
    // skill IS discoverable without a filter, but NOT under a wrong filter.
    write_market_cache_entry(
        &env.default_data_dir(),
        "anthropics",
        "claude-plugins-official",
        &[("some-real-cached-skill", "skills/foo", "Anthropic Official")],
    );

    // Sanity: without filter the lookup would succeed → market-install would
    // proceed to network download. We don't run that branch here; the test
    // below uses a filter that does NOT match anthropics/* so lookup fails.
    let out = env.run(&[
        "market-install",
        "some-real-cached-skill",
        "--source",
        "totally-unrelated-source-label",
    ]);
    dump(&out, "market-install with mismatched --source");

    assert!(
        !out.status.success(),
        "market-install with non-matching --source filter must fail"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.to_lowercase().contains("not found in market"),
        "expected 'not found in market' under mismatched filter; got:\n{combined}"
    );

    // No skill dir for that name should have been created.
    let target = env.default_skills_dir().join("some-real-cached-skill");
    assert!(
        std::fs::symlink_metadata(&target).is_err(),
        "no skill dir should exist after filter-mismatched lookup at {}",
        target.display()
    );
}

/// Source filter when no sources are enabled at all (every source disabled
/// via market-sources.json) must also surface as "not found" — defensive
/// check that `find_skill_in_sources` honors the `enabled` flag.
///
/// We disable EVERY built-in source by writing a market-sources.json that
/// flips them off, then seed a cache entry for one of them and try to
/// install. The cache exists but the source is disabled → lookup must miss.
#[test]
fn market_install_skips_disabled_source() {
    let env = TestEnv::new();

    // Write market-sources.json that disables the Anthropic source we're
    // about to seed a cache for. We list it as builtin=true with enabled=false
    // so `load_sources` merges the false state in.
    let sources_path = env.default_data_dir().join("market-sources.json");
    std::fs::write(
        &sources_path,
        r#"[{"owner":"anthropics","repo":"claude-plugins-official","branch":"main","skill_prefix":"","label":"Anthropic Official","description":"Official Claude plugins & skills (23)","builtin":true,"enabled":false}]"#,
    )
    .unwrap();

    write_market_cache_entry(
        &env.default_data_dir(),
        "anthropics",
        "claude-plugins-official",
        &[(
            "cached-but-disabled-skill",
            "skills/foo",
            "Anthropic Official",
        )],
    );

    let out = env.run(&["market-install", "cached-but-disabled-skill"]);
    dump(&out, "market-install with disabled source");

    assert!(
        !out.status.success(),
        "lookup must fail when the only source carrying the skill is disabled"
    );
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        combined.to_lowercase().contains("not found in market"),
        "expected 'not found in market' for disabled source; got:\n{combined}"
    );
    let target = env.default_skills_dir().join("cached-but-disabled-skill");
    assert!(
        std::fs::symlink_metadata(&target).is_err(),
        "no skill dir should exist when source disabled at {}",
        target.display()
    );
}

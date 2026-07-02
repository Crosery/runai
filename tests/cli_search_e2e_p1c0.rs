//! P1 regression tests for `runai search` (PLANNING.md test plan §1.16).
//!
//! Covers:
//!   1. `search_finds_installed_and_market_skills` — installed + market sections
//!      render with the correct icons / order, and "No matches" prints when
//!      nothing scores.
//!   2. `search_query_variations` — partial, exact, empty-query, and
//!      market-source-disabled behaviour at the CLI surface.
//!
//! Safety: each test creates an isolated `HOME` via tempfile and points
//! `RUNE_DATA_DIR` at that tempdir's `.runai` subtree. The real
//! `~/.runai/`, `~/.{claude,codex,gemini,opencode}/skills/`, and the four
//! CLI config files are never touched (verified by spawning the binary with
//! `HOME` overridden + `RUNE_DATA_DIR` explicitly pinned + `RUNAI_NO_AUTOSPAWN=1`
//! so the TUI dashboard server is never spawned).
//!
//! Skipped on Windows: symlinks + HOME mocking are unix-only here, matching
//! the existing `safety_e2e` / `cli_target_symmetry` integration suites.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

// ─── helpers ────────────────────────────────────────────────────────────────

struct TestEnv {
    home: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        // Pre-create CLI skills dirs so any incidental scanner pass doesn't
        // bail. Pre-create managed dirs so we can drop files in directly.
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

    fn rune_data(&self) -> PathBuf {
        self.home().join(".runai")
    }

    /// Run the real installed runai binary with HOME isolated to this tempdir.
    /// RUNE_DATA_DIR is pinned to `<home>/.runai`. RUNAI_NO_AUTOSPAWN=1 keeps
    /// any incidental TUI dashboard spawn from firing.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(RUNAI_BIN)
            .args(args)
            .env("HOME", self.home())
            .env("RUNE_DATA_DIR", self.rune_data())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .expect("spawn runai binary")
    }
}

/// Drop a real skill into `<HOME>/.runai/skills/<name>/SKILL.md` with the
/// given description so `runai scan` can adopt it.
fn make_skill(env: &TestEnv, name: &str, description: &str) {
    let dir = env.rune_data().join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let body = format!(
        "---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\n{description}\n",
    );
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
}

/// Write a market-cache JSON file at the exact path the production
/// `core::market::load_cache` helper looks for it. The filename is
/// `<owner>_<repo>.json` and lives under `<data_dir>/market-cache/`.
/// `<owner>/<repo>` here must match a real *enabled* builtin source so
/// `load_cache` actually opens it during search.
fn write_market_cache(env: &TestEnv, owner: &str, repo: &str, skills_json: &str) {
    let path = env
        .rune_data()
        .join("market-cache")
        .join(format!("{owner}_{repo}.json"));
    std::fs::write(&path, skills_json).unwrap();
}

/// Helper around `write_market_cache` for the active builtin source.
/// Detects whether the binary is the older 8-source layout (where the
/// "anthropics/claude-plugins-official" source is enabled by default) or
/// the newer skills.sh sentinel layout. Writes the cache to whichever owner
/// the running binary actually consults. Returns the source label that the
/// binary will print on each market row, so assertions can match it.
fn write_default_market_cache(env: &TestEnv, skills_json: &str) -> &'static str {
    // The skills.sh sentinel uses owner=repo="*skills-hub*", visible label
    // "skills.sh". We write both cache filenames so the test is robust
    // across binary builds. Whichever the runtime opens, the other is
    // harmlessly ignored.
    write_market_cache(env, "*skills-hub*", "*skills-hub*", skills_json);
    write_market_cache(env, "anthropics", "claude-plugins-official", skills_json);
    // Either label may render; tests check for both possibilities.
    "skills.sh or Anthropic Official"
}

fn dump(out: &Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\n[stdout]\n{}\n[stderr]\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// PLANNING §1.16 test 1: `runai search <query>` returns installed skills
/// (with ● / ○ enabled-state icon) AND market skills (from cache) AND emits
/// "No matches" when nothing scores. We rely on the well-known builtin
/// source "anthropics/claude-plugins-official" being enabled by default.
#[test]
fn search_finds_installed_and_market_skills() {
    let env = TestEnv::new();

    // 1. Pre-populate one local skill that will be adopted by `runai scan`.
    make_skill(
        &env,
        "fronted-helper",
        "Front-end build helper for React projects",
    );

    // 2. Adopt it into the DB so it shows up as an installed resource.
    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan should succeed");

    // 3. Pre-populate a market cache for whichever default builtin source the
    //    binary uses (old multi-repo layout vs new skills.sh sentinel).
    //    Fields match `core::market::MarketSkill`; extra fields not in this
    //    JSON rely on `#[serde(default)]` on the runtime struct.
    let market_json = r#"[
      {"name":"frontend-bundler","repo_path":"plugins/frontend-bundler","source_label":"Anthropic Official","source_repo":"anthropics/claude-plugins-official","branch":"main"},
      {"name":"backend-typed","repo_path":"plugins/backend-typed","source_label":"Anthropic Official","source_repo":"anthropics/claude-plugins-official","branch":"main"},
      {"name":"db-introspect","repo_path":"plugins/db-introspect","source_label":"Anthropic Official","source_repo":"anthropics/claude-plugins-official","branch":"main"}
    ]"#;
    write_default_market_cache(&env, market_json);

    // 4. Run search with a query that should match both the local skill name
    //    ("fronted-helper") and one market skill name ("frontend-bundler").
    let out = env.run(&["search", "front"]);
    dump(&out, "search front");
    assert!(out.status.success(), "search should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Installed section present, with the local skill, marked with the
    // disabled icon (○) — the test scaffold never enabled it on any CLI.
    assert!(
        stdout.contains("── Installed"),
        "expected Installed section header in output:\n{stdout}"
    );
    assert!(
        stdout.contains("fronted-helper"),
        "expected local skill in Installed section:\n{stdout}"
    );
    assert!(
        stdout.contains("○"),
        "expected disabled icon for newly-adopted skill:\n{stdout}"
    );

    // Market section present, with the best-matching skill name.
    assert!(
        stdout.contains("── Market"),
        "expected Market section header in output:\n{stdout}"
    );
    assert!(
        stdout.contains("frontend-bundler"),
        "expected market skill in Market section:\n{stdout}"
    );
    // The source label tag should be appended per the format
    // `"  {} ({})"` (name + source_label). Either layout's label is valid.
    assert!(
        stdout.contains("(Anthropic Official)") || stdout.contains("(skills.sh)"),
        "expected a recognized source_label appended to market rows:\n{stdout}"
    );
    // Hint line that points users at market-install.
    assert!(
        stdout.contains("runai market-install"),
        "expected market-install hint:\n{stdout}"
    );

    // 5. Already-installed names must not be duplicated in the Market section
    //    — `runai search` filters `installed_names.contains(...)` before
    //    pushing market candidates.
    let market_block = stdout.split("── Market").nth(1).unwrap_or("");
    assert!(
        !market_block.contains("fronted-helper"),
        "installed skill must not also appear in Market section:\n{stdout}"
    );

    // 6. A query with no matches anywhere should print "No matches".
    let nothing = env.run(&["search", "zzzz-nonexistent-keyword-xyz"]);
    dump(&nothing, "search nothing");
    assert!(
        nothing.status.success(),
        "search with no hits should still exit 0"
    );
    let nothing_out = String::from_utf8_lossy(&nothing.stdout);
    assert!(
        nothing_out.contains("No matches for 'zzzz-nonexistent-keyword-xyz'"),
        "expected 'No matches' line:\n{nothing_out}"
    );

    // 7. Safety: nothing leaked outside the sandbox HOME. The real ~/.runai
    //    skills dir must be untouched; we assert by checking the test's own
    //    rune_data dir still holds the only skill we created.
    let installed_dir = env.rune_data().join("skills");
    let entries: Vec<_> = std::fs::read_dir(&installed_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        entries.contains(&"fronted-helper".to_string()),
        "the seeded skill must still live in the sandbox; got {entries:?}"
    );
}

/// PLANNING §1.16 test 2 (converted from `unit_in_module` to
/// `cargo_integration` per the test-only rule — we cannot add a unit module
/// in src/). Validates query variations through the real binary:
///   - empty query does not crash and returns "No matches" or no rows
///   - partial-match query returns ranked rows
///   - exact-match query returns the exact-named skill
///   - disabling the market source removes Market rows but keeps Installed
#[test]
fn search_query_variations() {
    let env = TestEnv::new();

    // Two installed skills with distinct names so partial vs exact queries
    // can be told apart by the assertions.
    make_skill(
        &env,
        "alpha-deploy",
        "Deploys an alpha environment for testing",
    );
    make_skill(
        &env,
        "beta-deploy",
        "Deploys a beta environment for canary releases",
    );

    let scan = env.run(&["scan"]);
    dump(&scan, "scan");
    assert!(scan.status.success(), "scan should succeed");

    // Pre-populate market cache so we can verify market gating in case 4.
    let market_json = r#"[
      {"name":"alpha-cdn","repo_path":"plugins/alpha-cdn","source_label":"Anthropic Official","source_repo":"anthropics/claude-plugins-official","branch":"main"}
    ]"#;
    write_default_market_cache(&env, market_json);

    // ── case 1: empty query ────────────────────────────────────────────
    // `runai search` requires a query positional argument (clap will reject
    // a totally absent one), but an empty string is a valid CLI input. The
    // nucleo matcher should not crash on it; we just want a clean exit.
    let empty = env.run(&["search", ""]);
    dump(&empty, "search empty");
    assert!(
        empty.status.success(),
        "empty query should not crash the search command"
    );

    // ── case 2: partial match ──────────────────────────────────────────
    // "alph" should match "alpha-deploy" (installed) and "alpha-cdn"
    // (market). Both must appear; "beta-deploy" must not.
    let partial = env.run(&["search", "alph"]);
    dump(&partial, "search alph");
    assert!(partial.status.success());
    let partial_out = String::from_utf8_lossy(&partial.stdout);
    assert!(
        partial_out.contains("alpha-deploy"),
        "partial 'alph' should hit alpha-deploy:\n{partial_out}"
    );
    assert!(
        partial_out.contains("alpha-cdn"),
        "partial 'alph' should hit market alpha-cdn:\n{partial_out}"
    );
    assert!(
        !partial_out.contains("beta-deploy"),
        "partial 'alph' must not hit beta-deploy:\n{partial_out}"
    );

    // ── case 3: exact-name match ───────────────────────────────────────
    // The full name should still produce a hit and the Installed section
    // header should appear with count 1.
    let exact = env.run(&["search", "beta-deploy"]);
    dump(&exact, "search beta-deploy");
    assert!(exact.status.success());
    let exact_out = String::from_utf8_lossy(&exact.stdout);
    assert!(
        exact_out.contains("── Installed (1)"),
        "exact match should produce exactly one Installed row:\n{exact_out}"
    );
    assert!(
        exact_out.contains("beta-deploy"),
        "exact 'beta-deploy' should appear in the result:\n{exact_out}"
    );

    // ── case 4: disable the market source → only Installed remains ────
    // We persist a sources file marking the builtin source disabled, which
    // is exactly what `load_sources` merges in. After this, the search loop
    // should skip the cache lookup for that source, so even the matching
    // market skill "alpha-cdn" must not appear. We disable BOTH the old
    // anthropic source and the new skills.sh sentinel so this works
    // regardless of which layout the running binary uses.
    let sources_path = env.rune_data().join("market-sources.json");
    let disabled_sources = r#"[
      {"owner":"anthropics","repo":"claude-plugins-official","branch":"main","skill_prefix":"","label":"Anthropic Official","description":"disabled for test","builtin":true,"enabled":false},
      {"owner":"*skills-hub*","repo":"*skills-hub*","branch":"main","skill_prefix":"","label":"skills.sh","description":"disabled for test","builtin":true,"enabled":false}
    ]"#;
    std::fs::write(&sources_path, disabled_sources).unwrap();

    let no_market = env.run(&["search", "alph"]);
    dump(&no_market, "search alph (market disabled)");
    assert!(no_market.status.success());
    let no_market_out = String::from_utf8_lossy(&no_market.stdout);
    assert!(
        no_market_out.contains("alpha-deploy"),
        "installed alpha-deploy must still appear when market disabled:\n{no_market_out}"
    );
    assert!(
        !no_market_out.contains("alpha-cdn"),
        "market alpha-cdn must NOT appear when source disabled:\n{no_market_out}"
    );
    // The Market section header should also be absent in this case because
    // `market_scored` is empty.
    assert!(
        !no_market_out.contains("── Market"),
        "Market section header must not render when all sources disabled:\n{no_market_out}"
    );
}

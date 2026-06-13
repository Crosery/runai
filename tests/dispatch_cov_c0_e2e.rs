//! Coverage e2e for `runai market` CLI subcommand (dispatch.rs / cli/mod.rs).
//!
//! The `Market` arm in `cli::mod::run()`:
//!   * loads enabled sources via `core::market::load_sources(data_dir)`
//!   * for each source loads cached skills via `core::market::load_cache`
//!   * applies optional `--source <filter>` (matches against `label` or `repo_id`)
//!   * applies optional `--search <query>` (fuzzy across name / repo_path / source_label)
//!   * marks installed skills with `●`, uninstalled with `○`
//!   * prints "Total: N skills" or "No market skills matched."
//!
//! Tests stage a fresh cache file under `<HOME>/.runai/market-cache/<owner>_<repo>.json`
//! that matches an existing builtin source's cache_key, then spawns the real
//! `runai` binary and asserts on stdout. Each test uses an isolated tempdir
//! HOME and `RUNAI_NO_AUTOSPAWN=1` to keep the dashboard server out of the way.
//!
//! Skipped on Windows: `HOME` env override doesn't work there
//! (see AGENTS.md "Key constraints" — dirs::home_dir uses SHGetKnownFolderPath).
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
        let home = tempfile::tempdir().expect("create tmp HOME");
        for cli in ["claude", "codex", "gemini", "opencode"] {
            std::fs::create_dir_all(home.path().join(format!(".{cli}/skills"))).unwrap();
        }
        std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
        std::fs::create_dir_all(home.path().join(".runai/market-cache")).unwrap();
        Self { home }
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn data_dir(&self) -> PathBuf {
        self.home().join(".runai")
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
}

/// Write a market-cache JSON file for the given `(owner, repo, label)` source.
/// File path mirrors `core::market::cache_key` → `<owner>_<repo>.json`.
/// The cache only matters if the corresponding `SourceEntry` exists in
/// `load_sources()` and is `enabled` — that's why these tests target the two
/// builtin sources that ship enabled-by-default (`anthropics/claude-plugins-official`
/// and `affaan-m/everything-claude-code`).
fn write_cache(data_dir: &Path, owner: &str, repo: &str, label: &str, skills: &[(&str, &str)]) {
    let cache_dir = data_dir.join("market-cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let path = cache_dir.join(format!("{owner}_{repo}.json"));
    let json: Vec<serde_json::Value> = skills
        .iter()
        .map(|(name, repo_path)| {
            serde_json::json!({
                "name": name,
                "repo_path": repo_path,
                "source_label": label,
                "source_repo": format!("{owner}/{repo}"),
                "branch": "main",
            })
        })
        .collect();
    std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();
}

fn install_local_skill(data_dir: &Path, name: &str) {
    // Drop a SKILL.md into <data>/skills/<name>/ then `runai scan` to adopt it
    // — that's the simplest way to materialize a row in `resources` with the
    // exact name we want to test "● installed" rendering on.
    let dir = data_dir.join("skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: local test skill\n---\n\n# {name}\n"),
    )
    .unwrap();
}

fn dump(out: &std::process::Output, label: &str) {
    eprintln!(
        "--- {label} (exit={}) ---\nSTDOUT:\n{}\nSTDERR:\n{}\n--- end ---",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

// ─── scenario 1: empty market (no cache files) ────────────────────────────────

#[test]
fn market_handles_empty() {
    let env = TestEnv::new();
    // No cache files written → load_cache returns None for every source →
    // rows stays empty → dispatcher prints "No market skills matched."
    let out = env.run(&["market"]);
    dump(&out, "market_handles_empty");
    assert!(out.status.success(), "market should exit 0 on empty cache");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No market skills matched."),
        "expected empty-cache sentinel, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("Total:"),
        "should not print Total: when empty"
    );
}

// ─── scenario 2: market lists all cached skills ──────────────────────────────

#[test]
fn market_lists_all_skills() {
    let env = TestEnv::new();
    // Stage cache for the two builtins enabled by default.
    write_cache(
        &env.data_dir(),
        "anthropics",
        "claude-plugins-official",
        "Anthropic Official",
        &[("alpha-skill", "alpha-skill"), ("beta-skill", "beta-skill")],
    );
    write_cache(
        &env.data_dir(),
        "affaan-m",
        "everything-claude-code",
        "Everything Claude Code",
        &[("gamma-skill", "skills/gamma-skill")],
    );
    let out = env.run(&["market"]);
    dump(&out, "market_lists_all_skills");
    assert!(out.status.success(), "market should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    for name in ["alpha-skill", "beta-skill", "gamma-skill"] {
        assert!(stdout.contains(name), "expected `{name}` in:\n{stdout}");
    }
    assert!(
        stdout.contains("Total: 3 skills"),
        "expected `Total: 3 skills`, got:\n{stdout}"
    );
    // Every row starts with the not-installed `○` glyph since nothing is in DB.
    assert!(
        stdout.contains("○ alpha-skill"),
        "expected ○ before alpha-skill, got:\n{stdout}"
    );
}

// ─── scenario 3: --source filter narrows results ─────────────────────────────

#[test]
fn market_filters_by_source() {
    let env = TestEnv::new();
    write_cache(
        &env.data_dir(),
        "anthropics",
        "claude-plugins-official",
        "Anthropic Official",
        &[("anthro-only", "anthro-only")],
    );
    write_cache(
        &env.data_dir(),
        "affaan-m",
        "everything-claude-code",
        "Everything Claude Code",
        &[("everything-only", "skills/everything-only")],
    );

    // Filter by label substring (case-insensitive).
    let out = env.run(&["market", "--source", "Anthropic"]);
    dump(&out, "market_filters_by_source label");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("anthro-only"),
        "anthro skill should appear:\n{stdout}"
    );
    assert!(
        !stdout.contains("everything-only"),
        "everything skill should be filtered out:\n{stdout}"
    );
    assert!(
        stdout.contains("Total: 1 skills"),
        "expected exactly 1 row, got:\n{stdout}"
    );

    // Filter by repo_id substring (`owner/repo`) — `affaan-m/everything-claude-code`.
    let out = env.run(&["market", "--source", "affaan-m/everything-claude-code"]);
    dump(&out, "market_filters_by_source repo_id");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("everything-only"),
        "everything skill should appear:\n{stdout}"
    );
    assert!(
        !stdout.contains("anthro-only"),
        "anthro skill should be filtered out:\n{stdout}"
    );
}

// ─── scenario 4: --search fuzzy-matches and ranks ────────────────────────────

#[test]
fn market_filters_by_query() {
    let env = TestEnv::new();
    write_cache(
        &env.data_dir(),
        "anthropics",
        "claude-plugins-official",
        "Anthropic Official",
        &[
            ("pdf-extractor", "pdf-extractor"),
            ("docx-formatter", "docx-formatter"),
            ("brainstorm-helper", "brainstorm-helper"),
        ],
    );
    let out = env.run(&["market", "--search", "pdf"]);
    dump(&out, "market_filters_by_query");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("pdf-extractor"),
        "pdf-extractor should match `pdf`:\n{stdout}"
    );
    // Nothing else has "pdf" anywhere in name / repo_path / source_label.
    assert!(
        !stdout.contains("docx-formatter"),
        "docx-formatter should NOT match `pdf`:\n{stdout}"
    );
    assert!(
        !stdout.contains("brainstorm-helper"),
        "brainstorm-helper should NOT match `pdf`:\n{stdout}"
    );

    // Garbage query returns the "No market skills matched." sentinel.
    let out = env.run(&["market", "--search", "zzz-no-such-skill-zzz"]);
    dump(&out, "market_filters_by_query empty");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("No market skills matched."),
        "expected empty sentinel, got:\n{stdout}"
    );
}

// ─── scenario 5: installed skills get marked with `●` ────────────────────────

#[test]
fn market_marks_installed() {
    let env = TestEnv::new();
    // Stage a local skill on disk under <data>/skills/<name>/ and let `scan`
    // adopt it into the DB → list_resources() will then include this name.
    install_local_skill(&env.data_dir(), "shared-name");

    let scan = env.run(&["scan"]);
    dump(&scan, "scan adopt");
    assert!(scan.status.success(), "scan should succeed");

    // Cache a market skill that shares the same name.
    write_cache(
        &env.data_dir(),
        "anthropics",
        "claude-plugins-official",
        "Anthropic Official",
        &[
            ("shared-name", "shared-name"),
            ("unique-name", "unique-name"),
        ],
    );

    let out = env.run(&["market"]);
    dump(&out, "market_marks_installed");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("● shared-name"),
        "shared-name should be marked installed (●):\n{stdout}"
    );
    assert!(
        stdout.contains("○ unique-name"),
        "unique-name should be marked not-installed (○):\n{stdout}"
    );
    assert!(
        stdout.contains("Total: 2 skills"),
        "expected 2 rows, got:\n{stdout}"
    );
}

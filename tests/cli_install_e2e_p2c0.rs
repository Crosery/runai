//! P2 e2e regression tests for `runai install` (PLANNING test plan §1.6).
//!
//! These exercise the install entrypoint in `src/cli/mod.rs` (`Some(Commands::Install { .. })`)
//! via the real `runai` binary spawned in an isolated HOME so they never touch
//! the real `~/.runai/` or `~/.{claude,codex,gemini,opencode}/skills/`.
//!
//! The plan's "happy-path" install test (`install_clones_and_registers_github_skills`)
//! needs live GitHub access — that flow already has a manual coverage point in
//! `tests/install_test.rs::test_real_install_minimax` (gated with `#[ignore]`).
//! The tests here focus on what can be asserted without a working network:
//! input parsing, URL/branch handling, and "fails-without-pollution" contracts.
//!
//! Skipped on Windows: symlinks require Developer Mode / Admin and the rest of
//! the integration suite is gated the same way.
#![cfg(not(target_os = "windows"))]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Path to the workspace-built runai binary (produced by `cargo build`/`cargo test`).
fn runai_bin() -> &'static str {
    env!("CARGO_BIN_EXE_runai")
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

    fn data_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }

    fn skills_dir(&self) -> PathBuf {
        self.data_dir().join("skills")
    }

    fn cli_skills_dir(&self, cli: &str) -> PathBuf {
        self.home().join(format!(".{cli}/skills"))
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(runai_bin())
            .args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.data_dir())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .expect("runai binary spawn")
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

fn count_dir_entries(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|r| r.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

// ─── tests ──────────────────────────────────────────────────────────────────

/// `runai install` rejects malformed sources (missing slash). Per the cli
/// dispatch handler in `src/cli/mod.rs`, only `owner/repo` and `owner/repo@branch`
/// formats are accepted; other strings should bail without touching disk.
#[test]
fn install_rejects_malformed_source_without_touching_disk() {
    let env = TestEnv::new();

    // Snapshot the managed skills dir before the bad call.
    let before = count_dir_entries(&env.skills_dir());

    let out = env.run(&["install", "notanowner"]);
    dump(&out, "install notanowner");

    assert!(
        !out.status.success(),
        "install with malformed source should fail, got success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stdout}\n{stderr}");
    assert!(
        combined.to_lowercase().contains("invalid format")
            || combined.to_lowercase().contains("owner/repo"),
        "expected an 'invalid format' / 'owner/repo' hint, got:\n{combined}"
    );

    // Managed skills directory must be untouched.
    let after = count_dir_entries(&env.skills_dir());
    assert_eq!(
        before, after,
        "install with malformed source must not write to ~/.runai/skills (before={before}, after={after})"
    );

    // CLI skills directories must also be empty.
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let dir = env.cli_skills_dir(cli);
        assert_eq!(
            count_dir_entries(&dir),
            0,
            "install with malformed source must not write CLI symlinks ({cli})"
        );
    }
}

/// `runai install <owner>/<nonexistent-repo>` must fail gracefully and not
/// leave any partial skills in the managed directory. This is the plan's
/// `install_fails_gracefully_on_network_error` contract — what matters is the
/// transactional / "no partial state" guarantee, not the specific exit code.
///
/// This test requires DNS + GitHub HTTPS. If the env is offline, the request
/// fails at the transport layer; in both cases the install must error and
/// leave `~/.runai/skills/` empty.
#[test]
fn install_fails_gracefully_on_nonexistent_repo() {
    let env = TestEnv::new();

    let before = count_dir_entries(&env.skills_dir());

    // Use a clearly-nonexistent repo path so even an upstream DNS-OK GitHub
    // call returns a 404 response (the `fetch` helper bails on non-2xx).
    let out = env.run(&[
        "install",
        "runai-tests-nonexistent-owner/this-repo-will-never-exist-xyz-12345",
    ]);
    dump(&out, "install nonexistent repo");

    assert!(
        !out.status.success(),
        "install of a nonexistent repo must fail"
    );

    // No partial skill files should be left behind.
    let after = count_dir_entries(&env.skills_dir());
    assert_eq!(
        before, after,
        "failed install must not leave skills behind (before={before}, after={after})"
    );

    // No symlinks created in any CLI target.
    for cli in ["claude", "codex", "gemini", "opencode"] {
        let dir = env.cli_skills_dir(cli);
        assert_eq!(
            count_dir_entries(&dir),
            0,
            "failed install must not create CLI symlinks ({cli})"
        );
    }
}

/// `runai install https://github.com/<owner>/<repo>` must accept the full
/// GitHub URL form (per plan §1.6 test 3). We verify the URL is unwrapped by
/// using a nonexistent repo: the parse path strips `https://github.com/` and
/// `owner/repo` gets to the fetcher, which then 404s. If parsing rejected the
/// URL up front, the error wording would mention `Invalid format` /
/// `owner/repo`; instead we should see a GitHub-API style error (e.g. 404).
#[test]
fn install_accepts_full_github_url_format() {
    let env = TestEnv::new();

    let before = count_dir_entries(&env.skills_dir());

    let out = env.run(&[
        "install",
        "https://github.com/runai-tests-nonexistent-owner/this-repo-will-never-exist-xyz-12345",
    ]);
    dump(&out, "install full GitHub URL");

    // Should fail (repo doesn't exist) but not because of parse rejection.
    assert!(
        !out.status.success(),
        "install of nonexistent URL repo must fail"
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{stdout}\n{stderr}").to_lowercase();

    // If parsing accepted the URL, the failure must be downstream (network /
    // GitHub API) — NOT "invalid format". The stdout begins with
    // "Installing from <owner>/<repo>@main..." when parsing succeeded.
    assert!(
        !combined.contains("invalid format"),
        "full GitHub URL was rejected at parse stage — full URL form is broken:\n{combined}"
    );
    assert!(
        stdout.contains("Installing from")
            || combined.contains("github api")
            || combined.contains("404")
            || combined.contains("not found")
            || combined.contains("dns")
            || combined.contains("error sending request"),
        "expected a downstream (post-parse) error, got:\n{combined}"
    );

    // No partial skills.
    let after = count_dir_entries(&env.skills_dir());
    assert_eq!(
        before, after,
        "failed install via URL form must not leave skills (before={before}, after={after})"
    );
}

/// `runai install <owner>/<repo>@<branch>` must propagate the branch through
/// to the fetcher (plan §1.6 test 2). We can't observe the literal branch
/// without intercepting the HTTP call, but we can confirm:
///   1. branch parsing accepts `@dev` and doesn't fall back to "Invalid format"
///   2. failures are still transactional (no partial skills)
///   3. the printed pre-flight line includes the branch name we passed
#[test]
fn install_respects_branch_parameter_in_parse() {
    let env = TestEnv::new();

    let before = count_dir_entries(&env.skills_dir());

    let out = env.run(&[
        "install",
        "runai-tests-nonexistent-owner/this-repo-will-never-exist-xyz-12345@dev",
    ]);
    dump(&out, "install owner/repo@dev");

    assert!(
        !out.status.success(),
        "install of nonexistent repo on @dev must fail"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}\n{stderr}").to_lowercase();

    assert!(
        !combined.contains("invalid format"),
        "owner/repo@branch form was rejected at parse stage:\n{combined}"
    );
    // The handler prints "Installing from owner/repo@<branch>...". This is
    // the only externally-visible evidence that the branch made it through
    // the parse stage.
    assert!(
        stdout.contains("@dev"),
        "stdout should mention the parsed branch '@dev', got:\n{stdout}"
    );

    // Failure transactional: no partial skills.
    let after = count_dir_entries(&env.skills_dir());
    assert_eq!(
        before, after,
        "failed @dev install must not leave skills (before={before}, after={after})"
    );
}

/// `runai install` must respect `RUNE_DATA_DIR` (plan §1.6 test 5 — cross-data-dir
/// safety). Even on a failed install, the failure should be scoped to the
/// custom data dir; the user's default `~/.runai/skills/` must be untouched.
#[test]
fn install_respects_rune_data_dir_on_failure() {
    let env = TestEnv::new();

    // Plant a sentinel in the *default* home location to confirm it's not
    // disturbed when install runs against a *non-default* RUNE_DATA_DIR.
    let default_sentinel_dir = env.skills_dir().join("default-sentinel");
    std::fs::create_dir_all(&default_sentinel_dir).unwrap();
    std::fs::write(
        default_sentinel_dir.join("SKILL.md"),
        "---\nname: default-sentinel\ndescription: should not move\n---\n",
    )
    .unwrap();
    let sentinel_md = default_sentinel_dir.join("SKILL.md");
    let original = std::fs::read(&sentinel_md).unwrap();

    // Now run install against a separate RUNE_DATA_DIR; it should fail
    // (nonexistent repo), and crucially must not perturb the default tree.
    let custom = tempfile::tempdir().unwrap();
    let out = Command::new(runai_bin())
        .args([
            "install",
            "runai-tests-nonexistent-owner/this-repo-will-never-exist-xyz-12345",
        ])
        .env("HOME", env.home())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", custom.path())
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .output()
        .expect("runai binary spawn");
    dump(&out, "install with RUNE_DATA_DIR set");

    assert!(!out.status.success(), "install must still fail");

    // The default sentinel must survive untouched (this is the 4-20/4-27 root
    // cause class: a non-default data dir must never reach back into ~/.runai/skills/).
    assert!(
        sentinel_md.exists(),
        "REGRESSION: install with custom RUNE_DATA_DIR removed default sentinel"
    );
    assert_eq!(
        std::fs::read(&sentinel_md).unwrap(),
        original,
        "REGRESSION: install with custom RUNE_DATA_DIR mutated default sentinel"
    );

    // The custom data dir's skills subtree (if any) must not contain partial
    // download leftovers either.
    let custom_skills = custom.path().join("skills");
    let custom_count = count_dir_entries(&custom_skills);
    assert_eq!(
        custom_count, 0,
        "failed install must not leave partial skills in custom RUNE_DATA_DIR"
    );
}

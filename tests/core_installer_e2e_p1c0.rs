//! P1 regression tests for `core::installer`.
//!
//! Covers `Installer::parse_github_source` input gate — the first thing every
//! GitHub install touches. The other plan tests (extract-finds-nested,
//! overwrite-existing, HTTP-error-bubble) need a mockable HTTP layer; the
//! current `install_from_github` calls `reqwest::get(&url)` against a
//! hard-coded `https://github.com/...` archive URL with no DI seam, so they
//! can't be exercised without source changes (forbidden under the P1 rules)
//! or live network (flaky in CI). Those are skipped at the workflow layer.
#![cfg(not(target_os = "windows"))]

use runai::core::installer::Installer;

/// Test 1 from the plan: parse_github_source accepts owner/repo,
/// owner/repo@branch, full GitHub URLs (with and without trailing slash and
/// optional @branch suffix); defaults to `main`; rejects malformed inputs.
#[test]
fn installer_parse_github_source_accepts_formats() {
    // Plain owner/repo defaults to main.
    let (owner, repo, branch) = Installer::parse_github_source("owner/repo").expect("plain");
    assert_eq!(owner, "owner");
    assert_eq!(repo, "repo");
    assert_eq!(branch, "main");

    // owner/repo@custom selects the custom branch.
    let (owner, repo, branch) =
        Installer::parse_github_source("owner/repo@custom-branch").expect("custom branch");
    assert_eq!(owner, "owner");
    assert_eq!(repo, "repo");
    assert_eq!(branch, "custom-branch");

    // Full GitHub URL is accepted, host stripped, trailing slash tolerated,
    // defaults to main.
    let (owner, repo, branch) =
        Installer::parse_github_source("https://github.com/owner/repo/").expect("full URL");
    assert_eq!(owner, "owner");
    assert_eq!(repo, "repo");
    assert_eq!(branch, "main");

    // Full GitHub URL without trailing slash also works.
    let (owner, repo, branch) =
        Installer::parse_github_source("https://github.com/owner/repo").expect("URL no slash");
    assert_eq!(owner, "owner");
    assert_eq!(repo, "repo");
    assert_eq!(branch, "main");

    // Full GitHub URL with @branch suffix — current impl strips the trailing
    // slash *before* splitting on '@', so the branch is parsed correctly.
    let (owner, repo, branch) =
        Installer::parse_github_source("https://github.com/owner/repo@dev").expect("URL @branch");
    assert_eq!(owner, "owner");
    assert_eq!(repo, "repo");
    assert_eq!(branch, "dev");

    // Bare owner with no repo segment must bail (splitn on '/' yields len 1).
    let err = Installer::parse_github_source("owner").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("invalid GitHub source"),
        "expected 'invalid GitHub source' in error, got: {msg}"
    );

    // Empty input — same failure mode (no slash separator).
    let err = Installer::parse_github_source("").unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("invalid GitHub source"),
        "expected 'invalid GitHub source' for empty input, got: {msg}"
    );
}

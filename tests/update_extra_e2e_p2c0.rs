//! P2 regression tests for `core::updater` (PLANNING test plan §5.30).
//!
//! Covers the platform asset-name mapping, the 24h cache cooldown semantics,
//! and that `current_version()` is sourced from the compile-time
//! `CARGO_PKG_VERSION`, not from the on-disk update-check cache.
//!
//! Binary replacement (#4 in the plan) is not exercised here — exercising
//! `perform_update` for real requires a mocked GitHub release endpoint with a
//! checksums file and either a tar.gz or zip payload; that's broader than a
//! unit/integration test and is owned by the P0 `update_binary_safety_e2e`
//! plan entry, not this P2 chunk.

#![cfg(not(target_os = "windows"))]

use runai::core::updater::{
    UpdateCache, asset_name, current_version, read_cache, should_check, write_cache,
};

// ── §5.30 #1 asset_name_platform_mapping ────────────────────────────────────

#[test]
fn asset_name_platform_mapping_matches_release_yml() {
    // Linux
    assert_eq!(
        asset_name("linux", "x86_64").as_deref(),
        Some("runai-linux-amd64.tar.gz")
    );
    assert_eq!(
        asset_name("linux", "aarch64").as_deref(),
        Some("runai-linux-arm64.tar.gz")
    );

    // macOS — must be "darwin" in asset name, NOT "macos"
    let macos_x86 = asset_name("macos", "x86_64");
    assert_eq!(macos_x86.as_deref(), Some("runai-darwin-amd64.tar.gz"));
    assert!(
        !macos_x86.as_deref().unwrap().contains("macos-"),
        "macOS asset must use 'darwin' not 'macos': {macos_x86:?}"
    );
    assert_eq!(
        asset_name("macos", "aarch64").as_deref(),
        Some("runai-darwin-arm64.tar.gz")
    );

    // Windows — must be .zip, not .tar.gz
    let win_x86 = asset_name("windows", "x86_64");
    assert_eq!(win_x86.as_deref(), Some("runai-windows-amd64.zip"));
    assert!(
        win_x86.as_deref().unwrap().ends_with(".zip"),
        "windows asset must be a .zip, not a .tar.gz: {win_x86:?}"
    );

    // Unsupported combos must return None (not a default / placeholder)
    assert_eq!(asset_name("freebsd", "x86_64"), None);
    assert_eq!(asset_name("linux", "riscv64"), None);
    assert_eq!(asset_name("openbsd", "aarch64"), None);
    assert_eq!(asset_name("solaris", "x86_64"), None);
    assert_eq!(asset_name("", ""), None);
}

// ── §5.30 #2 update_cache_cooldown_24h ──────────────────────────────────────

#[test]
fn update_cache_writes_and_is_used_within_24h() {
    let tmp = tempfile::tempdir().expect("create tmp data dir");

    // Initially no cache => should_check returns true.
    assert!(
        should_check(tmp.path()),
        "should_check must be true when no cache file exists"
    );

    // Write a fresh cache (checked_at = now).
    let now = chrono::Utc::now();
    let cache = UpdateCache {
        latest_version: "0.99.0".to_string(),
        current_version: "0.7.0".to_string(),
        download_url: "https://example.invalid/x.tar.gz".to_string(),
        checksum_url: "https://example.invalid/checksums.txt".to_string(),
        checked_at: now,
    };
    write_cache(tmp.path(), &cache).expect("write_cache succeeds");

    // The file must exist on disk at <data_dir>/update-check.json.
    let cache_path = tmp.path().join("update-check.json");
    assert!(
        cache_path.exists(),
        "update-check.json must be written under data_dir: {}",
        cache_path.display()
    );

    // Read back: round-trip preserves checked_at and version fields.
    let loaded = read_cache(tmp.path()).expect("read_cache after write");
    assert_eq!(loaded.latest_version, "0.99.0");
    assert_eq!(loaded.current_version, "0.7.0");
    // checked_at survives JSON serde — same instant to second precision.
    assert_eq!(loaded.checked_at.timestamp(), now.timestamp());

    // Within 24h, should_check returns false (cooldown is active).
    assert!(
        !should_check(tmp.path()),
        "should_check must be false when cache is fresher than 24h"
    );

    // Stale cache (older than 24h) re-arms should_check.
    let stale = UpdateCache {
        latest_version: "0.99.0".to_string(),
        current_version: "0.7.0".to_string(),
        download_url: String::new(),
        checksum_url: String::new(),
        checked_at: now - chrono::Duration::hours(25),
    };
    write_cache(tmp.path(), &stale).expect("overwrite cache");
    assert!(
        should_check(tmp.path()),
        "should_check must be true again when cache is older than 24h"
    );
}

// ── §5.30 #3 current_version_from_cargo ─────────────────────────────────────

#[test]
fn current_version_matches_cargo_pkg_version() {
    let v = current_version();
    let from_env = env!("CARGO_PKG_VERSION");
    let parsed = semver::Version::parse(from_env).expect("CARGO_PKG_VERSION is valid semver");

    // current_version() reads CARGO_PKG_VERSION at compile time, so the in-process
    // value must equal the env var value byte-for-byte.
    assert_eq!(
        v, parsed,
        "current_version() must equal CARGO_PKG_VERSION ({from_env}), got {v}"
    );

    // It must not be the zero version — the binary always has a real version.
    assert_ne!(v, semver::Version::new(0, 0, 0));

    // current_version() must NOT pull from the on-disk cache: writing a bogus
    // current_version to the cache must not change what current_version() returns.
    let tmp = tempfile::tempdir().expect("create tmp data dir");
    let bogus = UpdateCache {
        latest_version: "999.999.999".to_string(),
        current_version: "999.999.999".to_string(),
        download_url: String::new(),
        checksum_url: String::new(),
        checked_at: chrono::Utc::now(),
    };
    write_cache(tmp.path(), &bogus).expect("write bogus cache");

    // current_version is compile-time only — there is no `current_version(data_dir)`
    // overload, so even when the cache says 999.999.999 the function is unchanged.
    let after = current_version();
    assert_eq!(
        after, parsed,
        "current_version() must not consult on-disk update-check.json"
    );
    assert_ne!(
        after,
        semver::Version::new(999, 999, 999),
        "current_version() must NOT pick up the bogus cached value"
    );
}

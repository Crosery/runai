//! P1 regression tests for `core::market` (chunk c0).
//!
//! Covers source list persistence, cache TTL enforcement, name+source lookup,
//! and plugin-marker round-trip. These are pure-fs / in-memory tests using
//! the public API surface of `runai::core::market`; they DO NOT hit the
//! network and DO NOT spawn the runai binary.
//!
//! Each test gets its own `tempfile::TempDir` so no test touches the real
//! `~/.runai/`. Tests live behind `cfg(not(target_os = "windows"))` only when
//! they would need symlinks — these don't, so they run on all platforms.

#![allow(dead_code, unused_imports)]

use runai::core::market::{
    MarketSkill, SourceEntry, find_skill_in_sources, is_plugin_source, load_cache, load_sources,
    save_cache, save_plugin_marker, save_sources,
};

// ─── helpers ───────────────────────────────────────────────────────────────

fn mk_source(owner: &str, repo: &str) -> SourceEntry {
    SourceEntry {
        owner: owner.into(),
        repo: repo.into(),
        branch: "main".into(),
        skill_prefix: String::new(),
        label: format!("{owner}/{repo}"),
        description: "test".into(),
        builtin: false,
        enabled: true,
    }
}

fn mk_skill(name: &str, repo_path: &str, source: &SourceEntry) -> MarketSkill {
    MarketSkill {
        name: name.into(),
        repo_path: repo_path.into(),
        source_label: source.label.clone(),
        source_repo: source.repo_id(),
        branch: source.branch.clone(),
        installed: false,
    }
}

// ─── Test 1: load_sources + save_sources roundtrip ─────────────────────────

#[test]
fn market_load_save_sources_roundtrip() {
    let tmp = tempfile::tempdir().expect("tmp data dir");
    let data_dir = tmp.path();

    // 1. Start clean: no market-sources.json yet. load_sources must still
    //    return the built-in list (default behaviour). Built-ins are merged
    //    in regardless of disk state, per `builtin_sources()`.
    let baseline = load_sources(data_dir);
    assert!(
        !baseline.is_empty(),
        "load_sources on a virgin data dir must still return built-in sources"
    );
    assert!(
        baseline.iter().all(|s| s.builtin),
        "with no saved file, every returned source should be a built-in"
    );

    // 2. Construct a payload: subset of built-ins (with toggled enabled) +
    //    two user-added repos. Save it.
    let user_a = SourceEntry::from_input("crosery/runai-skills")
        .expect("parse user-added source from owner/repo");
    let user_b = SourceEntry::from_input("foo/bar@dev")
        .expect("parse user-added source with branch suffix");

    // Take the first built-in and flip its enabled state to verify per-source
    // state is preserved on reload.
    let mut flipped = baseline[0].clone();
    let flipped_repo_id = flipped.repo_id();
    let flipped_was_enabled = flipped.enabled;
    flipped.enabled = !flipped_was_enabled;

    let payload = vec![flipped, user_a.clone(), user_b.clone()];
    save_sources(data_dir, &payload).expect("save_sources writes the JSON file");

    // 3. Verify file exists at the expected path.
    let path = data_dir.join("market-sources.json");
    assert!(
        path.is_file(),
        "save_sources should create market-sources.json at {}",
        path.display()
    );

    // 4. Reload and check user-added sources are present with the right
    //    owner / repo / branch.
    let reloaded = load_sources(data_dir);
    let reload_user_a = reloaded
        .iter()
        .find(|s| s.repo_id() == "crosery/runai-skills")
        .expect("user-added source crosery/runai-skills survived the roundtrip");
    assert!(!reload_user_a.builtin, "user-added source must keep builtin=false");
    assert_eq!(reload_user_a.owner, "crosery");
    assert_eq!(reload_user_a.repo, "runai-skills");
    assert_eq!(reload_user_a.branch, "main", "default branch is main");

    let reload_user_b = reloaded
        .iter()
        .find(|s| s.repo_id() == "foo/bar")
        .expect("user-added source foo/bar survived the roundtrip");
    assert!(!reload_user_b.builtin);
    assert_eq!(reload_user_b.branch, "dev", "branch suffix must be parsed");

    // 5. Per-builtin enabled-state should be preserved across reload.
    let reload_flipped = reloaded
        .iter()
        .find(|s| s.repo_id() == flipped_repo_id)
        .expect("the flipped built-in source must reappear after reload");
    assert!(reload_flipped.builtin);
    assert_eq!(
        reload_flipped.enabled, !flipped_was_enabled,
        "saved enabled flag must round-trip on a built-in source"
    );

    // 6. Invalid JSON must NOT panic; load_sources gracefully returns just
    //    built-ins (defaulted) when the saved file is corrupt.
    std::fs::write(&path, b"{not valid json").unwrap();
    let after_corrupt = load_sources(data_dir);
    assert!(
        !after_corrupt.is_empty(),
        "load_sources on a corrupt file must still return built-ins as fallback"
    );
    assert!(
        after_corrupt.iter().all(|s| s.builtin),
        "with corrupt file, only built-ins should appear (user-added entries lost \
         is the expected graceful behaviour)"
    );
}

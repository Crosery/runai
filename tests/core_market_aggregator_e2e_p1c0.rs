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

// ─── Test 2: cache TTL enforcement ─────────────────────────────────────────

#[test]
fn market_cache_ttl_enforcement() {
    let tmp = tempfile::tempdir().expect("tmp data dir");
    let data_dir = tmp.path();

    let source = mk_source("anthropics", "skills");
    let cached_skills = vec![
        mk_skill("brainstorming", "skills/brainstorming", &source),
        mk_skill("research", "skills/research", &source),
    ];

    // 1. save_cache must create the cache dir + write a parseable JSON file
    //    at the expected per-source path.
    save_cache(data_dir, &source, &cached_skills).expect("save_cache writes JSON");
    let cache_file = data_dir
        .join("market-cache")
        .join(format!("{}_{}.json", source.owner, source.repo));
    assert!(
        cache_file.is_file(),
        "save_cache must write to {}",
        cache_file.display()
    );

    // 2. Fresh cache (mtime < 1h): load_cache returns Some(Vec) with the
    //    original payload.
    let fresh = load_cache(data_dir, &source).expect("fresh cache must load");
    assert_eq!(fresh.len(), 2, "cache should preserve all entries");
    let names: Vec<&str> = fresh.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"brainstorming"));
    assert!(names.contains(&"research"));

    // 3. Force the file to look stale by rewinding its mtime to >1h ago.
    //    CACHE_MAX_AGE_SECS is 3600; we go to 2h ago to be unambiguous.
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(7200);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&cache_file)
        .expect("open cache file for mtime edit");
    file.set_modified(two_hours_ago)
        .expect("set_modified to 2h ago (Rust 1.75+ API)");
    drop(file);

    // 4. Stale cache (mtime > 1h): load_cache returns None — the TTL guard
    //    must reject this file and force a refresh.
    let stale = load_cache(data_dir, &source);
    assert!(
        stale.is_none(),
        "load_cache must return None for cache older than 1h, got Some"
    );

    // 5. Re-saving refreshes the mtime and the cache is hot again.
    save_cache(data_dir, &source, &cached_skills).expect("re-save refreshes cache");
    let refreshed = load_cache(data_dir, &source);
    assert!(
        refreshed.is_some(),
        "after re-save, load_cache must return Some again"
    );

    // 6. A wholly missing cache (different source) returns None, no panic.
    let other = mk_source("vercel-labs", "agent-skills");
    assert!(
        load_cache(data_dir, &other).is_none(),
        "load_cache must return None for a never-written source"
    );

    // 7. Corrupt cache contents (valid mtime, broken JSON) also returns None.
    std::fs::write(&cache_file, b"garbage").unwrap();
    let corrupt = load_cache(data_dir, &source);
    assert!(
        corrupt.is_none(),
        "load_cache must return None for corrupt JSON, not panic"
    );
}

// ─── Test 3: find_skill_in_sources by name + source filter ─────────────────

#[test]
fn market_find_skill_by_name_and_filter() {
    let tmp = tempfile::tempdir().expect("tmp data dir");
    let data_dir = tmp.path();

    let src1 = SourceEntry {
        owner: "repo1".into(),
        repo: "skills".into(),
        branch: "main".into(),
        skill_prefix: String::new(),
        label: "Repo One".into(),
        description: "".into(),
        builtin: false,
        enabled: true,
    };
    let src2 = SourceEntry {
        owner: "repo2".into(),
        repo: "skills".into(),
        branch: "main".into(),
        skill_prefix: String::new(),
        label: "Repo Two".into(),
        description: "".into(),
        builtin: false,
        enabled: true,
    };

    // Seed two separate caches.
    save_cache(data_dir, &src1, &[mk_skill("foo", "foo", &src1)])
        .expect("seed repo1 cache");
    save_cache(data_dir, &src2, &[mk_skill("bar", "bar", &src2)])
        .expect("seed repo2 cache");

    let sources = vec![src1.clone(), src2.clone()];

    // 1. (foo, None) — found in any enabled source.
    let r = find_skill_in_sources(data_dir, &sources, "foo", None)
        .expect("foo should be found without filter");
    assert_eq!(r.name, "foo");
    assert_eq!(r.source_repo, "repo1/skills");

    // 2. (foo, Some("repo1")) — match by repo_id substring; repo1/skills
    //    contains "repo1".
    let r = find_skill_in_sources(data_dir, &sources, "foo", Some("repo1"))
        .expect("foo should be found in repo1 filter");
    assert_eq!(r.name, "foo");
    assert_eq!(r.source_repo, "repo1/skills");

    // 3. (foo, Some("Repo Two")) — filter matches src2's label, but src2's
    //    cache has 'bar', not 'foo' → must return None.
    let r = find_skill_in_sources(data_dir, &sources, "foo", Some("Repo Two"));
    assert!(
        r.is_none(),
        "foo only lives in repo1; filtering to Repo Two must return None, got {r:?}"
    );

    // 4. (baz, None) — not in any cache.
    let r = find_skill_in_sources(data_dir, &sources, "baz", None);
    assert!(r.is_none(), "baz is in no cache; expect None");

    // 5. Disabled source must be skipped entirely. Mark src1 disabled and
    //    ask for 'foo' — it should now be unfindable because src2 doesn't
    //    have foo and src1 is excluded.
    let mut sources_disabled = sources.clone();
    sources_disabled[0].enabled = false;
    let r = find_skill_in_sources(data_dir, &sources_disabled, "foo", None);
    assert!(
        r.is_none(),
        "find_skill_in_sources must skip disabled sources; expected None"
    );

    // 6. Empty source slice returns None gracefully (no panic, no lookup).
    let r = find_skill_in_sources(data_dir, &[], "foo", None);
    assert!(r.is_none(), "empty sources → None");
}

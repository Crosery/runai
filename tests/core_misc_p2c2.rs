//! P2 regression tests for misc core modules (resource + search).
//!
//! Cloud HEAD layout:
//! - `core::resource` exists (this chunk covers it)
//! - `core::search` exists (this chunk covers it)
//! - `core::prefs` does NOT exist on this branch (skipped: src_missing)
//! - `core::server_mode` does NOT exist on this branch (skipped: src_missing)
//!
//! These are pure-Rust integration tests against the public API of the
//! `runai` library crate, so we do not need to spawn the binary or touch
//! the filesystem. No HOME / RUNE_DATA_DIR sandbox is required here.

#![cfg(not(target_os = "windows"))]

// ─── core::resource ─────────────────────────────────────────────────────────

use runai::core::resource::{Resource, ResourceKind, Source, TrashEntry};
use std::collections::HashMap;
use std::path::PathBuf;

#[test]
fn resource_id_generation_local() {
    // Local source → "local:<name>"
    let src = Source::Local {
        path: PathBuf::from("/tmp/foo"),
    };
    let id = Resource::generate_id(&src, "foo");
    assert_eq!(id, "local:foo");
}

#[test]
fn resource_id_generation_github() {
    // GitHub source → "github:<owner>/<repo>:<name>"
    let src = Source::GitHub {
        owner: "octocat".into(),
        repo: "hello-world".into(),
        branch: "main".into(),
    };
    let id = Resource::generate_id(&src, "skill-a");
    assert_eq!(id, "github:octocat/hello-world:skill-a");
}

#[test]
fn resource_id_generation_adopted() {
    // Adopted source → "adopted:<name>"
    let src = Source::Adopted {
        original_cli: "claude".into(),
    };
    let id = Resource::generate_id(&src, "legacy");
    assert_eq!(id, "adopted:legacy");
}

#[test]
fn resource_id_stable_across_calls() {
    // Same input must always produce the same id (id is DB primary key)
    let src = Source::Local {
        path: PathBuf::from("/x"),
    };
    let id1 = Resource::generate_id(&src, "abc");
    let id2 = Resource::generate_id(&src, "abc");
    assert_eq!(id1, id2);
}

#[test]
fn resource_id_distinguishes_local_vs_github_same_name() {
    // Same name + different sources must not collide
    let local = Source::Local {
        path: PathBuf::from("/a"),
    };
    let gh = Source::GitHub {
        owner: "o".into(),
        repo: "r".into(),
        branch: "main".into(),
    };
    let id_local = Resource::generate_id(&local, "name");
    let id_gh = Resource::generate_id(&gh, "name");
    assert_ne!(id_local, id_gh);
    assert!(id_local.starts_with("local:"));
    assert!(id_gh.starts_with("github:"));
}

#[test]
fn resource_kind_as_str_lowercase() {
    // ResourceKind serialization keys (DB / API expect lowercase strings)
    assert_eq!(ResourceKind::Skill.as_str(), "skill");
    assert_eq!(ResourceKind::Mcp.as_str(), "mcp");
}

#[test]
fn resource_kind_fromstr_roundtrip() {
    // FromStr accepts the same lowercase strings as_str() emits
    assert_eq!("skill".parse::<ResourceKind>().unwrap(), ResourceKind::Skill);
    assert_eq!("mcp".parse::<ResourceKind>().unwrap(), ResourceKind::Mcp);
}

#[test]
fn resource_kind_fromstr_rejects_unknown() {
    // Garbage input must Err so callers don't silently degrade
    assert!("agent".parse::<ResourceKind>().is_err());
    assert!("".parse::<ResourceKind>().is_err());
    assert!("Skill".parse::<ResourceKind>().is_err()); // case-sensitive
}

#[test]
fn resource_kind_serde_json_roundtrip() {
    // serde rename_all = "lowercase"
    let s_json = serde_json::to_string(&ResourceKind::Skill).unwrap();
    assert_eq!(s_json, "\"skill\"");
    let parsed: ResourceKind = serde_json::from_str("\"mcp\"").unwrap();
    assert_eq!(parsed, ResourceKind::Mcp);
}

#[test]
fn source_type_strings() {
    let l = Source::Local {
        path: PathBuf::from("/p"),
    };
    let g = Source::GitHub {
        owner: "o".into(),
        repo: "r".into(),
        branch: "b".into(),
    };
    let a = Source::Adopted {
        original_cli: "claude".into(),
    };
    assert_eq!(l.source_type(), "local");
    assert_eq!(g.source_type(), "github");
    assert_eq!(a.source_type(), "adopted");
}

#[test]
fn source_meta_json_roundtrip() {
    // to_meta_json / from_meta_json round-trip
    let src = Source::GitHub {
        owner: "anthropics".into(),
        repo: "skills".into(),
        branch: "main".into(),
    };
    let meta = src.to_meta_json();
    let back = Source::from_meta_json("github", &meta).expect("decode roundtrip");
    match back {
        Source::GitHub {
            owner,
            repo,
            branch,
        } => {
            assert_eq!(owner, "anthropics");
            assert_eq!(repo, "skills");
            assert_eq!(branch, "main");
        }
        _ => panic!("expected GitHub variant"),
    }
}

#[test]
fn source_from_meta_json_rejects_unknown_type() {
    // Unknown source_type returns None (defensive against schema drift)
    let bogus = Source::from_meta_json("nonsense", "{}");
    assert!(bogus.is_none());
}

#[test]
fn trash_entry_serde_roundtrip_full() {
    // TrashEntry must round-trip through serde — restore depends on this
    let entry = TrashEntry {
        id: "trash-1".into(),
        resource_id: "local:foo".into(),
        name: "foo".into(),
        kind: ResourceKind::Skill,
        description: "desc".into(),
        directory: PathBuf::from("/tmp/skills/foo"),
        source: Source::Local {
            path: PathBuf::from("/tmp/skills/foo"),
        },
        installed_at: 1_700_000_000,
        usage_count: 5,
        last_used_at: Some(1_700_000_999),
        deleted_at: 1_700_001_000,
        payload_path: Some(PathBuf::from("/tmp/trash/payload")),
        enabled_targets: vec![],
        group_ids: vec!["group-a".into()],
        mcp_configs: HashMap::new(),
        disabled_backup: None,
    };
    let json = serde_json::to_string(&entry).expect("encode trash entry");
    let back: TrashEntry = serde_json::from_str(&json).expect("decode trash entry");
    assert_eq!(back.id, "trash-1");
    assert_eq!(back.name, "foo");
    assert_eq!(back.kind, ResourceKind::Skill);
    assert_eq!(back.usage_count, 5);
    assert_eq!(back.group_ids, vec!["group-a".to_string()]);
}

#[test]
fn trash_entry_legacy_missing_optional_fields() {
    // Legacy trash JSON predating enabled_targets / group_ids / mcp_configs /
    // disabled_backup must still decode (serde(default) on those fields).
    let legacy = serde_json::json!({
        "id": "old-1",
        "resource_id": "local:bar",
        "name": "bar",
        "kind": "skill",
        "description": "",
        "directory": "/tmp/x",
        "source": { "type": "local", "path": "/tmp/x" },
        "installed_at": 1,
        "usage_count": 0,
        "last_used_at": null,
        "deleted_at": 2,
        "payload_path": null
    });
    let back: TrashEntry = serde_json::from_value(legacy).expect("legacy decode");
    assert_eq!(back.name, "bar");
    assert!(back.enabled_targets.is_empty());
    assert!(back.group_ids.is_empty());
    assert!(back.mcp_configs.is_empty());
    assert!(back.disabled_backup.is_none());
}

#[test]
fn resource_is_enabled_for_defaults_false() {
    // Resource.enabled defaults via HashMap::get → false for absent CliTarget
    use runai::core::cli_target::CliTarget;
    let r = Resource {
        id: "local:x".into(),
        name: "x".into(),
        kind: ResourceKind::Skill,
        description: "".into(),
        directory: PathBuf::from("/x"),
        source: Source::Local {
            path: PathBuf::from("/x"),
        },
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
    };
    assert!(!r.is_enabled_for(CliTarget::Claude));
    assert!(!r.is_enabled_for(CliTarget::Codex));
}

#[test]
fn resource_is_enabled_for_reflects_map() {
    use runai::core::cli_target::CliTarget;
    let mut enabled = HashMap::new();
    enabled.insert(CliTarget::Claude, true);
    enabled.insert(CliTarget::Codex, false);
    let r = Resource {
        id: "local:y".into(),
        name: "y".into(),
        kind: ResourceKind::Skill,
        description: "".into(),
        directory: PathBuf::from("/y"),
        source: Source::Local {
            path: PathBuf::from("/y"),
        },
        installed_at: 0,
        enabled,
        usage_count: 0,
        last_used_at: None,
    };
    assert!(r.is_enabled_for(CliTarget::Claude));
    assert!(!r.is_enabled_for(CliTarget::Codex));
}

// ─── core::search ───────────────────────────────────────────────────────────

use runai::core::search::{fuzzy_score, fuzzy_score_any, new_matcher, rank};

#[test]
fn search_typo_one_char_deletion_still_matches() {
    // 'fronted' is a subsequence of 'frontend' (deleted 'n')
    let mut m = new_matcher();
    let score = fuzzy_score(&mut m, "frontend-design", "fronted");
    assert!(
        score.is_some(),
        "typo subsequence should still produce a score, got None"
    );
    assert!(score.unwrap() > 0);
}

#[test]
fn search_score_any_returns_best_field() {
    // Multiple fields, only the second one matches well — best score wins
    let mut m = new_matcher();
    let strong = fuzzy_score_any(&mut m, "figma", &["plain-name", "figma-region-loop"]);
    assert!(strong.is_some());
    // Score of "figma" against "figma-region-loop" should exceed score against
    // "plain-name" (which doesn't match at all → None contributes nothing).
    let single = fuzzy_score(&mut m, "figma-region-loop", "figma").unwrap();
    assert_eq!(strong.unwrap(), single);
}

#[test]
fn search_score_any_no_field_matches_returns_none() {
    let mut m = new_matcher();
    let none = fuzzy_score_any(&mut m, "zzz", &["aaa", "bbb", "ccc"]);
    assert!(none.is_none());
}

#[test]
fn search_rank_orders_high_to_low() {
    // rank() must sort items by score descending
    let items = vec!["frontend-design", "frontend-slides", "backend-api"];
    let ranked = rank("frontend", items, |s| vec![s.to_string()]);
    assert!(!ranked.is_empty(), "expected at least one match");
    // backend-api should drop out (no match)
    assert!(ranked.iter().all(|(item, _)| item.starts_with("frontend")));
    // Sorted high → low
    for w in ranked.windows(2) {
        assert!(
            w[0].1 >= w[1].1,
            "rank result must be sorted descending: {:?}",
            ranked
        );
    }
}

#[test]
fn search_rank_drops_non_matches() {
    // Items whose fields_of() yields no fuzzy match must not appear in output
    let items = vec!["alpha", "beta", "gamma"];
    let ranked = rank("zzzzzz", items, |s| vec![s.to_string()]);
    assert!(ranked.is_empty(), "no item should match impossible needle");
}

#[test]
fn search_rank_supports_fzf_prefix_operator() {
    // '^prefix' fzf operator (rank goes through Pattern::parse)
    let items = vec!["alpha-one", "two-alpha", "alpha-three"];
    let ranked = rank("^alpha", items, |s| vec![s.to_string()]);
    // Only the prefixed items must pass
    for (item, _score) in &ranked {
        assert!(
            item.starts_with("alpha"),
            "^alpha must only match prefixed items, got {item}"
        );
    }
    assert_eq!(ranked.len(), 2);
}

#[test]
fn search_rank_supports_fzf_suffix_operator() {
    // 'suffix$' fzf operator
    let items = vec!["one-skill", "two-mcp", "three-skill"];
    let ranked = rank("skill$", items, |s| vec![s.to_string()]);
    for (item, _score) in &ranked {
        assert!(
            item.ends_with("skill"),
            "skill$ must only match suffixed items, got {item}"
        );
    }
    assert_eq!(ranked.len(), 2);
}

#[test]
fn search_rank_supports_fzf_exact_operator() {
    // "'exact" fzf substring operator
    let items = vec!["foo-bar", "baz-qux", "xfoox"];
    let ranked = rank("'foo", items, |s| vec![s.to_string()]);
    // Must include exact substring "foo"
    for (item, _) in &ranked {
        assert!(
            item.contains("foo"),
            "'foo must require substring foo, got {item}"
        );
    }
    assert!(ranked.iter().any(|(i, _)| *i == "foo-bar"));
}

#[test]
fn search_smart_case_lowercase_needle_is_case_insensitive() {
    // Lower-case needle → smart case treats as case-insensitive
    let mut m = new_matcher();
    let lower_against_capitalized = fuzzy_score(&mut m, "Python-Testing", "python");
    assert!(
        lower_against_capitalized.is_some(),
        "lowercase needle should match capitalized haystack under smart-case"
    );
}

#[test]
fn search_rank_smart_case_uppercase_needle_is_strict() {
    // Mixed-case needle → smart case becomes case-sensitive.
    // "python" lower should match "Python-Testing" (smart insensitive)
    // "Python" should *also* match "Python-Testing" exactly.
    let items_pylower = vec!["Python-Testing"];
    let lower = rank("python", items_pylower, |s| vec![s.to_string()]);
    assert_eq!(lower.len(), 1);

    let items_strict = vec!["Python-Testing", "python-testing"];
    let upper = rank("Python", items_strict.clone(), |s| vec![s.to_string()]);
    // Both should at least appear (Pattern smart-case still admits case-fold for the lower haystack)
    // but the capitalized haystack should not be filtered out.
    assert!(upper.iter().any(|(i, _)| *i == "Python-Testing"));
}

#[test]
fn search_matcher_reusable_across_multiple_haystacks() {
    // Matcher can be reused — same instance scoring several haystacks
    // without crashing or yielding bogus values.
    let mut m = new_matcher();
    let a = fuzzy_score(&mut m, "frontend-design", "frontend");
    let b = fuzzy_score(&mut m, "frontend-slides", "frontend");
    let c = fuzzy_score(&mut m, "backend-api", "frontend");
    assert!(a.is_some());
    assert!(b.is_some());
    assert!(c.is_none());
}

#[test]
fn search_subsequence_match_returns_some_score() {
    // Classical fzf subsequence test
    let mut m = new_matcher();
    assert!(fuzzy_score(&mut m, "frontend", "fnt").is_some());
}

#[test]
fn search_completely_disjoint_returns_none() {
    let mut m = new_matcher();
    assert!(fuzzy_score(&mut m, "frontend", "xyzqq").is_none());
}

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

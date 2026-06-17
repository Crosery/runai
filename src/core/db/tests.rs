use super::Database;
use crate::core::cli_target::CliTarget;
use crate::core::resource::{Resource, ResourceKind, Source, TrashEntry};
use rusqlite::params;
use std::collections::HashMap;
use std::path::PathBuf;

#[test]
fn migration_creates_schema_version() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();
    let version: i64 = db
        .conn
        .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, 20);
}

#[test]
fn migration_v3_adds_usage_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();
    let version = db.schema_version();
    assert!(version >= 3, "schema version should be >= 3, got {version}");

    // Verify columns exist by inserting and reading back
    let source = crate::core::resource::Source::Local {
        path: PathBuf::from("/tmp"),
    };
    let res = Resource {
        id: "local:test".into(),
        name: "test".into(),
        kind: ResourceKind::Skill,
        description: String::new(),
        directory: PathBuf::from("/tmp"),
        source,
        installed_at: 0,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: None,
        publish_status: "draft".to_string(),
    };
    db.insert_resource(&res).unwrap();

    let loaded = db.get_resource("local:test").unwrap().unwrap();
    assert_eq!(loaded.usage_count, 0);
    assert_eq!(loaded.last_used_at, None);
}

#[test]
fn record_usage_increments_count() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let source = crate::core::resource::Source::Local {
        path: PathBuf::from("/tmp"),
    };
    let res = Resource {
        id: "local:foo".into(),
        name: "foo".into(),
        kind: ResourceKind::Skill,
        description: String::new(),
        directory: PathBuf::from("/tmp"),
        source,
        installed_at: 0,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: None,
        publish_status: "draft".to_string(),
    };
    db.insert_resource(&res).unwrap();

    db.record_usage("local:foo").unwrap();
    db.record_usage("local:foo").unwrap();

    let loaded = db.get_resource("local:foo").unwrap().unwrap();
    assert_eq!(loaded.usage_count, 2);
    assert!(loaded.last_used_at.is_some());
}

#[test]
fn record_usage_unknown_resource_returns_zero_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();
    // Should not error, but affect 0 rows
    let affected = db.record_usage("nonexistent").unwrap();
    assert_eq!(affected, 0);
}

#[test]
fn get_usage_stats_sorted_by_count() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    for (id, name) in &[("local:a", "a"), ("local:b", "b"), ("local:c", "c")] {
        let source = crate::core::resource::Source::Local {
            path: PathBuf::from("/tmp"),
        };
        let res = Resource {
            id: id.to_string(),
            name: name.to_string(),
            kind: ResourceKind::Skill,
            description: String::new(),
            directory: PathBuf::from("/tmp"),
            source,
            installed_at: 0,
            enabled: std::collections::HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: None,
            publish_status: "draft".to_string(),
        };
        db.insert_resource(&res).unwrap();
    }

    // b: 3 uses, a: 1 use, c: 0 uses
    db.record_usage("local:b").unwrap();
    db.record_usage("local:b").unwrap();
    db.record_usage("local:b").unwrap();
    db.record_usage("local:a").unwrap();

    let stats = db.get_usage_stats().unwrap();
    assert_eq!(stats.len(), 3);
    assert_eq!(stats[0].id, "local:b");
    assert_eq!(stats[0].count, 3);
    assert_eq!(stats[1].id, "local:a");
    assert_eq!(stats[1].count, 1);
    assert_eq!(stats[2].id, "local:c");
    assert_eq!(stats[2].count, 0);
}

#[test]
fn insert_resource_preserves_usage_on_conflict() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let source = crate::core::resource::Source::Local {
        path: PathBuf::from("/tmp"),
    };
    let res = Resource {
        id: "local:x".into(),
        name: "x".into(),
        kind: ResourceKind::Skill,
        description: "v1".into(),
        directory: PathBuf::from("/tmp"),
        source: source.clone(),
        installed_at: 0,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: None,
        publish_status: "draft".to_string(),
    };
    db.insert_resource(&res).unwrap();

    // Record usage
    db.record_usage("local:x").unwrap();
    db.record_usage("local:x").unwrap();

    // Re-insert with updated description (simulates re-scan)
    let res2 = Resource {
        id: "local:x".into(),
        name: "x".into(),
        kind: ResourceKind::Skill,
        description: "v2".into(),
        directory: PathBuf::from("/tmp/new"),
        source,
        installed_at: 0,
        enabled: std::collections::HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: None,
        publish_status: "draft".to_string(),
    };
    db.insert_resource(&res2).unwrap();

    // Usage should be preserved, description should be updated
    let loaded = db.get_resource("local:x").unwrap().unwrap();
    assert_eq!(
        loaded.usage_count, 2,
        "usage_count should survive re-insert"
    );
    assert!(
        loaded.last_used_at.is_some(),
        "last_used_at should survive re-insert"
    );
    assert_eq!(loaded.description, "v2", "description should be updated");
}

#[test]
fn trash_entries_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let entry = TrashEntry {
        id: "trash:1".into(),
        resource_id: "local:foo".into(),
        name: "foo".into(),
        kind: ResourceKind::Skill,
        description: "desc".into(),
        directory: PathBuf::from("/tmp/foo"),
        source: Source::Local {
            path: PathBuf::from("/tmp/foo"),
        },
        installed_at: 1,
        usage_count: 3,
        last_used_at: Some(4),
        owner_user_id: None,
        deleted_at: 2,
        payload_path: Some(PathBuf::from("/tmp/trash/foo")),
        enabled_targets: vec![CliTarget::Claude, CliTarget::Codex],
        group_ids: vec!["grp".into()],
        mcp_configs: HashMap::new(),
        disabled_backup: None,
    };

    db.insert_trash_entry(&entry).unwrap();

    let listed = db.list_trash_entries().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "trash:1");
    assert_eq!(listed[0].enabled_targets.len(), 2);

    let loaded = db.get_trash_entry("trash:1").unwrap().unwrap();
    assert_eq!(loaded.name, "foo");
    assert_eq!(loaded.group_ids, vec!["grp".to_string()]);

    db.delete_trash_entry("trash:1").unwrap();
    assert!(db.get_trash_entry("trash:1").unwrap().is_none());
}

#[test]
fn migration_preserves_group_members() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("test.db");

    // Create old schema with FK (disable FK enforcement to insert mcp: row without resources entry)
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             CREATE TABLE resources (id TEXT PRIMARY KEY, name TEXT, kind TEXT, description TEXT, directory TEXT, source_type TEXT, source_meta TEXT, installed_at INTEGER);
             CREATE TABLE group_members (group_id TEXT, resource_id TEXT, PRIMARY KEY(group_id, resource_id), FOREIGN KEY(resource_id) REFERENCES resources(id));
             INSERT INTO resources VALUES ('local:foo','foo','skill','','/tmp','local','{}',0);
             INSERT INTO group_members VALUES ('grp1','local:foo');
             INSERT INTO group_members VALUES ('grp1','mcp:bar');"
        ).unwrap();
    }

    // Open with migration
    let db = Database::open(&db_path).unwrap();
    let ids = db.get_group_member_ids("grp1").unwrap();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"local:foo".to_string()));
    assert!(ids.contains(&"mcp:bar".to_string()));
}

// -------- v15 multi-user tests --------

#[test]
fn schema_at_v15_after_open() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("v15.db")).unwrap();
    let version: i64 = db
        .conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    // After v17 added resources.publish_status the freshly-opened DB
    // should report the current head, not the v15 snapshot. The name of
    // this test is kept for git-blame continuity; the v15 tables it
    // spot-checks below are still there post-v17, just behind a higher
    // version number.
    assert_eq!(version, 20);

    // Tables must exist
    for tbl in &["users", "user_skill_library"] {
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![tbl],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "table {} missing", tbl);
    }

    // resources.owner_user_id must exist
    let owner_col: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('resources') WHERE name='owner_user_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner_col, 1);

    // router_events.user_id must exist
    let user_id_col: i64 = db
        .conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('router_events') WHERE name='user_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(user_id_col, 1);
}

#[test]
fn user_crud_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("users.db")).unwrap();

    db.create_user("u1", "alice", "phash1", "akhash1", false)
        .unwrap();

    // Look up by username
    let u = db.find_user_by_username("alice").unwrap().unwrap();
    assert_eq!(u.user_id, "u1");
    assert_eq!(u.username, "alice");
    assert!(!u.is_admin);
    assert!(!u.disabled);

    // Look up by api_key_hash
    let u2 = db.find_user_by_api_key_hash("akhash1").unwrap().unwrap();
    assert_eq!(u2.user_id, "u1");

    // Username uniqueness enforced
    let dup = db.create_user("u2", "alice", "phash2", "akhash2", false);
    assert!(dup.is_err(), "duplicate username must fail");

    // Admin promotion
    db.set_user_admin("u1", true).unwrap();
    let promoted = db.find_user_by_id("u1").unwrap().unwrap();
    assert!(promoted.is_admin);

    // Disable
    db.set_user_disabled("u1", true).unwrap();
    let disabled = db.find_user_by_id("u1").unwrap().unwrap();
    assert!(disabled.disabled);

    // Prefs update
    db.update_user_prefs("u1", r#"{"allow_public_recommend":true}"#)
        .unwrap();
    let with_prefs = db.find_user_by_id("u1").unwrap().unwrap();
    assert!(with_prefs.prefs_json.contains("allow_public_recommend"));

    // Rotate api key
    db.rotate_api_key("u1", "akhash1_new").unwrap();
    assert!(db.find_user_by_api_key_hash("akhash1").unwrap().is_none());
    assert!(
        db.find_user_by_api_key_hash("akhash1_new")
            .unwrap()
            .is_some()
    );

    // List
    db.create_user("u2", "bob", "phash2", "akhash2", false)
        .unwrap();
    let list = db.list_users().unwrap();
    assert_eq!(list.len(), 2);
}

#[test]
fn library_crud_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("lib.db")).unwrap();

    db.create_user("u1", "alice", "p", "k", false).unwrap();

    // Empty by default
    assert_eq!(db.library_count("u1").unwrap(), 0);
    assert!(!db.library_contains("u1", "bolder").unwrap());

    // Add
    db.library_add("u1", "bolder").unwrap();
    db.library_add("u1", "delight").unwrap();
    assert_eq!(db.library_count("u1").unwrap(), 2);
    assert!(db.library_contains("u1", "bolder").unwrap());

    // Idempotent add (INSERT OR IGNORE)
    db.library_add("u1", "bolder").unwrap();
    assert_eq!(db.library_count("u1").unwrap(), 2);

    // Remove one
    db.library_remove("u1", "bolder").unwrap();
    assert!(!db.library_contains("u1", "bolder").unwrap());
    assert_eq!(db.library_count("u1").unwrap(), 1);

    // List in DESC order of added_at
    let list = db.library_list("u1").unwrap();
    assert_eq!(list, vec!["delight"]);

    // Clear
    let cleared = db.library_clear("u1").unwrap();
    assert_eq!(cleared, 1);
    assert_eq!(db.library_count("u1").unwrap(), 0);

    // Per-user isolation
    db.create_user("u2", "bob", "p", "k2", false).unwrap();
    db.library_add("u1", "bolder").unwrap();
    db.library_add("u2", "overdrive").unwrap();
    assert_eq!(db.library_list("u1").unwrap(), vec!["bolder"]);
    assert_eq!(db.library_list("u2").unwrap(), vec!["overdrive"]);
}

// =========================================================================
//  Phase B: owner_user_id write-path + per-user query helpers
// =========================================================================

fn mk_skill(id: &str, name: &str, owner: Option<&str>) -> Resource {
    Resource {
        id: id.into(),
        name: name.into(),
        kind: ResourceKind::Skill,
        description: "x".into(),
        directory: PathBuf::from("/tmp"),
        source: crate::core::resource::Source::Local {
            path: PathBuf::from("/tmp"),
        },
        installed_at: 0,
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: owner.map(String::from),
        publish_status: "draft".to_string(),
    }
}

#[test]
fn insert_resource_persists_owner_user_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    db.insert_resource(&mk_skill("local:pub", "pub", None))
        .unwrap();
    db.insert_resource(&mk_skill(
        "u:usr_alice:local:priv",
        "priv",
        Some("usr_alice"),
    ))
    .unwrap();

    let pub_row = db.get_resource("local:pub").unwrap().unwrap();
    let priv_row = db.get_resource("u:usr_alice:local:priv").unwrap().unwrap();
    assert_eq!(pub_row.owner_user_id, None, "public row stays NULL");
    assert_eq!(
        priv_row.owner_user_id.as_deref(),
        Some("usr_alice"),
        "private row carries owner"
    );
}

#[test]
fn list_resources_for_user_filters_by_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    db.insert_resource(&mk_skill("local:pubA", "pubA", None))
        .unwrap();
    db.insert_resource(&mk_skill("local:pubB", "pubB", None))
        .unwrap();
    db.insert_resource(&mk_skill(
        "u:usr_alice:local:apriv",
        "apriv",
        Some("usr_alice"),
    ))
    .unwrap();
    db.insert_resource(&mk_skill("u:usr_bob:local:bpriv", "bpriv", Some("usr_bob")))
        .unwrap();

    // owner = None: public only.
    let public_only = db
        .list_resources_for_user(Some(ResourceKind::Skill), None)
        .unwrap();
    let names: Vec<_> = public_only.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["pubA", "pubB"]);

    // owner = Some(alice): public ∪ alice's private.
    let alice = db
        .list_resources_for_user(Some(ResourceKind::Skill), Some("usr_alice"))
        .unwrap();
    let names: Vec<_> = alice.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["apriv", "pubA", "pubB"]);

    // owner = Some(bob): public ∪ bob's private, NOT alice's.
    let bob = db
        .list_resources_for_user(Some(ResourceKind::Skill), Some("usr_bob"))
        .unwrap();
    let names: Vec<_> = bob.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["bpriv", "pubA", "pubB"]);
    assert!(bob.iter().all(|r| r.name != "apriv"));

    // owner = Some("*"): admin sees everything across all owners.
    let admin = db
        .list_resources_for_user(Some(ResourceKind::Skill), Some("*"))
        .unwrap();
    let names: Vec<_> = admin.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["apriv", "bpriv", "pubA", "pubB"]);
}

#[test]
fn same_name_private_skills_coexist_across_users() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    // Same `name` (foo), different ids that include the owner prefix.
    db.insert_resource(&mk_skill(
        &Resource::generate_id(
            &crate::core::resource::Source::Local {
                path: PathBuf::from("/tmp"),
            },
            "foo",
            Some("usr_alice"),
        ),
        "foo",
        Some("usr_alice"),
    ))
    .unwrap();
    db.insert_resource(&mk_skill(
        &Resource::generate_id(
            &crate::core::resource::Source::Local {
                path: PathBuf::from("/tmp"),
            },
            "foo",
            Some("usr_bob"),
        ),
        "foo",
        Some("usr_bob"),
    ))
    .unwrap();

    // Each owner sees only their own foo (no public version exists).
    let alice = db
        .list_resources_for_user(Some(ResourceKind::Skill), Some("usr_alice"))
        .unwrap();
    assert_eq!(alice.len(), 1);
    assert_eq!(alice[0].name, "foo");
    assert_eq!(alice[0].owner_user_id.as_deref(), Some("usr_alice"));

    let bob = db
        .list_resources_for_user(Some(ResourceKind::Skill), Some("usr_bob"))
        .unwrap();
    assert_eq!(bob.len(), 1);
    assert_eq!(bob[0].owner_user_id.as_deref(), Some("usr_bob"));

    // Public scope sees neither (both are private).
    let public_only = db
        .list_resources_for_user(Some(ResourceKind::Skill), None)
        .unwrap();
    assert!(public_only.is_empty(), "no public foo exists");
}

#[test]
fn find_by_name_for_user_private_wins_over_public() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    // Public foo, and alice's private foo. Alice sees the private one.
    db.insert_resource(&mk_skill("local:foo", "foo", None))
        .unwrap();
    db.insert_resource(&mk_skill("u:usr_alice:local:foo", "foo", Some("usr_alice")))
        .unwrap();

    // Anonymous lookup → public.
    let pub_hit = db
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", None)
        .unwrap()
        .unwrap();
    assert_eq!(pub_hit.owner_user_id, None);

    // Alice's lookup → her private one (shadows public).
    let alice_hit = db
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some("usr_alice"))
        .unwrap()
        .unwrap();
    assert_eq!(alice_hit.owner_user_id.as_deref(), Some("usr_alice"));

    // Bob's lookup → public foo (he has no private one).
    let bob_hit = db
        .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some("usr_bob"))
        .unwrap()
        .unwrap();
    assert_eq!(bob_hit.owner_user_id, None);

    // Unknown name → None.
    assert!(
        db.find_resource_by_name_for_user(ResourceKind::Skill, "nope", Some("usr_alice"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn generate_id_distinguishes_public_from_private() {
    let src = crate::core::resource::Source::Local {
        path: PathBuf::from("/tmp"),
    };
    let pub_id = Resource::generate_id(&src, "foo", None);
    let alice_id = Resource::generate_id(&src, "foo", Some("usr_alice"));
    let bob_id = Resource::generate_id(&src, "foo", Some("usr_bob"));

    assert_eq!(pub_id, "local:foo", "public id is back-compat with pre-v15");
    assert_eq!(alice_id, "u:usr_alice:local:foo");
    assert_eq!(bob_id, "u:usr_bob:local:foo");
    assert_ne!(alice_id, bob_id);
    assert_ne!(alice_id, pub_id);
}

#[test]
fn trash_entry_owner_user_id_survives_serde_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    let entry = TrashEntry {
        id: "trash:priv".into(),
        resource_id: "u:usr_alice:local:secret".into(),
        name: "secret".into(),
        kind: ResourceKind::Skill,
        description: "alice's".into(),
        directory: PathBuf::from("/tmp/secret"),
        source: crate::core::resource::Source::Local {
            path: PathBuf::from("/tmp/secret"),
        },
        installed_at: 1,
        usage_count: 0,
        last_used_at: None,
        owner_user_id: Some("usr_alice".into()),
        deleted_at: 2,
        payload_path: None,
        enabled_targets: vec![],
        group_ids: vec![],
        mcp_configs: HashMap::new(),
        disabled_backup: None,
    };
    db.insert_trash_entry(&entry).unwrap();

    let loaded = db.get_trash_entry("trash:priv").unwrap().unwrap();
    assert_eq!(loaded.owner_user_id.as_deref(), Some("usr_alice"));
}

#[test]
fn pre_v15_trash_payload_decodes_with_owner_default_none() {
    // Older payload_json blobs predate the owner_user_id field. They
    // must still decode (serde(default)) and surface as public-pool
    // trash so the restore path treats them as public.
    let pre_v15_json = r#"{
        "id": "trash:legacy",
        "resource_id": "local:legacy",
        "name": "legacy",
        "kind": "skill",
        "description": "",
        "directory": "/tmp/legacy",
        "source": { "type": "local", "path": "/tmp/legacy" },
        "installed_at": 0,
        "usage_count": 0,
        "last_used_at": null,
        "deleted_at": 0,
        "payload_path": null
    }"#;
    let entry: TrashEntry = serde_json::from_str(pre_v15_json).unwrap();
    assert_eq!(entry.owner_user_id, None);
    assert_eq!(entry.id, "trash:legacy");
}

// =========================================================================
//  v16: community market — community_skills table CRUD
// =========================================================================

#[test]
fn community_skills_crud_roundtrip() {
    use crate::core::db::CommunitySort;

    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("community.db")).unwrap();

    // Table empty by default.
    assert_eq!(db.count_community_skills().unwrap(), 0);
    assert!(
        db.get_community_skill("usr_alice", "foo")
            .unwrap()
            .is_none(),
        "missing row → None"
    );

    // Insert a fresh row, then re-read.
    db.insert_community_skill("usr_alice", "foo", "v1").unwrap();
    let row = db
        .get_community_skill("usr_alice", "foo")
        .unwrap()
        .expect("row present after insert");
    assert_eq!(row.uploader_uid, "usr_alice");
    assert_eq!(row.name, "foo");
    assert_eq!(row.version, "v1");
    assert_eq!(row.installs_total, 0);

    // PK collision on raw insert → error.
    assert!(
        db.insert_community_skill("usr_alice", "foo", "v1-dup")
            .is_err(),
        "same (uploader_uid, name) must collide on PK"
    );

    // Upsert bumps version + updated_at, preserves installs_total.
    db.increment_community_installs("usr_alice", "foo").unwrap();
    db.increment_community_installs("usr_alice", "foo").unwrap();
    let pre_upsert = db.get_community_skill("usr_alice", "foo").unwrap().unwrap();
    assert_eq!(pre_upsert.installs_total, 2);

    db.upsert_community_skill("usr_alice", "foo", "v2").unwrap();
    let post_upsert = db.get_community_skill("usr_alice", "foo").unwrap().unwrap();
    assert_eq!(post_upsert.version, "v2", "upsert bumped version");
    assert_eq!(
        post_upsert.installs_total, 2,
        "upsert preserves installs_total"
    );
    assert_eq!(
        post_upsert.created_at, pre_upsert.created_at,
        "upsert preserves created_at"
    );

    // Different uploaders can share a name.
    db.insert_community_skill("usr_bob", "foo", "vbob").unwrap();
    assert!(db.get_community_skill("usr_bob", "foo").unwrap().is_some());

    // List sorted by installs (alice/foo has 2, bob/foo has 0).
    let list = db
        .list_community_skills(CommunitySort::Installs, 0, 50)
        .unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].uploader_uid, "usr_alice");
    assert_eq!(list[1].uploader_uid, "usr_bob");

    // List sorted by name (both are "foo" — tie-break is uploader asc).
    let by_name = db
        .list_community_skills(CommunitySort::Name, 0, 50)
        .unwrap();
    assert_eq!(by_name[0].uploader_uid, "usr_alice");
    assert_eq!(by_name[1].uploader_uid, "usr_bob");

    // Pagination: offset=1 limit=1 returns the second row.
    let page = db
        .list_community_skills(CommunitySort::Installs, 1, 1)
        .unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].uploader_uid, "usr_bob");

    assert_eq!(db.count_community_skills().unwrap(), 2);

    // Delete returns true on hit, false on miss.
    assert!(db.delete_community_skill("usr_alice", "foo").unwrap());
    assert!(!db.delete_community_skill("usr_alice", "foo").unwrap());
    assert!(
        db.get_community_skill("usr_alice", "foo")
            .unwrap()
            .is_none()
    );
    assert_eq!(db.count_community_skills().unwrap(), 1);
}

#[test]
fn community_sort_parses_query_strings() {
    use crate::core::db::CommunitySort;
    matches!(
        CommunitySort::parse(Some("installs")),
        CommunitySort::Installs
    );
    matches!(CommunitySort::parse(Some("name")), CommunitySort::Name);
    matches!(
        CommunitySort::parse(Some("created")),
        CommunitySort::Created
    );
    // Default + unknown both fall through to Installs.
    matches!(CommunitySort::parse(None), CommunitySort::Installs);
    matches!(
        CommunitySort::parse(Some("garbage")),
        CommunitySort::Installs
    );
}

// =========================================================================
//  Owner-aware dedupe — same skill NAME owned by different users must NOT
//  be collapsed into one row (that would delete the loser's directory
//  reference, a cross-owner data-loss against the owner-pool invariant).
// =========================================================================

#[test]
fn cleanup_orphan_library_for_deleted_users_sweeps_ghosts() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    // u1 exists; u_ghost was deleted but left library rows behind.
    db.create_user("u1", "alice", "p", "k", false).unwrap();
    db.library_add("u1", "alpha").unwrap();
    db.library_add("u1", "beta").unwrap();
    db.library_add("u_ghost", "alpha").unwrap();
    db.library_add("u_ghost", "gamma").unwrap();

    let removed = db.cleanup_orphan_library_for_deleted_users().unwrap();
    assert_eq!(removed, 2, "both ghost rows must be swept");
    assert_eq!(db.library_count("u1").unwrap(), 2, "u1's rows are kept");
    assert_eq!(db.library_count("u_ghost").unwrap(), 0);
}

#[test]
fn dedupe_skills_by_name_does_not_merge_across_owners() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    // alice + bob each own a private skill named "shared"; a public "shared"
    // also exists. Three distinct owners, one shared name → no duplicates
    // within any single owner pool, so dedupe must remove nothing.
    db.insert_resource(&mk_skill("local:shared", "shared", None))
        .unwrap();
    db.insert_resource(&mk_skill(
        "u:usr_alice:local:shared",
        "shared",
        Some("usr_alice"),
    ))
    .unwrap();
    db.insert_resource(&mk_skill(
        "u:usr_bob:local:shared",
        "shared",
        Some("usr_bob"),
    ))
    .unwrap();

    let removed = db.dedupe_skills_by_name().unwrap();
    assert_eq!(
        removed, 0,
        "owner-distinct same-name rows must not be merged"
    );

    assert!(db.get_resource("local:shared").unwrap().is_some());
    assert!(
        db.get_resource("u:usr_alice:local:shared")
            .unwrap()
            .is_some()
    );
    assert!(db.get_resource("u:usr_bob:local:shared").unwrap().is_some());
}

#[test]
fn dedupe_skills_by_name_still_collapses_within_one_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    // Two PUBLIC rows of the same name (e.g. local install + later adopt) —
    // the legitimate dedupe case. Keeper = newest installed_at.
    let mut older = mk_skill("local:dup", "dup", None);
    older.installed_at = 100;
    let mut newer = mk_skill("adopted:dup", "dup", None);
    newer.installed_at = 200;
    db.insert_resource(&older).unwrap();
    db.insert_resource(&newer).unwrap();

    let removed = db.dedupe_skills_by_name().unwrap();
    assert_eq!(removed, 1, "same-owner same-name dup must still collapse");
    assert!(db.get_resource("adopted:dup").unwrap().is_some());
    assert!(db.get_resource("local:dup").unwrap().is_none());
}

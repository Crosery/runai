use super::Database;
use crate::core::cli_target::CliTarget;
use crate::core::db::{RouterEvent, RouterIntentMemoryItem, RouterSkillStats, SkillFeedbackRow};
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
    assert_eq!(version, 26);
}

#[test]
fn router_intent_memory_appends_and_drops_oldest() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    for i in 1..=4 {
        db.append_router_intent_memory(
            "rnai_sess_test",
            Some("u1"),
            "pi",
            &format!("memory {i}"),
            3,
        )
        .unwrap();
    }

    let items = db
        .router_intent_memory("rnai_sess_test", Some("u1"), "pi", 10)
        .unwrap();
    let memories: Vec<String> = items.into_iter().map(|i| i.memory).collect();
    assert_eq!(memories, vec!["memory 2", "memory 3", "memory 4"]);
}

#[test]
fn router_intent_memory_is_scoped_by_session_user_and_client() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    db.append_router_intent_memory("rnai_sess_a", Some("u1"), "pi", "pi memory", 10)
        .unwrap();
    db.append_router_intent_memory("rnai_sess_a", Some("u1"), "codex", "codex memory", 10)
        .unwrap();
    db.append_router_intent_memory("rnai_sess_a", Some("u2"), "pi", "other user", 10)
        .unwrap();
    db.append_router_intent_memory("rnai_sess_b", Some("u1"), "pi", "other session", 10)
        .unwrap();

    let items = db
        .router_intent_memory("rnai_sess_a", Some("u1"), "pi", 10)
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].memory, "pi memory");
}

#[test]
fn router_intent_memory_zero_limit_saves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    db.append_router_intent_memory("rnai_sess_zero", None, "claude", "ignored", 0)
        .unwrap();
    let items: Vec<RouterIntentMemoryItem> = db
        .router_intent_memory("rnai_sess_zero", None, "claude", 10)
        .unwrap();
    assert!(items.is_empty());
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
fn migration_v21_moves_ai_summary_to_owner_scoped_key() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("legacy.db");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version VALUES (20);
             CREATE TABLE resource_ai_summary (
                name TEXT PRIMARY KEY,
                summary TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                llm_score INTEGER NOT NULL DEFAULT 5
             );
             INSERT INTO resource_ai_summary (name, summary, updated_at, llm_score)
             VALUES ('legacy', 'task: legacy public summary', 42, 8);",
        )
        .unwrap();
    }

    let db = Database::open(&db_path).unwrap();
    assert_eq!(db.schema_version(), 26);
    let loaded = db.skill_ai_index("legacy").unwrap().unwrap();
    assert_eq!(loaded.summary, "task: legacy public summary");
    assert_eq!(loaded.updated_at, 42);
    assert_eq!(loaded.llm_score, 8);

    let mut stmt = db
        .conn_ref()
        .prepare("PRAGMA table_info(resource_ai_summary)")
        .unwrap();
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(cols.contains(&"owner_user_id".to_string()));
    assert!(cols.contains(&"search_doc".to_string()));

    let owner: String = db
        .conn_ref()
        .query_row(
            "SELECT owner_user_id FROM resource_ai_summary WHERE name = 'legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(owner, "");
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
    assert_eq!(version, 26);

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

/// v22 (issue #35): the browser-session slot is independent from the api_key.
#[test]
fn session_key_hash_roundtrip_and_reset_clears_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("sess.db")).unwrap();
    db.create_user("u1", "alice", "phash1", "akhash1", false)
        .unwrap();

    // No session yet.
    assert!(
        db.find_user_by_session_key_hash("shash1")
            .unwrap()
            .is_none()
    );

    // Set → lookup hits; api_key lane untouched.
    db.set_session_key_hash("u1", Some("shash1")).unwrap();
    assert_eq!(
        db.find_user_by_session_key_hash("shash1")
            .unwrap()
            .unwrap()
            .user_id,
        "u1"
    );
    assert!(db.find_user_by_api_key_hash("akhash1").unwrap().is_some());
    // A session hash never resolves on the api_key lane and vice versa.
    assert!(db.find_user_by_api_key_hash("shash1").unwrap().is_none());
    assert!(
        db.find_user_by_session_key_hash("akhash1")
            .unwrap()
            .is_none()
    );

    // Replace (new browser login) → old session token dies.
    db.set_session_key_hash("u1", Some("shash2")).unwrap();
    assert!(
        db.find_user_by_session_key_hash("shash1")
            .unwrap()
            .is_none()
    );
    assert!(
        db.find_user_by_session_key_hash("shash2")
            .unwrap()
            .is_some()
    );

    // Clear (logout-everywhere).
    db.set_session_key_hash("u1", None).unwrap();
    assert!(
        db.find_user_by_session_key_hash("shash2")
            .unwrap()
            .is_none()
    );

    // Credential reset clears the session alongside password + api_key.
    db.set_session_key_hash("u1", Some("shash3")).unwrap();
    db.set_user_credentials("u1", "phash_new", "akhash_new")
        .unwrap();
    assert!(
        db.find_user_by_session_key_hash("shash3")
            .unwrap()
            .is_none()
    );
    assert!(
        db.find_user_by_api_key_hash("akhash_new")
            .unwrap()
            .is_some()
    );
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

/// C4 (scan_findings.md): trashing a user's PRIVATE skill must NOT wipe every
/// other user's library subscription to the still-existing PUBLIC skill of the
/// same name. `library_remove_for_all` only tracks public-pool subscriptions,
/// so it must no-op while a public row of that name still exists, and only
/// sweep once that public skill is genuinely gone.
#[test]
fn library_remove_for_all_spares_public_when_private_same_name_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    // A public skill `web-scraper` (owner NULL) that user Bob subscribes to.
    db.insert_resource(&mk_skill("local:web-scraper", "web-scraper", None))
        .unwrap();
    // Alice separately owns a PRIVATE skill of the same name (allowed — private
    // shadows public for the owner only).
    db.insert_resource(&mk_skill(
        "u:usr_alice:local:web-scraper",
        "web-scraper",
        Some("usr_alice"),
    ))
    .unwrap();
    db.create_user("usr_bob", "bob", "p", "kb", false).unwrap();
    db.library_add("usr_bob", "web-scraper").unwrap();
    assert!(db.library_contains("usr_bob", "web-scraper").unwrap());

    // Alice trashes HER private web-scraper: the private row goes away, then
    // (as the trash path does) library_remove_for_all is called by name.
    db.delete_resource("u:usr_alice:local:web-scraper").unwrap();
    db.library_remove_for_all("web-scraper").unwrap();

    // Bob's subscription to the STILL-EXISTING public web-scraper must survive.
    assert!(
        db.library_contains("usr_bob", "web-scraper").unwrap(),
        "trashing a private same-name skill must not wipe public subscribers"
    );

    // Now the public skill is genuinely trashed → subscribers are swept.
    db.delete_resource("local:web-scraper").unwrap();
    db.library_remove_for_all("web-scraper").unwrap();
    assert!(
        !db.library_contains("usr_bob", "web-scraper").unwrap(),
        "trashing the public skill must drop its now-orphan subscriptions"
    );
}

/// C4 mirror gap: `cleanup_orphan_library_entries` must count only PUBLIC rows
/// as "the skill still exists". A library row whose public skill was trashed
/// is a genuine orphan even if a different user's PRIVATE skill of that name
/// still exists — it must be swept, not kept alive by the private row.
#[test]
fn cleanup_orphan_library_entries_is_public_pool_aware() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("test.db")).unwrap();

    // Only a PRIVATE `foo` exists (public `foo` was trashed earlier). Bob has a
    // leftover library subscription to the (gone) public `foo`.
    db.insert_resource(&mk_skill("u:usr_alice:local:foo", "foo", Some("usr_alice")))
        .unwrap();
    db.create_user("usr_bob", "bob", "p", "kb", false).unwrap();
    db.library_add("usr_bob", "foo").unwrap();

    // A second, genuinely-valid public subscription that must be KEPT.
    db.insert_resource(&mk_skill("local:bar", "bar", None))
        .unwrap();
    db.library_add("usr_bob", "bar").unwrap();

    let removed = db.cleanup_orphan_library_entries().unwrap();
    assert!(
        removed >= 1,
        "the orphan `foo` subscription must be swept even though a private foo exists"
    );
    assert!(
        !db.library_contains("usr_bob", "foo").unwrap(),
        "orphan public subscription must be gone (private same-name row must not shield it)"
    );
    assert!(
        db.library_contains("usr_bob", "bar").unwrap(),
        "a subscription whose public skill still exists must be kept"
    );
}

#[test]
fn ai_index_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("ai.db")).unwrap();

    let idx = crate::core::db::SkillAiIndex {
        summary: "task: 生成文档\ntriggers: docx".into(),
        search_doc: "docx 生成文档 trigger".into(),
        router_card: "docx: 生成文档 | trigger".into(),
        llm_score: 7,
        updated_at: 123,
        source_hash: "source".into(),
        prompt_hash: "prompt".into(),
        format_key: "summary-task-triggers-inputs-outputs-not-for-score".into(),
    };
    db.set_skill_ai_index("docx-skill", &idx).unwrap();

    let loaded = db.skill_ai_index("docx-skill").unwrap().unwrap();
    assert_eq!(loaded.summary, idx.summary);
    assert_eq!(loaded.search_doc, idx.search_doc);
    assert_eq!(loaded.router_card, idx.router_card);
    assert_eq!(loaded.llm_score, idx.llm_score);
    assert_eq!(loaded.updated_at, idx.updated_at);
    assert_eq!(loaded.source_hash, idx.source_hash);
    assert_eq!(loaded.prompt_hash, idx.prompt_hash);
    assert_eq!(loaded.format_key, idx.format_key);
}

#[test]
fn ai_index_scoped_roundtrip_keeps_same_name_isolated() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("ai-scoped.db")).unwrap();

    let public = crate::core::db::SkillAiIndex {
        summary: "task: public summary".into(),
        search_doc: "public searchable words".into(),
        router_card: "public card".into(),
        llm_score: 4,
        updated_at: 10,
        source_hash: "public-source".into(),
        prompt_hash: "public-prompt".into(),
        format_key: "fmt".into(),
    };
    let private = crate::core::db::SkillAiIndex {
        summary: "task: private summary".into(),
        search_doc: "private searchable words".into(),
        router_card: "private card".into(),
        llm_score: 9,
        updated_at: 20,
        source_hash: "private-source".into(),
        prompt_hash: "private-prompt".into(),
        format_key: "fmt".into(),
    };

    db.set_skill_ai_index("shared", &public).unwrap();
    db.set_skill_ai_index_scoped("shared", Some("usr_alice"), &private)
        .unwrap();

    let loaded_public = db.skill_ai_index("shared").unwrap().unwrap();
    let loaded_private = db
        .skill_ai_index_scoped("shared", Some("usr_alice"))
        .unwrap()
        .unwrap();
    assert_eq!(loaded_public.summary, "task: public summary");
    assert_eq!(loaded_public.llm_score, 4);
    assert_eq!(loaded_private.summary, "task: private summary");
    assert_eq!(loaded_private.llm_score, 9);

    let visible_public = db.skill_ai_index_all_visible(None).unwrap();
    assert_eq!(visible_public["shared"].summary, "task: public summary");
    let visible_alice = db.skill_ai_index_all_visible(Some("usr_alice")).unwrap();
    assert_eq!(visible_alice["shared"].summary, "task: private summary");

    let all = db.skill_ai_index_all_by_resource_key().unwrap();
    assert!(all.contains_key(&Database::skill_ai_index_key(None, "shared")));
    assert!(all.contains_key(&Database::skill_ai_index_key(Some("usr_alice"), "shared")));
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

// =========================================================================
//  issue #27 — router.rs / router_stats.rs functional coverage.
//
//  Historical real bug: `router_event_by_id` dropped the `user_id` column
//  from its SELECT while `row_to_router_event` still reads it positionally
//  at index 23, so every returned event silently reports `user_id: None`
//  regardless of what is actually stored. This blind spot let the bug ship
//  to production and get discovered only by manual audit. These tests are
//  the regression gate.
// =========================================================================

/// Base fixture event. Individual tests override the fields they care about
/// via struct-update syntax so each test reads as "what's different here".
fn base_event() -> RouterEvent {
    RouterEvent {
        id: None,
        ts: 1_000,
        provider: "openai-compat".into(),
        model: "deepseek-v4-flash".into(),
        prompt_tokens: 100,
        completion_tokens: 20,
        reasoning_tokens: 5,
        total_tokens: 125,
        cache_hit_tokens: 10,
        cache_miss_tokens: 90,
        latency_ms: 250,
        chosen_skills_json: "[]".into(),
        candidate_count: 0,
        status: "ok".into(),
        error_msg: None,
        session_id: "sess-1".into(),
        mode: "compatible".into(),
        user_prompt: "help me write a test".into(),
        cwd: "/tmp/proj".into(),
        bm25_kept: 0,
        llm_raw_response: "COMPATIBLE\nfoo".into(),
        hook_output: "# runai recommend\n...".into(),
        llm_input: "candidate listing + user prompt".into(),
        intent_llm_input: "stage1 prompt".into(),
        intent_llm_output: "intent: compact task".into(),
        intent_status: "ok".into(),
        intent_error_msg: None,
        bm25_candidates_json: r#"["foo","bar"]"#.into(),
        user_id: None,
    }
}

#[test]
fn insert_router_event_roundtrip_preserves_all_fields_including_user_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    let ev = RouterEvent {
        chosen_skills_json: r#"["foo","bar"]"#.into(),
        candidate_count: 7,
        error_msg: Some("boom".into()),
        user_id: Some("usr_alice".into()),
        ..base_event()
    };
    db.insert_router_event(&ev).unwrap();

    let rows = db.router_recent_events(10).unwrap();
    assert_eq!(rows.len(), 1);
    let got = &rows[0];
    assert!(got.id.is_some(), "inserted row must have an assigned rowid");
    assert_eq!(got.ts, ev.ts);
    assert_eq!(got.provider, ev.provider);
    assert_eq!(got.model, ev.model);
    assert_eq!(got.prompt_tokens, ev.prompt_tokens);
    assert_eq!(got.completion_tokens, ev.completion_tokens);
    assert_eq!(got.reasoning_tokens, ev.reasoning_tokens);
    assert_eq!(got.total_tokens, ev.total_tokens);
    assert_eq!(got.cache_hit_tokens, ev.cache_hit_tokens);
    assert_eq!(got.cache_miss_tokens, ev.cache_miss_tokens);
    assert_eq!(got.latency_ms, ev.latency_ms);
    assert_eq!(got.chosen_skills_json, ev.chosen_skills_json);
    assert_eq!(got.candidate_count, ev.candidate_count);
    assert_eq!(got.status, ev.status);
    assert_eq!(got.error_msg, ev.error_msg);
    assert_eq!(got.session_id, ev.session_id);
    assert_eq!(got.mode, ev.mode);
    assert_eq!(got.user_prompt, ev.user_prompt);
    assert_eq!(got.cwd, ev.cwd);
    assert_eq!(got.bm25_kept, ev.bm25_kept);
    assert_eq!(got.llm_raw_response, ev.llm_raw_response);
    assert_eq!(got.hook_output, ev.hook_output);
    assert_eq!(got.llm_input, ev.llm_input);
    assert_eq!(got.intent_llm_input, ev.intent_llm_input);
    assert_eq!(got.intent_llm_output, ev.intent_llm_output);
    assert_eq!(got.intent_status, ev.intent_status);
    assert_eq!(got.intent_error_msg, ev.intent_error_msg);
    assert_eq!(got.bm25_candidates_json, ev.bm25_candidates_json);
    assert_eq!(
        got.user_id.as_deref(),
        Some("usr_alice"),
        "user_id must round-trip through router_recent_events (uses the \
         24-column SELECT, unlike router_event_by_id — see REAL_BUG test below)"
    );
}

#[test]
fn insert_router_event_caps_oversized_prompt_and_input_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    let ev = RouterEvent {
        user_prompt: "a".repeat(5_000),
        llm_raw_response: "b".repeat(5_000),
        hook_output: "c".repeat(10_000),
        llm_input: "d".repeat(100_000),
        intent_llm_input: "e".repeat(100_000),
        intent_llm_output: "f".repeat(10_000),
        bm25_candidates_json: serde_json::to_string(&vec!["skill"; 2000]).unwrap(),
        ..base_event()
    };
    db.insert_router_event(&ev).unwrap();

    let got = db.router_recent_events(1).unwrap().remove(0);
    assert_eq!(
        got.user_prompt.chars().count(),
        2000,
        "user_prompt capped at 2KB chars"
    );
    assert_eq!(
        got.llm_raw_response.chars().count(),
        2000,
        "llm_raw_response capped at 2KB chars"
    );
    assert_eq!(
        got.hook_output.chars().count(),
        6000,
        "hook_output capped at 6KB chars"
    );
    assert_eq!(
        got.llm_input.chars().count(),
        65536,
        "llm_input capped at 64KB chars"
    );
    assert_eq!(
        got.intent_llm_input.chars().count(),
        16384,
        "intent_llm_input capped at 16KB chars"
    );
    assert_eq!(
        got.intent_llm_output.chars().count(),
        2000,
        "intent_llm_output capped at 2KB chars"
    );
    assert!(
        got.bm25_candidates_json.chars().count() <= 12000,
        "bm25_candidates_json capped for dashboard detail"
    );
}

#[test]
fn router_event_by_id_finds_row_and_reports_none_for_unknown_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    let ev = RouterEvent {
        session_id: "sess-lookup".into(),
        model: "gpt-lookup".into(),
        chosen_skills_json: r#"["alpha"]"#.into(),
        ..base_event()
    };
    db.insert_router_event(&ev).unwrap();
    let id = db.router_recent_events(1).unwrap()[0].id.unwrap();

    let found = db.router_event_by_id(id).unwrap().unwrap();
    assert_eq!(found.id, Some(id));
    assert_eq!(found.session_id, "sess-lookup");
    assert_eq!(found.model, "gpt-lookup");
    assert_eq!(found.chosen_skills_json, r#"["alpha"]"#);

    assert!(
        db.router_event_by_id(id + 999).unwrap().is_none(),
        "unknown id must return None, not error"
    );
}

/// Regression pin for github.com/Crosery/runai/issues/33: `router_event_by_id`'s
/// SELECT used to omit the `user_id` column while `row_to_router_event` read
/// it positionally at index 23 — an out-of-range read that
/// `Row::get::<_, Option<_>>` swallowed via `unwrap_or_default()`, so the
/// returned event's `user_id` was always `None` even when the stored row had
/// one. This is the same failure class as the historical "router_event_by_id
/// 漏 user_id 列" incident. Fixed by adding `user_id` to the SELECT.
#[test]
fn router_event_by_id_preserves_user_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    let ev = RouterEvent {
        user_id: Some("usr_alice".into()),
        ..base_event()
    };
    db.insert_router_event(&ev).unwrap();
    let id = db.router_recent_events(1).unwrap()[0].id.unwrap();

    let found = db.router_event_by_id(id).unwrap().unwrap();
    assert_eq!(
        found.user_id.as_deref(),
        Some("usr_alice"),
        "router_event_by_id must preserve user_id like router_recent_events does"
    );
}

#[test]
fn router_events_for_skill_matches_exact_name_not_substring() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    // "foo" must not match an event whose chosen array only contains
    // "foobar" — this is exactly what json_each (vs a LIKE '%foo%') buys us.
    db.insert_router_event(&RouterEvent {
        ts: 1,
        chosen_skills_json: r#"["foo"]"#.into(),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 2,
        chosen_skills_json: r#"["foobar"]"#.into(),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 3,
        chosen_skills_json: r#"["bar","foo"]"#.into(),
        ..base_event()
    })
    .unwrap();

    let hits = db.router_events_for_skill("foo", 10).unwrap();
    assert_eq!(hits.len(), 2, "foobar must not count as a foo hit");
    // ORDER BY ts DESC — the ts=3 row comes first.
    assert_eq!(hits[0].ts, 3);
    assert_eq!(hits[1].ts, 1);

    // limit is honored.
    let limited = db.router_events_for_skill("foo", 1).unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].ts, 3);
}

#[test]
fn record_session_adoption_is_idempotent_and_ignores_empty_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    db.record_session_adoption("sess-a", "skill-one").unwrap();
    db.record_session_adoption("sess-a", "skill-one").unwrap(); // repeat signal
    db.record_session_adoption("sess-a", "skill-two").unwrap();

    let count: i64 = db
        .conn_ref()
        .query_row(
            "SELECT COUNT(*) FROM router_session_adoptions WHERE session_id = 'sess-a' AND skill_name = 'skill-one'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "PK (session_id, skill_name) must collapse repeats"
    );

    // Empty session_id / skill_name are explicit no-ops, not errors.
    db.record_session_adoption("", "skill-three").unwrap();
    db.record_session_adoption("sess-a", "").unwrap();
    let total: i64 = db
        .conn_ref()
        .query_row("SELECT COUNT(*) FROM router_session_adoptions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        total, 2,
        "empty session_id/skill_name must not insert a row"
    );
}

#[test]
fn router_session_routed_skills_dedups_and_sorts() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    assert_eq!(
        db.router_session_routed_skills("").unwrap(),
        Vec::<String>::new(),
        "empty session_id short-circuits without a query"
    );
    assert_eq!(
        db.router_session_routed_skills("no-such-session").unwrap(),
        Vec::<String>::new()
    );

    db.record_session_adoption("sess-x", "zeta").unwrap();
    db.record_session_adoption("sess-x", "alpha").unwrap();
    db.record_session_adoption("sess-x", "alpha").unwrap();
    db.record_session_adoption("sess-y", "other-session-skill")
        .unwrap();

    let routed = db.router_session_routed_skills("sess-x").unwrap();
    assert_eq!(
        routed,
        vec!["alpha".to_string(), "zeta".to_string()],
        "deduped (BTreeSet) and alphabetically sorted"
    );
}

#[test]
fn router_session_recommended_skills_dedups_preserving_newest_first_order() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    assert_eq!(
        db.router_session_recommended_skills("").unwrap(),
        Vec::<String>::new()
    );

    // Older event (ts=1): recommended a, b.
    db.insert_router_event(&RouterEvent {
        ts: 1,
        session_id: "sess-rec".into(),
        status: "ok".into(),
        chosen_skills_json: r#"["a","b"]"#.into(),
        ..base_event()
    })
    .unwrap();
    // Newer event (ts=2): recommended b, c.
    db.insert_router_event(&RouterEvent {
        ts: 2,
        session_id: "sess-rec".into(),
        status: "ok".into(),
        chosen_skills_json: r#"["b","c"]"#.into(),
        ..base_event()
    })
    .unwrap();
    // Error-status event must be excluded even though it has a chosen array.
    db.insert_router_event(&RouterEvent {
        ts: 3,
        session_id: "sess-rec".into(),
        status: "error".into(),
        chosen_skills_json: r#"["should-not-appear"]"#.into(),
        ..base_event()
    })
    .unwrap();

    let names = db.router_session_recommended_skills("sess-rec").unwrap();
    // Rows are walked newest-first (ORDER BY ts DESC), so ts=2 ("b","c") is
    // seen before ts=1 ("a","b") -> first-seen order is b, c, a.
    assert_eq!(
        names,
        vec!["b".to_string(), "c".to_string(), "a".to_string()]
    );
}

#[test]
fn router_session_turn_history_orders_ascending_and_respects_limit_and_status() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    assert_eq!(
        db.router_session_turn_history("", 10).unwrap(),
        Vec::<(String, String)>::new()
    );

    for (ts, input, output) in [
        (10, "turn1-in", "turn1-out"),
        (20, "turn2-in", "turn2-out"),
        (30, "turn3-in", "turn3-out"),
    ] {
        db.insert_router_event(&RouterEvent {
            ts,
            session_id: "sess-turns".into(),
            status: "ok".into(),
            llm_input: input.into(),
            llm_raw_response: output.into(),
            ..base_event()
        })
        .unwrap();
    }
    // Error-status turn must never show up in conversation replay.
    db.insert_router_event(&RouterEvent {
        ts: 40,
        session_id: "sess-turns".into(),
        status: "error".into(),
        llm_input: "turn4-in".into(),
        llm_raw_response: "turn4-out".into(),
        ..base_event()
    })
    .unwrap();

    let all = db.router_session_turn_history("sess-turns", 10).unwrap();
    assert_eq!(
        all,
        vec![
            ("turn1-in".to_string(), "turn1-out".to_string()),
            ("turn2-in".to_string(), "turn2-out".to_string()),
            ("turn3-in".to_string(), "turn3-out".to_string()),
        ],
        "ASC by ts, error-status row excluded"
    );

    let limited = db.router_session_turn_history("sess-turns", 2).unwrap();
    assert_eq!(
        limited,
        vec![
            ("turn1-in".to_string(), "turn1-out".to_string()),
            ("turn2-in".to_string(), "turn2-out".to_string()),
        ],
        "limit caps the ASC-ordered result, keeping the oldest turns"
    );
}

#[test]
fn router_recent_events_orders_desc_and_respects_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    for ts in [100, 200, 300, 400, 500] {
        db.insert_router_event(&RouterEvent { ts, ..base_event() })
            .unwrap();
    }

    let recent = db.router_recent_events(3).unwrap();
    assert_eq!(
        recent.iter().map(|e| e.ts).collect::<Vec<_>>(),
        vec![500, 400, 300],
        "most recent 3, newest first"
    );
}

#[test]
fn router_events_paged_filtered_applies_since_ts_model_and_hit_only() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    // ts=100 model-a miss (empty chosen).
    db.insert_router_event(&RouterEvent {
        ts: 100,
        model: "model-a".into(),
        status: "ok".into(),
        chosen_skills_json: "[]".into(),
        ..base_event()
    })
    .unwrap();
    // ts=200 model-a hit.
    db.insert_router_event(&RouterEvent {
        ts: 200,
        model: "model-a".into(),
        status: "ok".into(),
        chosen_skills_json: r#"["x"]"#.into(),
        ..base_event()
    })
    .unwrap();
    // ts=300 model-b hit.
    db.insert_router_event(&RouterEvent {
        ts: 300,
        model: "model-b".into(),
        status: "ok".into(),
        chosen_skills_json: r#"["y"]"#.into(),
        ..base_event()
    })
    .unwrap();
    // ts=400 model-a error (should be excluded by hit_only even though model matches).
    db.insert_router_event(&RouterEvent {
        ts: 400,
        model: "model-a".into(),
        status: "error".into(),
        chosen_skills_json: r#"["z"]"#.into(),
        ..base_event()
    })
    .unwrap();

    // No filters: everything, newest first.
    let all = db.router_events_paged(None, 100, 0, None, false).unwrap();
    assert_eq!(
        all.iter().map(|e| e.ts).collect::<Vec<_>>(),
        vec![400, 300, 200, 100]
    );

    // since_ts = 200: rows with ts >= 200 only.
    let since = db
        .router_events_paged(Some(200), 100, 0, None, false)
        .unwrap();
    assert_eq!(
        since.iter().map(|e| e.ts).collect::<Vec<_>>(),
        vec![400, 300, 200]
    );

    // model filter: only model-a rows.
    let by_model = db
        .router_events_paged(None, 100, 0, Some("model-a"), false)
        .unwrap();
    assert_eq!(
        by_model.iter().map(|e| e.ts).collect::<Vec<_>>(),
        vec![400, 200, 100]
    );

    // hit_only: status='ok' AND chosen_skills_json != '[]' -> ts 300, 200.
    let hits = db.router_events_paged(None, 100, 0, None, true).unwrap();
    assert_eq!(
        hits.iter().map(|e| e.ts).collect::<Vec<_>>(),
        vec![300, 200]
    );

    // Combine model + hit_only: model-a hit only -> ts=200 (400 is error, 100 is miss).
    let combo = db
        .router_events_paged(None, 100, 0, Some("model-a"), true)
        .unwrap();
    assert_eq!(combo.iter().map(|e| e.ts).collect::<Vec<_>>(), vec![200]);

    // Pagination: limit=1 offset=1 over the unfiltered set -> second-newest.
    let paged = db.router_events_paged(None, 1, 1, None, false).unwrap();
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0].ts, 300);
}

#[test]
fn router_events_paged_filtered_scopes_by_user_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    db.insert_router_event(&RouterEvent {
        ts: 1,
        user_id: Some("alice".into()),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 2,
        user_id: Some("bob".into()),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 3,
        user_id: None,
        ..base_event()
    })
    .unwrap();

    let alice_only = db
        .router_events_paged_filtered(None, 100, 0, None, false, Some("alice"))
        .unwrap();
    assert_eq!(alice_only.len(), 1);
    assert_eq!(alice_only[0].ts, 1);
    assert_eq!(alice_only[0].user_id.as_deref(), Some("alice"));

    let unscoped = db
        .router_events_paged_filtered(None, 100, 0, None, false, None)
        .unwrap();
    assert_eq!(
        unscoped.len(),
        3,
        "None scope = every row, admin/compat view"
    );
}

#[test]
fn router_events_count_filtered_matches_paged_result_lengths() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    db.insert_router_event(&RouterEvent {
        ts: 1,
        model: "m1".into(),
        status: "ok".into(),
        chosen_skills_json: r#"["a"]"#.into(),
        user_id: Some("alice".into()),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 2,
        model: "m2".into(),
        status: "error".into(),
        chosen_skills_json: "[]".into(),
        user_id: Some("bob".into()),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 3,
        model: "m1".into(),
        status: "ok".into(),
        chosen_skills_json: r#"["b"]"#.into(),
        user_id: None,
        ..base_event()
    })
    .unwrap();

    assert_eq!(db.router_events_count(None, None, false).unwrap(), 3);
    assert_eq!(db.router_events_count(Some(2), None, false).unwrap(), 2);
    assert_eq!(db.router_events_count(None, Some("m1"), false).unwrap(), 2);
    assert_eq!(db.router_events_count(None, None, true).unwrap(), 2);
    assert_eq!(
        db.router_events_count_filtered(None, None, false, Some("alice"))
            .unwrap(),
        1
    );
    assert_eq!(
        db.router_events_count_filtered(None, None, false, None)
            .unwrap(),
        3
    );

    // Cross-check against the paged variant for one non-trivial combo.
    let paged_m1_hits = db
        .router_events_paged(None, 100, 0, Some("m1"), true)
        .unwrap();
    assert_eq!(
        paged_m1_hits.len() as i64,
        db.router_events_count(None, Some("m1"), true).unwrap()
    );
}

#[test]
fn router_events_since_ordered_orders_by_session_then_ts_and_excludes_empty_session() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    // Interleaved ts across two sessions — result must group by session_id
    // then order by ts within the group, per the SQL ORDER BY.
    db.insert_router_event(&RouterEvent {
        ts: 10,
        session_id: "sess-b".into(),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 5,
        session_id: "sess-a".into(),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 20,
        session_id: "sess-a".into(),
        ..base_event()
    })
    .unwrap();
    // Empty session_id must never appear (feedback mining needs a real session).
    db.insert_router_event(&RouterEvent {
        ts: 15,
        session_id: "".into(),
        ..base_event()
    })
    .unwrap();
    // Before the `since_ts` cutoff — excluded.
    db.insert_router_event(&RouterEvent {
        ts: 1,
        session_id: "sess-a".into(),
        ..base_event()
    })
    .unwrap();

    let rows = db.router_events_since_ordered(5).unwrap();
    let got: Vec<(String, i64)> = rows.iter().map(|e| (e.session_id.clone(), e.ts)).collect();
    assert_eq!(
        got,
        vec![
            ("sess-a".to_string(), 5),
            ("sess-a".to_string(), 20),
            ("sess-b".to_string(), 10),
        ],
        "grouped by session_id, ascending ts within each group, ts=1 dropped by since_ts, empty session_id dropped"
    );
}

/// Regression pin for github.com/Crosery/runai/issues/33 (same root cause as
/// `router_event_by_id`, see above): this SELECT used to also omit
/// `user_id`, so every row this function returned reported `user_id: None`
/// even when the underlying row had one set. This function currently has no
/// in-tree caller (feedback mining is not wired up yet), so the blast radius
/// was latent rather than live — but it was the same landmine, fixed in the
/// same pass as `router_event_by_id`.
#[test]
fn router_events_since_ordered_preserves_user_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    db.insert_router_event(&RouterEvent {
        ts: 5,
        session_id: "sess-a".into(),
        user_id: Some("usr_alice".into()),
        ..base_event()
    })
    .unwrap();

    let rows = db.router_events_since_ordered(0).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].user_id.as_deref(), Some("usr_alice"));
}

#[test]
fn router_stats_summary_filtered_aggregates_tokens_errors_latency_and_per_model() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    db.insert_router_event(&RouterEvent {
        ts: 100,
        model: "model-a".into(),
        status: "ok".into(),
        prompt_tokens: 10,
        completion_tokens: 5,
        reasoning_tokens: 1,
        total_tokens: 16,
        latency_ms: 200,
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 200,
        model: "model-a".into(),
        status: "ok".into(),
        prompt_tokens: 20,
        completion_tokens: 10,
        reasoning_tokens: 2,
        total_tokens: 32,
        latency_ms: 400,
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 300,
        model: "model-b".into(),
        status: "error".into(),
        prompt_tokens: 5,
        completion_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 5,
        latency_ms: 999_999, // error rows must not pollute avg_latency_ms
        ..base_event()
    })
    .unwrap();

    let summary = db.router_stats_summary(None).unwrap();
    assert_eq!(summary.total_calls, 3);
    assert_eq!(summary.total_prompt_tokens, 35);
    assert_eq!(summary.total_completion_tokens, 15);
    assert_eq!(summary.total_reasoning_tokens, 3);
    assert_eq!(summary.total_tokens, 53);
    assert_eq!(summary.errors, 1);
    assert_eq!(
        summary.avg_latency_ms,
        Some(300.0),
        "avg over ok-status rows only: (200+400)/2"
    );
    assert_eq!(summary.per_model.len(), 2);
    // ORDER BY total_tokens DESC -> model-a (48) before model-b (5).
    assert_eq!(summary.per_model[0].model, "model-a");
    assert_eq!(summary.per_model[0].calls, 2);
    assert_eq!(summary.per_model[0].total_tokens, 48);
    assert_eq!(summary.per_model[1].model, "model-b");
    assert_eq!(summary.per_model[1].total_tokens, 5);

    // since_ts cuts off the ts=100 row.
    let since = db.router_stats_summary(Some(200)).unwrap();
    assert_eq!(since.total_calls, 2);
    assert_eq!(since.total_prompt_tokens, 25);
}

#[test]
fn router_stats_summary_filtered_scopes_by_user_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    db.insert_router_event(&RouterEvent {
        ts: 1,
        total_tokens: 10,
        user_id: Some("alice".into()),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: 2,
        total_tokens: 999,
        user_id: Some("bob".into()),
        ..base_event()
    })
    .unwrap();

    let alice_scope = db
        .router_stats_summary_filtered(None, Some("alice"))
        .unwrap();
    assert_eq!(alice_scope.total_calls, 1);
    assert_eq!(alice_scope.total_tokens, 10);

    let unscoped = db.router_stats_summary_filtered(None, None).unwrap();
    assert_eq!(unscoped.total_calls, 2);
    assert_eq!(unscoped.total_tokens, 1009);
}

#[test]
fn router_timeline_filtered_buckets_counts_hits_errors_and_latency() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    let now = chrono::Utc::now().timestamp();
    let bucket_secs = 3600; // wide buckets so test timing jitter can't flip a bucket boundary
    let buckets = 2;
    let start = now - bucket_secs * buckets;

    // Bucket 0 spans [start, start+3600): one hit at start+10.
    db.insert_router_event(&RouterEvent {
        ts: start + 10,
        status: "ok".into(),
        chosen_skills_json: r#"["hit"]"#.into(),
        latency_ms: 100,
        ..base_event()
    })
    .unwrap();
    // Bucket 1 spans [start+3600, start+7200): one error + one ok-miss.
    db.insert_router_event(&RouterEvent {
        ts: start + 3600 + 5,
        status: "error".into(),
        chosen_skills_json: "[]".into(),
        latency_ms: 50,
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: start + 3600 + 6,
        status: "ok".into(),
        chosen_skills_json: "[]".into(),
        latency_ms: 300,
        ..base_event()
    })
    .unwrap();

    let timeline = db.router_timeline(bucket_secs, buckets).unwrap();
    assert_eq!(
        timeline.len(),
        2,
        "always returns exactly `buckets` entries"
    );
    assert_eq!(timeline[0].total, 1);
    assert_eq!(timeline[0].hits, 1);
    assert_eq!(timeline[0].errors, 0);
    assert_eq!(timeline[0].avg_latency_ms, 100.0);

    assert_eq!(timeline[1].total, 2);
    assert_eq!(
        timeline[1].hits, 0,
        "ok-status but empty chosen array is not a hit"
    );
    assert_eq!(timeline[1].errors, 1);
    assert_eq!(
        timeline[1].avg_latency_ms, 175.0,
        "(50+300)/2 averaged over both rows"
    );
}

#[test]
fn router_timeline_filtered_scopes_by_user_id() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    let now = chrono::Utc::now().timestamp();
    let bucket_secs = 3600;
    let buckets = 1;
    let start = now - bucket_secs * buckets;

    db.insert_router_event(&RouterEvent {
        ts: start + 10,
        user_id: Some("alice".into()),
        ..base_event()
    })
    .unwrap();
    db.insert_router_event(&RouterEvent {
        ts: start + 20,
        user_id: Some("bob".into()),
        ..base_event()
    })
    .unwrap();

    let alice_timeline = db
        .router_timeline_filtered(bucket_secs, buckets, Some("alice"))
        .unwrap();
    assert_eq!(alice_timeline[0].total, 1);

    let all_timeline = db
        .router_timeline_filtered(bucket_secs, buckets, None)
        .unwrap();
    assert_eq!(all_timeline[0].total, 2);
}

// =========================================================================
//  skill_feedback + skill_router_stats (v26 — skill feedback radar).
// =========================================================================

#[test]
fn skill_feedback_record_and_recent_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("feedback.db")).unwrap();

    let id1 = db
        .record_skill_feedback(
            1_000,
            "pdf-extract",
            None,
            Some("alice"),
            Some("rnai_sess_a"),
            Some(42),
            1,
            Some("worked great"),
        )
        .unwrap();
    let id2 = db
        .record_skill_feedback(
            2_000,
            "pdf-extract",
            None,
            Some("bob"),
            None,
            None,
            -1,
            None,
        )
        .unwrap();
    assert_ne!(id1, id2, "each feedback event gets its own row id");

    let recent: Vec<SkillFeedbackRow> = db.recent_skill_feedback("pdf-extract", 10).unwrap();
    assert_eq!(recent.len(), 2);
    // Newest first.
    assert_eq!(recent[0].id, id2);
    assert_eq!(recent[0].ts, 2_000);
    assert_eq!(recent[0].verdict, -1);
    assert_eq!(recent[0].user_id.as_deref(), Some("bob"));
    assert_eq!(recent[0].session_id, None);
    assert_eq!(recent[0].event_id, None);
    assert_eq!(recent[0].note, None);

    assert_eq!(recent[1].id, id1);
    assert_eq!(recent[1].skill_name, "pdf-extract");
    assert_eq!(recent[1].owner_user_id, None);
    assert_eq!(recent[1].user_id.as_deref(), Some("alice"));
    assert_eq!(recent[1].session_id.as_deref(), Some("rnai_sess_a"));
    assert_eq!(recent[1].event_id, Some(42));
    assert_eq!(recent[1].verdict, 1);
    assert_eq!(recent[1].note.as_deref(), Some("worked great"));
}

#[test]
fn skill_feedback_rejects_non_unit_verdict() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("feedback.db")).unwrap();

    for bad in [0_i64, 2, -2, 100] {
        let err = db
            .record_skill_feedback(1_000, "foo", None, None, None, None, bad, None)
            .expect_err("non +-1 verdict must be rejected");
        assert!(err.to_string().contains("+1 or -1"));
    }
    assert!(
        db.recent_skill_feedback("foo", 10).unwrap().is_empty(),
        "rejected verdicts must not leave a row behind"
    );
}

#[test]
fn skill_feedback_counts_are_owner_null_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("feedback.db")).unwrap();

    // A public-pool "dup" skill and a same-named private skill owned by u1
    // must never share feedback counts.
    db.record_skill_feedback(1_000, "dup", None, Some("alice"), None, None, 1, None)
        .unwrap();
    db.record_skill_feedback(1_001, "dup", None, Some("carol"), None, None, 1, None)
        .unwrap();
    db.record_skill_feedback(1_002, "dup", Some("u1"), Some("bob"), None, None, -1, None)
        .unwrap();

    let public_counts = db.skill_feedback_counts("dup", None).unwrap();
    assert_eq!(public_counts, (2, 0), "public pool sees only its own votes");

    let private_counts = db.skill_feedback_counts("dup", Some("u1")).unwrap();
    assert_eq!(
        private_counts,
        (0, 1),
        "u1's private dup sees only its own vote, not the public pool's"
    );

    let other_owner_counts = db.skill_feedback_counts("dup", Some("u2")).unwrap();
    assert_eq!(
        other_owner_counts,
        (0, 0),
        "an owner with no feedback rows gets zero counts, not a cross-owner leak"
    );
}

#[test]
fn skill_feedback_counts_all_aggregates_across_owners() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("feedback.db")).unwrap();

    db.record_skill_feedback(1_000, "dup", None, None, None, None, 1, None)
        .unwrap();
    db.record_skill_feedback(1_001, "dup", Some("u1"), None, None, None, -1, None)
        .unwrap();
    db.record_skill_feedback(1_002, "dup", Some("u2"), None, None, None, 1, None)
        .unwrap();
    db.record_skill_feedback(1_003, "solo", None, None, None, None, 1, None)
        .unwrap();

    let all = db.skill_feedback_counts_all().unwrap();
    assert_eq!(
        all.get("dup").copied(),
        Some((2, 1)),
        "counts_all merges every owner-pool instance of the same skill name"
    );
    assert_eq!(all.get("solo").copied(), Some((1, 0)));
    assert!(all.get("missing").is_none());
}

/// Covers the four funnel states the router cares about: a candidate that
/// was never chosen, a chosen skill whose session never adopted it, a chosen
/// skill that WAS adopted, and a stale event before `since_ts` that must be
/// excluded entirely.
#[test]
fn skill_router_stats_counts_funnel_and_respects_since_ts() {
    let tmp = tempfile::tempdir().unwrap();
    let db = Database::open(&tmp.path().join("router.db")).unwrap();

    db.insert_router_event(&RouterEvent {
        ts: 2_000,
        session_id: "s1".into(),
        bm25_candidates_json: serde_json::to_string(&[
            "cand_only",
            "chosen_no_adopt",
            "chosen_and_adopted",
        ])
        .unwrap(),
        chosen_skills_json: serde_json::to_string(&["chosen_no_adopt", "chosen_and_adopted"])
            .unwrap(),
        ..base_event()
    })
    .unwrap();
    db.record_session_adoption("s1", "chosen_and_adopted")
        .unwrap();

    // Predates the since_ts cutoff below — must be excluded entirely.
    db.insert_router_event(&RouterEvent {
        ts: 100,
        session_id: "s_old".into(),
        bm25_candidates_json: serde_json::to_string(&["before_cutoff"]).unwrap(),
        chosen_skills_json: serde_json::to_string(&["before_cutoff"]).unwrap(),
        ..base_event()
    })
    .unwrap();

    let stats: HashMap<String, RouterSkillStats> = db.skill_router_stats(1_000).unwrap();

    let cand_only = stats.get("cand_only").copied().unwrap();
    assert_eq!(cand_only.candidate_events, 1);
    assert_eq!(
        cand_only.chosen_events, 0,
        "candidate that was never chosen"
    );
    assert_eq!(cand_only.chosen_sessions, 0);
    assert_eq!(cand_only.adopted_sessions, 0);

    let chosen_no_adopt = stats.get("chosen_no_adopt").copied().unwrap();
    assert_eq!(chosen_no_adopt.candidate_events, 1);
    assert_eq!(chosen_no_adopt.chosen_events, 1);
    assert_eq!(chosen_no_adopt.chosen_sessions, 1);
    assert_eq!(
        chosen_no_adopt.adopted_sessions, 0,
        "chosen but the session never recorded an adoption"
    );

    let chosen_and_adopted = stats.get("chosen_and_adopted").copied().unwrap();
    assert_eq!(chosen_and_adopted.candidate_events, 1);
    assert_eq!(chosen_and_adopted.chosen_events, 1);
    assert_eq!(chosen_and_adopted.chosen_sessions, 1);
    assert_eq!(
        chosen_and_adopted.adopted_sessions, 1,
        "chosen AND the session recorded an adoption"
    );

    assert!(
        stats.get("before_cutoff").is_none(),
        "since_ts must exclude events entirely, not just zero their counts"
    );
}

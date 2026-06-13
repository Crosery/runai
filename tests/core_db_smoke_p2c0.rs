//! P2 e2e regression tests for core::db, core::recommend::enrich,
//! core::bm25, core::backup. Cloud HEAD here predates multi-user (v15+),
//! so tests target the actual public API of these modules at this commit.

use runai::core::bm25::{contains_cjk, is_cjk, rank, tokenize};

// ============================================================================
// core::bm25
// ============================================================================

#[test]
fn bm25_tokenize_english_words_and_dashes_smoke() {
    // English words split by whitespace AND dash/underscore are lowercased
    // single tokens. "foo-bar baz_qux" must yield 4 tokens.
    let t = tokenize("Hello WORLD foo-bar baz_qux");
    assert!(t.contains(&"hello".to_string()), "missing 'hello': {:?}", t);
    assert!(t.contains(&"world".to_string()));
    assert!(t.contains(&"foo".to_string()));
    assert!(t.contains(&"bar".to_string()));
    assert!(t.contains(&"baz".to_string()));
    assert!(t.contains(&"qux".to_string()));
    // No uppercase or dash survives
    for tok in &t {
        assert!(
            !tok.contains('-') && !tok.contains('_'),
            "tok '{}' should have no dash/underscore",
            tok
        );
        assert_eq!(
            tok.to_lowercase(),
            *tok,
            "tok '{}' should be lowercase",
            tok
        );
    }
}

#[test]
fn bm25_tokenize_cjk_unigrams_and_bigrams_smoke() {
    // tokenize emits CJK unigrams AND adjacent-pair bigrams, plus latin words.
    // "做ppt视频" → "视" "频" unigrams + "视频" bigram + "ppt" latin.
    let t = tokenize("做ppt视频");
    assert!(t.contains(&"ppt".to_string()), "missing latin ppt: {:?}", t);
    // Content unigrams retained
    assert!(
        t.contains(&"视".to_string()),
        "missing '视' unigram: {:?}",
        t
    );
    assert!(
        t.contains(&"频".to_string()),
        "missing '频' unigram: {:?}",
        t
    );
    // Adjacent-pair bigram emitted
    assert!(
        t.contains(&"视频".to_string()),
        "missing '视频' bigram: {:?}",
        t
    );
}

#[test]
fn bm25_tokenize_cjk_stop_unigrams_filtered_smoke() {
    // High-frequency CJK stopwords are dropped from the unigram stream so
    // BM25 doesn't false-positive on them.
    let t = tokenize("的了和我做");
    // bare stop-unigrams must NOT appear standalone
    for stop in &["的", "了", "和", "我", "做"] {
        assert!(
            !t.contains(&stop.to_string()),
            "stop unigram '{}' must be filtered, got {:?}",
            stop,
            t
        );
    }
}

#[test]
fn bm25_rank_empty_inputs_smoke() {
    // Empty docs OR empty query both return Vec::new(); no panic.
    let empty: Vec<&str> = Vec::new();
    assert!(rank("query", &empty).is_empty());
    let docs = vec!["foo doc", "bar doc"];
    assert!(rank("", &docs).is_empty());
    // Whitespace-only query → no tokens → empty
    assert!(rank("   ", &docs).is_empty());
}

#[test]
fn bm25_rank_scoring_and_length_norm_smoke() {
    // BM25 ranks docs containing query terms higher; longer docs penalized
    // by B=0.75 length normalization. Doc with the rare term in a short doc
    // should rank higher than doc with the rare term diluted in a long doc.
    let docs = vec![
        "python basics tutorial",                    // 3 tokens
        "ruby guide programming language reference", // 5 tokens
        "python advanced reference",                 // 3 tokens
    ];
    let scores = rank("python", &docs);
    assert_eq!(scores.len(), 3);
    // doc[0] and doc[2] contain "python"; doc[1] does not
    let s0 = scores
        .iter()
        .find(|(i, _)| *i == 0)
        .map(|(_, s)| *s)
        .unwrap();
    let s1 = scores
        .iter()
        .find(|(i, _)| *i == 1)
        .map(|(_, s)| *s)
        .unwrap();
    let s2 = scores
        .iter()
        .find(|(i, _)| *i == 2)
        .map(|(_, s)| *s)
        .unwrap();
    assert!(
        s0 > s1,
        "doc[0] (has python) > doc[1] (no python): {} > {}",
        s0,
        s1
    );
    assert!(
        s2 > s1,
        "doc[2] (has python) > doc[1] (no python): {} > {}",
        s2,
        s1
    );
    // Top-ranked must be one of the two python docs, not the ruby doc
    assert!(
        scores[0].0 == 0 || scores[0].0 == 2,
        "top should be a python doc, got idx {}",
        scores[0].0
    );
    // Doc 1 (no python) gets zero score for query "python"
    assert_eq!(s1, 0.0, "ruby doc must score 0 for python query");
}

#[test]
fn bm25_rank_exact_keyword_match_cjk_doc_smoke() {
    // BM25 must find a doc by exact latin keyword even when the doc has
    // CJK context around it. This is the recommend-prefilter contract:
    // a user typing 'ppt' must hit a CJK skill that says 'ppt' in trigger.
    let docs = vec![
        "pptmaker: 幻灯片生成 ppt 演示",
        "image processing python",
        "git commit helper",
    ];
    let scores = rank("ppt", &docs);
    assert_eq!(scores.len(), 3);
    assert_eq!(scores[0].0, 0, "ppt doc must rank first, got {:?}", scores);
    assert!(scores[0].1 > 0.0, "score should be > 0 for matching doc");
    // Non-matching docs score zero
    let s1 = scores
        .iter()
        .find(|(i, _)| *i == 1)
        .map(|(_, s)| *s)
        .unwrap();
    let s2 = scores
        .iter()
        .find(|(i, _)| *i == 2)
        .map(|(_, s)| *s)
        .unwrap();
    assert_eq!(s1, 0.0);
    assert_eq!(s2, 0.0);
}

#[test]
fn bm25_is_cjk_and_contains_cjk_smoke() {
    // is_cjk: single-char classifier covers CJK Unified + Ext A + kana + hangul.
    assert!(is_cjk('做'));
    assert!(is_cjk('世'));
    assert!(!is_cjk('a'));
    assert!(!is_cjk(' '));
    assert!(!is_cjk('1'));
    // contains_cjk: any CJK char in string
    assert!(contains_cjk("做个 ppt"));
    assert!(contains_cjk("hello 世界"));
    assert!(!contains_cjk("ascii only"));
    assert!(!contains_cjk(""));
}

// ============================================================================
// core::backup
// ============================================================================

#[cfg(not(target_os = "windows"))]
mod backup_tests {
    use runai::core::backup::{create_backup, has_backup, list_backups, restore_backup};
    use runai::core::paths::AppPaths;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Run a closure with HOME set to a tempdir. Tests are serialized
    /// by --test-threads=1 so this is safe.
    fn with_isolated_home<F: FnOnce(&PathBuf, &AppPaths)>(f: F) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().to_path_buf();
        std::fs::create_dir_all(&home).unwrap();
        let data = home.join(".runai");
        let paths = AppPaths::with_base(data);
        paths.ensure_dirs().unwrap();

        let orig_home = std::env::var_os("HOME");
        let orig_rdd = std::env::var_os("RUNE_DATA_DIR");
        // SAFETY: cargo test runs with --test-threads=1; HOME/RUNE_DATA_DIR
        // are only read elsewhere by code under test serialized through this
        // single thread.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("RUNE_DATA_DIR", paths.data_dir());
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&home, &paths)));

        unsafe {
            match orig_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match orig_rdd {
                Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
                None => std::env::remove_var("RUNE_DATA_DIR"),
            }
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    #[test]
    fn backup_preserves_symlinks_not_dereferenced() {
        // create_backup must copy symlinks as symlinks (preserved targets),
        // NOT dereference them. This is load-bearing: symlinks encode
        // "which skills are enabled" state. Dereferencing = lost meaning.
        with_isolated_home(|home, paths| {
            let claude_skills = home.join(".claude/skills");
            std::fs::create_dir_all(&claude_skills).unwrap();
            // Create a managed skill the symlink points at (so symlink is valid)
            let real_skill = paths.skills_dir().join("real");
            std::fs::create_dir_all(&real_skill).unwrap();
            std::fs::write(real_skill.join("SKILL.md"), "# real").unwrap();

            let link_path = claude_skills.join("link-skill");
            std::os::unix::fs::symlink(&real_skill, &link_path).unwrap();
            assert!(
                link_path
                    .symlink_metadata()
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );

            let backup_dir = create_backup(paths).expect("create_backup");

            let backed_up_link = backup_dir.join("claude-skills").join("link-skill");
            let md =
                std::fs::symlink_metadata(&backed_up_link).expect("backed up symlink should exist");
            assert!(
                md.file_type().is_symlink(),
                "backed up entry must be a symlink, was: {:?}",
                md.file_type()
            );
            let resolved = std::fs::read_link(&backed_up_link).unwrap();
            assert_eq!(resolved, real_skill, "symlink target should be preserved");
        });
    }

    #[test]
    fn backup_snapshots_managed_data_and_cli_configs() {
        // create_backup captures managed skills + managed MCPs + ~/.claude.json.
        with_isolated_home(|home, paths| {
            // managed skill
            std::fs::create_dir_all(paths.skills_dir().join("alpha")).unwrap();
            std::fs::write(paths.skills_dir().join("alpha/SKILL.md"), "# alpha").unwrap();
            // managed MCP backup
            std::fs::create_dir_all(paths.mcps_dir()).unwrap();
            std::fs::write(
                paths.mcps_dir().join("pencil.json"),
                r#"{"command":"pencil"}"#,
            )
            .unwrap();
            // claude config + gemini settings
            std::fs::write(home.join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();
            std::fs::create_dir_all(home.join(".gemini")).unwrap();
            std::fs::write(home.join(".gemini/settings.json"), r#"{"a":1}"#).unwrap();

            let backup_dir = create_backup(paths).expect("create_backup");

            assert!(backup_dir.join("timestamp").exists(), "timestamp marker");
            assert!(
                backup_dir.join("managed-skills/alpha/SKILL.md").exists(),
                "managed skill copied"
            );
            assert!(
                backup_dir.join("managed-mcps/pencil.json").exists(),
                "managed MCP backup copied"
            );
            assert!(
                backup_dir.join("claude.json").exists(),
                "claude config copied"
            );
            assert!(
                backup_dir.join("gemini-settings.json").exists(),
                "gemini settings copied"
            );
        });
    }

    #[test]
    fn list_backups_returns_newest_first() {
        // list_backups returns timestamps sorted lexicographically descending,
        // which for the YYYYMMDD_HHMMSS format equals newest first.
        with_isolated_home(|_home, paths| {
            let bdir = paths.data_dir().join("backups");
            std::fs::create_dir_all(bdir.join("20260101_100000")).unwrap();
            std::fs::create_dir_all(bdir.join("20260102_100000")).unwrap();
            std::fs::create_dir_all(bdir.join("20260101_150000")).unwrap();

            let list = list_backups(paths);
            assert_eq!(
                list,
                vec!["20260102_100000", "20260101_150000", "20260101_100000"],
                "expected newest first"
            );
        });
    }

    #[test]
    fn restore_backup_overlays_into_managed_dirs() {
        // restore_backup must move backed-up managed-skills back into
        // paths.skills_dir() and restore claude.json. The current impl
        // wipes the live managed dir before overlaying.
        with_isolated_home(|home, paths| {
            // Pre-state: a managed skill + claude config to back up
            std::fs::create_dir_all(paths.skills_dir().join("foo")).unwrap();
            std::fs::write(paths.skills_dir().join("foo/SKILL.md"), "# original").unwrap();
            std::fs::write(home.join(".claude.json"), r#"{"v":"orig"}"#).unwrap();

            let backup_dir = create_backup(paths).expect("create_backup");
            let ts = std::fs::read_to_string(backup_dir.join("timestamp")).unwrap();

            // Simulate damage
            std::fs::remove_dir_all(paths.skills_dir()).unwrap();
            std::fs::write(home.join(".claude.json"), r#"{"v":"damaged"}"#).unwrap();

            let restored = restore_backup(paths, &ts).expect("restore_backup");
            assert!(
                restored >= 2,
                "restore count expected >= 2, got {}",
                restored
            );

            assert!(
                paths.skills_dir().join("foo/SKILL.md").exists(),
                "managed skill restored"
            );
            let content = std::fs::read_to_string(home.join(".claude.json")).unwrap();
            assert!(
                content.contains("orig"),
                "claude.json restored, got: {}",
                content
            );
        });
    }

    #[test]
    fn restore_backup_nonexistent_timestamp_err() {
        // restore_backup on a timestamp directory that doesn't exist returns
        // an Err — no files modified, no panic.
        with_isolated_home(|home, paths| {
            std::fs::write(home.join(".claude.json"), r#"{"a":1}"#).unwrap();
            let result = restore_backup(paths, "nonexistent_timestamp_xx");
            assert!(result.is_err(), "expected Err, got: {:?}", result);
            // Live file untouched
            let content = std::fs::read_to_string(home.join(".claude.json")).unwrap();
            assert!(content.contains("\"a\":1"));
        });
    }

    #[test]
    fn has_backup_reflects_backups_dir_state() {
        // has_backup returns false on a fresh data dir, true after a backup
        // exists. Used by TUI to decide whether to offer 'Restore'.
        with_isolated_home(|home, paths| {
            assert!(!has_backup(paths), "fresh dir has no backup");
            std::fs::create_dir_all(home).unwrap();
            let _bd = create_backup(paths).expect("create_backup");
            assert!(has_backup(paths), "after backup, has_backup true");
        });
    }

    #[test]
    fn backup_restore_all_4_cli_targets_symmetric() {
        // create_backup + restore_backup cover all 4 CLI skill dirs:
        // claude, codex, gemini, opencode. Each dir's symlinks are preserved.
        with_isolated_home(|home, paths| {
            let real_skill = paths.skills_dir().join("multi");
            std::fs::create_dir_all(&real_skill).unwrap();
            std::fs::write(real_skill.join("SKILL.md"), "# multi").unwrap();

            for cli in &["claude", "codex", "gemini", "opencode"] {
                let dir = home.join(format!(".{cli}/skills"));
                std::fs::create_dir_all(&dir).unwrap();
                let link = dir.join("multi");
                std::os::unix::fs::symlink(&real_skill, &link).unwrap();
            }

            let backup_dir = create_backup(paths).expect("create_backup");

            // Each CLI dir backed up
            for cli in &["claude", "codex", "gemini", "opencode"] {
                let backed = backup_dir.join(format!("{cli}-skills")).join("multi");
                let md = std::fs::symlink_metadata(&backed).unwrap_or_else(|e| {
                    panic!("missing {cli} symlink in backup: {e}");
                });
                assert!(
                    md.file_type().is_symlink(),
                    "{cli} backup entry must be symlink"
                );
            }

            // Damage one CLI dir and restore
            let ts = std::fs::read_to_string(backup_dir.join("timestamp")).unwrap();
            std::fs::remove_dir_all(home.join(".claude/skills")).unwrap();
            restore_backup(paths, &ts).expect("restore_backup");
            let restored = home.join(".claude/skills/multi");
            assert!(
                std::fs::symlink_metadata(&restored)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false),
                "claude symlink restored"
            );
        });
    }
}

// ============================================================================
// core::db
// ============================================================================

mod db_tests {
    use runai::core::db::{Database, RouterEvent};
    use runai::core::resource::{Resource, ResourceKind, Source, TrashEntry};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn open_db() -> (TempDir, Database) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (tmp, db)
    }

    #[test]
    fn db_migrations_idempotent_to_current_schema_version() {
        // Open the same DB file twice: first open initializes the schema,
        // second open is a no-op replay. schema_version() must be stable and
        // be at the current head (>= 14 on this cloud HEAD). Verify the
        // resources / trash_entries / router_events tables work through the
        // public API rather than poking the raw sqlite_master view.
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("v_to_v.db");

        let db1 = Database::open(&db_path).unwrap();
        let v1 = db1.schema_version();
        assert!(
            v1 >= 14,
            "schema_version should be at least 14 (cloud HEAD), got {}",
            v1
        );
        drop(db1);

        // Second open must not re-run migrations destructively
        let db2 = Database::open(&db_path).unwrap();
        let v2 = db2.schema_version();
        assert_eq!(v1, v2, "schema_version should be stable across re-opens");

        // resources table works (empty list w/o error)
        let listed = db2.list_resources(None, None).unwrap();
        assert!(listed.is_empty(), "fresh DB has zero resources");
        let (n_skill, n_mcp) = db2.resource_count().unwrap();
        assert_eq!(n_skill, 0);
        assert_eq!(n_mcp, 0);

        // trash_entries table works
        let trash = db2.list_trash_entries().unwrap();
        assert!(trash.is_empty(), "fresh DB has empty trash");

        // router_events table works
        let recent = db2.router_recent_events(10).unwrap();
        assert!(recent.is_empty(), "fresh DB has no router events");
    }

    #[test]
    fn db_insert_get_resource_roundtrip_preserves_fields() {
        // insert_resource + get_resource roundtrips name, description, kind,
        // source, installed_at. ON CONFLICT preserves usage_count on re-insert
        // (so an "adopt" pass doesn't zero usage stats — load-bearing invariant).
        let (_tmp, db) = open_db();
        let src = Source::Local {
            path: PathBuf::from("/tmp/foo"),
        };
        let mut res = Resource {
            id: Resource::generate_id(&src, "alpha"),
            name: "alpha".into(),
            kind: ResourceKind::Skill,
            description: "first version".into(),
            directory: PathBuf::from("/tmp/foo/alpha"),
            source: src.clone(),
            installed_at: 1_700_000_000,
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
        };
        db.insert_resource(&res).unwrap();

        // Record usage to bump usage_count via the path the manager uses
        let updated = db.record_usage(&res.id).unwrap();
        assert_eq!(updated, 1, "record_usage should bump the row");

        // Re-insert with same id but new description; usage_count must persist
        res.description = "updated description".into();
        db.insert_resource(&res).unwrap();

        let got = db.get_resource(&res.id).unwrap().expect("resource present");
        assert_eq!(got.name, "alpha");
        assert_eq!(got.kind.as_str(), "skill");
        assert_eq!(got.description, "updated description");
        assert_eq!(
            got.usage_count, 1,
            "ON CONFLICT must NOT zero usage_count, got {}",
            got.usage_count
        );
    }

    #[test]
    fn db_dedupe_skills_by_name_keeps_newest_installed_at() {
        // Two skill rows share the same name 'foo' with different installed_at
        // and different ids (e.g. local vs github). dedupe_skills_by_name
        // keeps the one with the largest installed_at, deletes the loser,
        // returns count of rows removed.
        let (_tmp, db) = open_db();
        let src_local = Source::Local {
            path: PathBuf::from("/tmp/loser"),
        };
        let src_gh = Source::GitHub {
            owner: "o".into(),
            repo: "r".into(),
            branch: "main".into(),
        };
        let loser = Resource {
            id: Resource::generate_id(&src_local, "foo"),
            name: "foo".into(),
            kind: ResourceKind::Skill,
            description: "loser".into(),
            directory: PathBuf::from("/tmp/loser"),
            source: src_local,
            installed_at: 100,
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
        };
        let keeper = Resource {
            id: Resource::generate_id(&src_gh, "foo"),
            name: "foo".into(),
            kind: ResourceKind::Skill,
            description: "keeper".into(),
            directory: PathBuf::from("/tmp/keeper"),
            source: src_gh,
            installed_at: 300,
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
        };
        assert_ne!(loser.id, keeper.id, "ids must differ for dedupe to trigger");
        db.insert_resource(&loser).unwrap();
        db.insert_resource(&keeper).unwrap();

        let removed = db.dedupe_skills_by_name().unwrap();
        assert!(
            removed >= 1,
            "dedupe should remove at least the loser, got {}",
            removed
        );

        // Keeper survives, loser is gone
        assert!(
            db.get_resource(&keeper.id).unwrap().is_some(),
            "keeper row must remain"
        );
        assert!(
            db.get_resource(&loser.id).unwrap().is_none(),
            "loser row must be deleted"
        );

        // list_resources de-dupes by id but the surviving row's installed_at is 300
        let listed = db.list_resources(Some(ResourceKind::Skill), None).unwrap();
        let matched: Vec<_> = listed.iter().filter(|r| r.name == "foo").collect();
        assert_eq!(matched.len(), 1, "exactly one 'foo' row remains");
        assert_eq!(matched[0].installed_at, 300, "newest wins");
    }

    #[test]
    fn db_trash_entry_serialize_roundtrip_with_serde_default() {
        // insert_trash_entry serializes the TrashEntry as JSON; get_trash_entry
        // deserializes back. serde(default) on optional fields like
        // enabled_targets / group_ids / mcp_configs / disabled_backup makes
        // legacy JSON (without those fields) safe to decode.
        let (_tmp, db) = open_db();
        let src = Source::Local {
            path: PathBuf::from("/tmp/foo"),
        };
        let entry = TrashEntry {
            id: "trash-1".into(),
            resource_id: Resource::generate_id(&src, "victim"),
            name: "victim".into(),
            kind: ResourceKind::Skill,
            description: "x".into(),
            directory: PathBuf::from("/tmp/foo/victim"),
            source: src,
            installed_at: 100,
            usage_count: 7,
            last_used_at: Some(200),
            deleted_at: 300,
            payload_path: Some(PathBuf::from("/tmp/trash/payload")),
            enabled_targets: Vec::new(),
            group_ids: Vec::new(),
            mcp_configs: HashMap::new(),
            disabled_backup: None,
        };
        db.insert_trash_entry(&entry).unwrap();

        let got = db
            .get_trash_entry("trash-1")
            .unwrap()
            .expect("entry present");
        assert_eq!(got.id, "trash-1");
        assert_eq!(got.name, "victim");
        assert_eq!(got.usage_count, 7);
        assert_eq!(got.last_used_at, Some(200));
        assert_eq!(got.deleted_at, 300);
        assert_eq!(got.directory, PathBuf::from("/tmp/foo/victim"));

        // list_trash_entries also returns it
        let listed = db.list_trash_entries().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "trash-1");

        // Delete trash row → gone
        db.delete_trash_entry("trash-1").unwrap();
        assert!(db.get_trash_entry("trash-1").unwrap().is_none());
    }

    #[test]
    fn db_insert_router_event_and_session_dedupe() {
        // insert_router_event stores telemetry; record_session_adoption is
        // idempotent on (session_id, skill_name) PK; router_session_routed_skills
        // returns the deduped adoption set.
        let (_tmp, db) = open_db();
        let ev = RouterEvent {
            id: None,
            ts: 1_700_000_500,
            provider: "deepseek".into(),
            model: "v4-flash".into(),
            prompt_tokens: 100,
            completion_tokens: 30,
            reasoning_tokens: 0,
            total_tokens: 130,
            cache_hit_tokens: 0,
            cache_miss_tokens: 130,
            latency_ms: 250,
            chosen_skills_json: r#"["skill1","skill2"]"#.into(),
            candidate_count: 50,
            status: "ok".into(),
            error_msg: None,
            session_id: "sid_abc".into(),
            mode: "router".into(),
            user_prompt: "do something".into(),
            cwd: "/tmp/work".into(),
            bm25_kept: 25,
            llm_raw_response: "<routed>skill1, skill2</routed>".into(),
            hook_output: "### routed\nskill1\nskill2\n".into(),
            llm_input: "candidate listing + prompt".into(),
        };
        db.insert_router_event(&ev).unwrap();

        // Recent events surface it back
        let recent = db.router_recent_events(10).unwrap();
        assert!(
            recent.iter().any(|e| e.session_id == "sid_abc"),
            "inserted event should appear in recent list"
        );

        // Adopt twice → idempotent (no panic) and dedup list = single name
        db.record_session_adoption("sid_abc", "skill1").unwrap();
        db.record_session_adoption("sid_abc", "skill1").unwrap();
        db.record_session_adoption("sid_abc", "skill2").unwrap();
        let routed = db.router_session_routed_skills("sid_abc").unwrap();
        assert_eq!(
            routed.len(),
            2,
            "two unique adopted skills, got {:?}",
            routed
        );
        assert!(routed.contains(&"skill1".to_string()));
        assert!(routed.contains(&"skill2".to_string()));

        // router_session_recommended_skills returns recommended (chosen_skills_json) names
        let rec = db.router_session_recommended_skills("sid_abc").unwrap();
        assert!(
            rec.contains(&"skill1".to_string()),
            "recommended should include skill1, got {:?}",
            rec
        );
    }

    #[test]
    fn db_ai_summary_set_and_read_back() {
        // set_skill_ai_summary stores a per-skill summary JSON; skill_ai_summary
        // reads it back. set_skill_ai_summary_scored also stores an llm_score.
        let (_tmp, db) = open_db();
        let summary_a = r#"{"task":"做ppt","triggers":["ppt","slides"]}"#;
        db.set_skill_ai_summary("ppt-anything", summary_a).unwrap();
        let got = db.skill_ai_summary("ppt-anything").unwrap();
        assert_eq!(got, summary_a, "summary roundtrips byte-for-byte");

        // scored variant updates the same row + sets a score
        let summary_b = r#"{"task":"do ppt","triggers":["ppt"]}"#;
        db.set_skill_ai_summary_scored("ppt-anything", summary_b, 8)
            .unwrap();
        let got2 = db.skill_ai_summary("ppt-anything").unwrap();
        assert_eq!(got2, summary_b);
        let score = db.skill_llm_score("ppt-anything").unwrap();
        assert_eq!(score, 8, "score should be persisted");

        // All-summary map returns it
        let all = db.skill_ai_summary_all().unwrap();
        assert_eq!(all.get("ppt-anything").map(|s| s.as_str()), Some(summary_b));
    }
}

// ============================================================================
// core::recommend::enrich
// ============================================================================

#[cfg(not(target_os = "windows"))]
mod enrich_tests {
    use runai::core::manager::SkillManager;
    use runai::core::recommend::{EnrichMode, EnrichReport, RecommendConfig, enrich_skills};
    use runai::core::resource::{Resource, ResourceKind, Source};
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// Build a SkillManager rooted at a fresh tempdir. The data dir is
    /// isolated; default RecommendConfig has enabled=false.
    fn make_mgr() -> (TempDir, SkillManager) {
        let tmp = TempDir::new().unwrap();
        let mgr = SkillManager::with_base(tmp.path().join("data")).unwrap();
        (tmp, mgr)
    }

    #[test]
    fn enrich_skills_gate_returns_empty_when_router_disabled() {
        // Default RecommendConfig has enabled=false. enrich_skills must short-
        // circuit at the gate, returning EnrichReport::default() — no LLM
        // request, no DB writes, no errors.
        let (_tmp, mgr) = make_mgr();
        let report = enrich_skills(&mgr, None, EnrichMode::MissingOnly, false, 1, None)
            .expect("gate path must succeed");
        // EnrichReport::default() is the all-zero report — assert each field
        let default = EnrichReport::default();
        assert_eq!(report.generated, default.generated);
        assert_eq!(report.skipped_have_summary, default.skipped_have_summary);
        assert_eq!(report.skipped_no_skill_md, default.skipped_no_skill_md);
        assert_eq!(report.refreshed_stale, default.refreshed_stale);
        assert!(
            report.errors.is_empty(),
            "no errors when gate short-circuits, got {:?}",
            report.errors
        );
    }

    #[test]
    fn enrich_skills_empty_resource_set_returns_empty_report() {
        // With enabled=true + api_key set but ZERO skills in the DB, enrich
        // builds no jobs and returns the empty report without any HTTP call.
        let (_tmp, mgr) = make_mgr();
        let mut cfg = RecommendConfig::default();
        cfg.enabled = true;
        cfg.api_key = "test-key".into();
        cfg.save(mgr.paths()).unwrap();

        let report = enrich_skills(&mgr, None, EnrichMode::MissingOnly, false, 1, None)
            .expect("enrich on empty resource set should not error");
        assert_eq!(report.generated, 0);
        assert_eq!(report.skipped_have_summary, 0);
        assert_eq!(report.skipped_no_skill_md, 0);
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);
    }

    #[test]
    fn enrich_mode_missing_only_skips_skills_with_existing_summary() {
        // MissingOnly + existing summary in DB + the resource is registered →
        // planner says should_process=false → skipped_have_summary counted →
        // no LLM call (jobs.is_empty() so the worker pool is never spawned).
        let (_tmp, mgr) = make_mgr();
        let mut cfg = RecommendConfig::default();
        cfg.enabled = true;
        cfg.api_key = "test-key".into();
        cfg.save(mgr.paths()).unwrap();

        // Register a skill physically + in the DB so list_resources includes it
        let skill_dir = mgr.paths().skills_dir().join("alpha");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# alpha\n\nbody").unwrap();
        let src = Source::Local {
            path: skill_dir.clone(),
        };
        let res = Resource {
            id: Resource::generate_id(&src, "alpha"),
            name: "alpha".into(),
            kind: ResourceKind::Skill,
            description: "alpha skill".into(),
            directory: skill_dir,
            source: src,
            installed_at: 1_700_000_000,
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
        };
        mgr.db().insert_resource(&res).unwrap();

        // Pre-populate a summary so MissingOnly skips it
        mgr.db()
            .set_skill_ai_summary("alpha", r#"{"task":"existing"}"#)
            .unwrap();

        let report = enrich_skills(&mgr, None, EnrichMode::MissingOnly, false, 1, None)
            .expect("MissingOnly w/ existing summary should not error");
        assert_eq!(
            report.generated, 0,
            "no LLM call should fire; got generated={}",
            report.generated
        );
        assert_eq!(
            report.skipped_have_summary, 1,
            "alpha must be counted as skipped_have_summary, got {}",
            report.skipped_have_summary
        );
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);

        // DB summary untouched
        let got = mgr.db().skill_ai_summary("alpha").unwrap();
        assert!(
            got.contains("existing"),
            "summary should be unchanged: {}",
            got
        );
    }

    #[test]
    fn enrich_mode_stale_with_fresh_summary_skips_no_llm_call() {
        // Stale mode: if the SKILL.md mtime is OLDER than the stored summary's
        // updated_at, the planner says is_stale=false → should_process=false →
        // skipped_have_summary counted → no LLM call. This proves the mtime
        // comparison wired up correctly (the wrong-way wiring would re-enrich
        // unnecessarily and overcost users).
        let (_tmp, mgr) = make_mgr();
        let mut cfg = RecommendConfig::default();
        cfg.enabled = true;
        cfg.api_key = "test-key".into();
        cfg.save(mgr.paths()).unwrap();

        let skill_dir = mgr.paths().skills_dir().join("beta");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let skill_md = skill_dir.join("SKILL.md");
        std::fs::write(&skill_md, "# beta\n").unwrap();

        let src = Source::Local {
            path: skill_dir.clone(),
        };
        let res = Resource {
            id: Resource::generate_id(&src, "beta"),
            name: "beta".into(),
            kind: ResourceKind::Skill,
            description: "beta skill".into(),
            directory: skill_dir,
            source: src,
            installed_at: 1_700_000_000,
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
        };
        mgr.db().insert_resource(&res).unwrap();

        // Sleep briefly so the summary's NOW-stamped updated_at is strictly
        // greater than the SKILL.md mtime we just wrote. Then write summary:
        // summary_ts >= skill_md mtime → is_stale = false → skipped.
        std::thread::sleep(std::time::Duration::from_secs(2));
        mgr.db()
            .set_skill_ai_summary("beta", r#"{"task":"beta task"}"#)
            .unwrap();

        let report = enrich_skills(&mgr, None, EnrichMode::Stale, false, 1, None)
            .expect("Stale mode should not error");
        assert_eq!(
            report.generated, 0,
            "summary is fresh; no enrich should fire, got generated={}",
            report.generated
        );
        assert_eq!(report.refreshed_stale, 0, "no stale refresh");
        // The skill counts as skipped_have_summary (planner short-circuit)
        assert_eq!(
            report.skipped_have_summary, 1,
            "beta should be skipped_have_summary, got {}",
            report.skipped_have_summary
        );
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);
    }

    #[test]
    fn enrich_report_default_is_zero_and_no_errors() {
        // EnrichReport::default() must produce all-zero counts and an empty
        // errors vec. This is the contract callers rely on when the gate
        // short-circuits.
        let r = EnrichReport::default();
        assert_eq!(r.generated, 0);
        assert_eq!(r.skipped_have_summary, 0);
        assert_eq!(r.skipped_no_skill_md, 0);
        assert_eq!(r.refreshed_stale, 0);
        assert!(r.errors.is_empty());
    }

    #[test]
    fn enrich_mode_variants_are_distinct() {
        // Sanity: the three EnrichMode variants are PartialEq distinct.
        // Guards against accidental enum collapse during refactors.
        assert_eq!(EnrichMode::MissingOnly, EnrichMode::MissingOnly);
        assert_eq!(EnrichMode::Stale, EnrichMode::Stale);
        assert_eq!(EnrichMode::Force, EnrichMode::Force);
        assert_ne!(EnrichMode::MissingOnly, EnrichMode::Stale);
        assert_ne!(EnrichMode::Stale, EnrichMode::Force);
        assert_ne!(EnrichMode::MissingOnly, EnrichMode::Force);
    }
}

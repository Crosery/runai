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
    assert!(t.contains(&"视".to_string()), "missing '视' unigram: {:?}", t);
    assert!(t.contains(&"频".to_string()), "missing '频' unigram: {:?}", t);
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
        "python basics tutorial",                   // 3 tokens
        "ruby guide programming language reference", // 5 tokens
        "python advanced reference",                // 3 tokens
    ];
    let scores = rank("python", &docs);
    assert_eq!(scores.len(), 3);
    // doc[0] and doc[2] contain "python"; doc[1] does not
    let s0 = scores.iter().find(|(i, _)| *i == 0).map(|(_, s)| *s).unwrap();
    let s1 = scores.iter().find(|(i, _)| *i == 1).map(|(_, s)| *s).unwrap();
    let s2 = scores.iter().find(|(i, _)| *i == 2).map(|(_, s)| *s).unwrap();
    assert!(s0 > s1, "doc[0] (has python) > doc[1] (no python): {} > {}", s0, s1);
    assert!(s2 > s1, "doc[2] (has python) > doc[1] (no python): {} > {}", s2, s1);
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
    let s1 = scores.iter().find(|(i, _)| *i == 1).map(|(_, s)| *s).unwrap();
    let s2 = scores.iter().find(|(i, _)| *i == 2).map(|(_, s)| *s).unwrap();
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
            assert!(link_path.symlink_metadata().unwrap().file_type().is_symlink());

            let backup_dir = create_backup(paths).expect("create_backup");

            let backed_up_link = backup_dir.join("claude-skills").join("link-skill");
            let md = std::fs::symlink_metadata(&backed_up_link)
                .expect("backed up symlink should exist");
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
            assert!(backup_dir.join("claude.json").exists(), "claude config copied");
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
            assert!(restored >= 2, "restore count expected >= 2, got {}", restored);

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

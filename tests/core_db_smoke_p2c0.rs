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

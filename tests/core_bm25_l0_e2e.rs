//! L0 integration tests for `core::bm25`.
//!
//! Exercises the public API surface: `tokenize`, `rank`, `contains_cjk`.
//! Black-box only — pulls the module through `runai::core::bm25` like a
//! real downstream consumer (e.g. `recommend`) does.

use runai::core::bm25;

#[test]
fn tokenize_returns_vec_of_strings_for_ascii() {
    let toks = bm25::tokenize("hello world");
    assert_eq!(toks, vec!["hello".to_string(), "world".to_string()]);
}

#[test]
fn tokenize_lowercases_input() {
    let toks = bm25::tokenize("Hello WORLD");
    assert!(toks.contains(&"hello".to_string()));
    assert!(toks.contains(&"world".to_string()));
    assert!(!toks.contains(&"Hello".to_string()));
    assert!(!toks.contains(&"WORLD".to_string()));
}

#[test]
fn tokenize_splits_dash_and_underscore_compound_words() {
    // Skill names like `ppt-anything` or `write_doc` must split so
    // single-word queries (`ppt`, `doc`) still hit.
    let toks = bm25::tokenize("ppt-anything");
    assert!(toks.contains(&"ppt".to_string()));
    assert!(toks.contains(&"anything".to_string()));

    let toks = bm25::tokenize("write_doc");
    assert!(toks.contains(&"write".to_string()));
    assert!(toks.contains(&"doc".to_string()));
}

#[test]
fn tokenize_emits_cjk_unigrams_and_bigrams() {
    let toks = bm25::tokenize("提交模型");
    // unigrams
    assert!(toks.contains(&"提".to_string()));
    assert!(toks.contains(&"交".to_string()));
    assert!(toks.contains(&"模".to_string()));
    assert!(toks.contains(&"型".to_string()));
    // bigrams
    assert!(toks.contains(&"提交".to_string()));
    assert!(toks.contains(&"模型".to_string()));
}

#[test]
fn tokenize_skips_punctuation_and_whitespace() {
    let toks = bm25::tokenize("hello, world!  re-run");
    assert_eq!(
        toks,
        vec![
            "hello".to_string(),
            "world".to_string(),
            "re".to_string(),
            "run".to_string()
        ]
    );
}

#[test]
fn contains_cjk_true_for_chinese() {
    assert!(bm25::contains_cjk("做个 ppt"));
    assert!(bm25::contains_cjk("提交模型"));
    assert!(bm25::contains_cjk("hello 世界"));
}

#[test]
fn contains_cjk_false_for_pure_ascii() {
    assert!(!bm25::contains_cjk("create a frontend page"));
    assert!(!bm25::contains_cjk("figma-alignment"));
    assert!(!bm25::contains_cjk(""));
    assert!(!bm25::contains_cjk("ppt-anything"));
}

#[test]
fn rank_empty_corpus_returns_empty() {
    let docs: Vec<String> = Vec::new();
    let scores = bm25::rank("ppt", &docs);
    assert!(scores.is_empty());
}

#[test]
fn rank_empty_query_returns_empty() {
    let docs = vec!["doc one".to_string(), "doc two".to_string()];
    let scores = bm25::rank("", &docs);
    assert!(scores.is_empty());
}

#[test]
fn rank_returns_pairs_of_doc_index_and_score() {
    let docs = vec![
        "ppt-anything: build illustrated slide deck".to_string(),
        "github wrapper for gh cli".to_string(),
        "figma alignment for vue h5".to_string(),
    ];
    let scores = bm25::rank("ppt", &docs);
    assert_eq!(scores.len(), 3);
    // Each entry is (index, f64 score)
    for (i, s) in &scores {
        assert!(*i < 3);
        assert!(s.is_finite());
    }
}

#[test]
fn rank_sorts_descending_by_score() {
    let docs = vec![
        "alpha".to_string(),
        "alpha beta".to_string(),
        "alpha beta gamma".to_string(),
    ];
    let scores = bm25::rank("alpha beta gamma", &docs);
    assert_eq!(scores.len(), 3);
    // descending
    assert!(scores[0].1 >= scores[1].1);
    assert!(scores[1].1 >= scores[2].1);
    // doc 2 has all 3 terms → wins
    assert_eq!(scores[0].0, 2);
}

#[test]
fn rank_top_hit_matches_exact_keyword() {
    let docs = vec![
        "ppt-anything: build illustrated slide deck".to_string(),
        "github wrapper for gh cli".to_string(),
        "figma alignment for vue h5".to_string(),
    ];
    let scores = bm25::rank("ppt", &docs);
    // Top result must be the ppt doc.
    assert_eq!(scores[0].0, 0);
    assert!(scores[0].1 > 0.0);
}

#[test]
fn rank_bilingual_query_hits_bilingual_doc() {
    let docs = vec![
        "ppt-anything: 做漂亮的 ppt 演示文稿".to_string(),
        "git commit assistant".to_string(),
        "kaiwu rl reward designer".to_string(),
    ];
    let scores = bm25::rank("做 ppt", &docs);
    assert_eq!(scores[0].0, 0, "ppt skill must rank first for `做 ppt`");
    assert!(scores[0].1 > scores[1].1);
}

#[test]
fn rank_unmatched_query_yields_zero_score_top() {
    let docs = vec!["alpha beta gamma".to_string(), "delta epsilon".to_string()];
    let scores = bm25::rank("zzz unrelated keywords", &docs);
    // All zero — but vec is non-empty: caller decides whether to use
    // it or fall back. Scores must still be finite.
    assert_eq!(scores.len(), 2);
    for (_, s) in &scores {
        assert!(s.is_finite());
        assert!(*s == 0.0);
    }
}

#[test]
fn rank_accepts_string_and_str_slices() {
    // The signature is `rank<T: AsRef<str>>(...)` — must accept both.
    let owned: Vec<String> = vec!["one two".into(), "three four".into()];
    let _ = bm25::rank("one", &owned);

    let borrowed: Vec<&str> = vec!["one two", "three four"];
    let _ = bm25::rank("one", &borrowed);
}

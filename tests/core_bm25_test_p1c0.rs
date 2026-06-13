//! P1 regression coverage for `core::bm25`.
//!
//! Tests live in this external integration file (not the in-module `tests`
//! mod) so the public API surface (`tokenize`, `rank`) is exercised exactly
//! as `recommend::router` consumes it.

use runai::core::bm25::{rank, tokenize};

// ---------------------------------------------------------------------------
// tokenize
// ---------------------------------------------------------------------------

/// Plan 5.9 #1 — `tokenize_english_words_and_dashes`
///
/// Basic English tokenization: whitespace splits words, dashes split tokens,
/// output is lower-cased. This is the baseline for skill-name matching
/// (compound identifiers like `foo-bar` must yield both `foo` and `bar`).
#[test]
fn tokenize_english_words_and_dashes() {
    let toks = tokenize("hello world foo-bar");
    assert_eq!(
        toks,
        vec!["hello", "world", "foo", "bar"],
        "whitespace and dashes must both split tokens"
    );

    // Mixed-case input must be lower-cased so query and doc tokens compare
    // case-insensitively.
    let toks_upper = tokenize("Hello WORLD Foo-Bar");
    assert_eq!(
        toks_upper,
        vec!["hello", "world", "foo", "bar"],
        "tokenizer must lowercase"
    );

    // Underscore is treated like dash — `write_doc` → ["write", "doc"].
    let toks_under = tokenize("write_doc");
    assert_eq!(toks_under, vec!["write", "doc"]);
}

/// Plan 5.9 #2 — `tokenize_cjk_unigrams_and_bigrams`
///
/// Bilingual tokenizer emits CJK unigrams (filtered against the stop list)
/// AND adjacent-pair bigrams, while still emitting latin words from a mixed
/// string. This is what lets a query "ppt" or "视频" both hit a doc that
/// describes a "做ppt视频" skill.
#[test]
fn tokenize_cjk_unigrams_and_bigrams() {
    let toks = tokenize("做ppt视频");

    // Latin run must be preserved as a single token.
    assert!(
        toks.contains(&"ppt".to_string()),
        "ppt must be tokenized as latin word: {:?}",
        toks
    );

    // Content CJK chars surface as bare unigrams.
    assert!(
        toks.contains(&"视".to_string()),
        "视 unigram expected: {:?}",
        toks
    );
    assert!(
        toks.contains(&"频".to_string()),
        "频 unigram expected: {:?}",
        toks
    );

    // Adjacent CJK pair must emit a bigram.
    assert!(
        toks.contains(&"视频".to_string()),
        "adjacent-pair bigram 视频 expected: {:?}",
        toks
    );

    // The stop unigram "做" is filtered (see #3); however the "做p" bigram
    // requires two CJK chars adjacent and is therefore NOT emitted across
    // the CJK→latin boundary. This is the documented behaviour of the
    // tokenizer — assert it explicitly to lock in the contract.
    assert!(
        !toks.contains(&"做".to_string()),
        "做 should be stop-filtered: {:?}",
        toks
    );

    // Both a query of "ppt" and a query of "视频" must match this doc via rank.
    let docs = ["做ppt视频"];
    let by_ppt = rank("ppt", &docs);
    assert!(
        !by_ppt.is_empty() && by_ppt[0].1 > 0.0,
        "query ppt must hit mixed doc"
    );
    let by_video = rank("视频", &docs);
    assert!(
        !by_video.is_empty() && by_video[0].1 > 0.0,
        "query 视频 must hit mixed doc"
    );
}

/// Plan 5.9 #3 — `tokenize_cjk_stop_unigrams_filtered`
///
/// High-frequency CJK function words ("的"/"了"/"和"/"我"/"做") must not
/// surface as bare unigrams. Otherwise every skill whose triggers contain
/// "做" lights up regardless of topic. This is the precise BM25 noise the
/// stop-list was added to prevent.
#[test]
fn tokenize_cjk_stop_unigrams_filtered() {
    let toks = tokenize("的了和我做");

    for stop in ["的", "了", "和", "我", "做"] {
        assert!(
            !toks.contains(&stop.to_string()),
            "{} must be filtered as bare unigram, got tokens: {:?}",
            stop,
            toks
        );
    }
}

// ---------------------------------------------------------------------------
// rank
// ---------------------------------------------------------------------------

/// Plan 5.9 #4 — `rank_empty_inputs`
///
/// Empty query OR empty docs must return `Vec::new()` without panicking.
/// The caller (`recommend::router`) treats an empty rank result as "fall
/// back to no prefilter", so it must be a clean, allocation-free sentinel.
#[test]
fn rank_empty_inputs() {
    // Empty docs.
    let docs: Vec<&str> = Vec::new();
    let r1 = rank("query", &docs);
    assert!(r1.is_empty(), "empty docs must return empty rank");

    // Empty query.
    let docs2 = vec!["doc one", "doc two", "doc three"];
    let r2 = rank("", &docs2);
    assert!(r2.is_empty(), "empty query must return empty rank");

    // Whitespace-only query has no tokens → also empty.
    let r3 = rank("   ", &docs2);
    assert!(r3.is_empty(), "whitespace-only query must return empty rank");

    // Both empty.
    let r4 = rank("", &docs);
    assert!(r4.is_empty(), "both empty must return empty rank");
}

/// Plan 5.9 #5 — `rank_bm25_scoring_and_length_norm`
///
/// BM25 must (a) rank docs containing the query term above docs that don't,
/// (b) reward rarer terms more (IDF), and (c) penalize longer docs via the
/// B=0.75 length normalization. We exercise all three with a small synthetic
/// corpus.
#[test]
fn rank_bm25_scoring_and_length_norm() {
    let docs = vec![
        "python basics tutorial", // doc 0
        "ruby guide",             // doc 1 — no "python"
        "python advanced",        // doc 2
    ];

    // (a) Both python-containing docs must outrank the python-less doc.
    let r = rank("python", &docs);
    assert_eq!(r.len(), 3, "rank returns one entry per doc");
    let score_by_idx: std::collections::HashMap<usize, f64> = r.iter().copied().collect();
    let s0 = score_by_idx[&0];
    let s1 = score_by_idx[&1];
    let s2 = score_by_idx[&2];
    assert!(
        s0 > s1,
        "doc 0 (has python) must outrank doc 1 (no python): {:?}",
        r
    );
    assert!(
        s2 > s1,
        "doc 2 (has python) must outrank doc 1 (no python): {:?}",
        r
    );
    assert_eq!(s1, 0.0, "doc without query term must score zero");

    // (c) Length normalization: doc 2 ("python advanced", 2 tokens) is
    // shorter than doc 0 ("python basics tutorial", 3 tokens). With B=0.75
    // and equal TF=1, the shorter doc must receive a higher BM25 score.
    assert!(
        s2 > s0,
        "shorter doc must score higher under length norm (B=0.75): doc0={}, doc2={}",
        s0,
        s2
    );

    // (b) Rare-term IDF: "basics" appears in only one doc (rare); "python"
    // appears in two (common). Query "basics" must place doc 0 first.
    let r_basics = rank("basics", &docs);
    assert_eq!(r_basics[0].0, 0, "rare-term query must rank doc 0 first");
    assert!(
        r_basics[0].1 > 0.0,
        "doc with rare term must have positive score"
    );
    // The two docs without "basics" both score zero.
    let zero_count = r_basics.iter().filter(|(_, s)| *s == 0.0).count();
    assert_eq!(
        zero_count, 2,
        "two docs without rare term must score zero: {:?}",
        r_basics
    );
}

/// Plan 5.9 #6 — `rank_finds_exact_keyword_match`
///
/// A CJK-leaning corpus that ALSO contains the latin keyword "ppt" must
/// still be matchable by an English-only query of "ppt". This is the cross-
/// language prefilter contract: skill names are mostly latin compound
/// identifiers (split on dash/underscore) even in zh SKILL.md descriptions,
/// so the prefilter has to bridge.
///
/// Note on tokenizer contract: BM25 matches whole tokens, so `pptmaker`
/// (no separator) would NOT be split into `ppt`+`maker`. Real skills like
/// `ppt-anything` rely on the dash/underscore split, which is why the
/// dash-separator test (#1) is the load-bearing one for skill names.
#[test]
fn rank_finds_exact_keyword_match() {
    let docs = vec![
        "ppt-anything: 幻灯片生成", // doc 0 — ppt-anything tokenizes to ["ppt","anything","幻","灯","片","生","成", ...bigrams]
        "image processing",         // doc 1 — unrelated, no ppt token
    ];
    let r = rank("ppt", &docs);
    assert_eq!(r.len(), 2, "rank returns one entry per doc");
    assert_eq!(
        r[0].0, 0,
        "doc 0 (contains ppt) must rank first regardless of CJK content"
    );
    assert!(
        r[0].1 > 0.0,
        "winning doc must have positive score, got {}",
        r[0].1
    );
    // The doc without "ppt" must score zero — BM25 only fires on exact
    // token equality.
    assert_eq!(r[1].1, 0.0, "doc without query term must score zero");

    // Also exercise the CJK direction of the cross-language contract: a
    // pure-CJK query must hit a doc that uses the same CJK characters even
    // when surrounded by latin.
    let r_cjk = rank("幻灯", &docs);
    assert_eq!(
        r_cjk[0].0, 0,
        "CJK query must rank the CJK-containing doc first"
    );
    assert!(r_cjk[0].1 > 0.0, "CJK query must yield positive score");
}

//! P2 test coverage for core::search (PLANNING test plan §5.27).
//!
//! Exercises the public surface of `runai::core::search`:
//! `fuzzy_score`, `fuzzy_score_any`, `new_matcher`, `rank`.
//!
//! These are pure functions — no filesystem, no DB, no real binary needed.

use runai::core::search::{fuzzy_score, fuzzy_score_any, new_matcher, rank};

/// 5.27 #1 typo_matching — `fuzzy_score` accepts 1-char typo (subsequence match).
#[test]
fn typo_matching_one_char() {
    let mut m = new_matcher();
    // 'fronted' is a subsequence of 'frontend-design'
    let score = fuzzy_score(&mut m, "frontend-design", "fronted");
    assert!(
        score.is_some(),
        "expected Some(score) for typo subsequence match, got None"
    );
    assert!(
        score.unwrap() > 0,
        "expected score > 0 for subsequence match, got {:?}",
        score
    );

    // Negative control: gibberish should not match
    let mut m2 = new_matcher();
    assert!(
        fuzzy_score(&mut m2, "frontend-design", "zzzqqq").is_none(),
        "gibberish must return None"
    );
}

/// 5.27 #2 score_any_best_field — `fuzzy_score_any` returns highest of any field.
#[test]
fn score_any_returns_best_field() {
    let mut m = new_matcher();
    // "figma" matches the 2nd field much better than the 1st (which has no f-i-g-m-a subseq).
    let best =
        fuzzy_score_any(&mut m, "figma", &["plain-name", "figma-region-loop"]).expect("must match");

    // Individual scores: field 1 = None, field 2 = Some(s2)
    let mut m1 = new_matcher();
    let s1 = fuzzy_score(&mut m1, "plain-name", "figma");
    let mut m2 = new_matcher();
    let s2 = fuzzy_score(&mut m2, "figma-region-loop", "figma").expect("must match field 2");

    assert!(s1.is_none(), "field 1 should not match needle 'figma'");
    assert_eq!(
        best, s2,
        "fuzzy_score_any should return the best (only matching) field's score"
    );

    // No matching field → None
    let mut m3 = new_matcher();
    assert!(
        fuzzy_score_any(&mut m3, "figma", &["alpha", "beta", "gamma"]).is_none(),
        "no matching field must return None"
    );
}

/// 5.27 #2 score_any takes the MAX when multiple fields match.
#[test]
fn score_any_takes_maximum_when_multiple_match() {
    let mut m = new_matcher();
    // Both fields contain a fuzzy match for "ab". Pick the field with higher score.
    let mut m1 = new_matcher();
    let s1 = fuzzy_score(&mut m1, "a_b_c_d", "ab").unwrap_or(0);
    let mut m2 = new_matcher();
    let s2 = fuzzy_score(&mut m2, "ab", "ab").unwrap_or(0);

    let best = fuzzy_score_any(&mut m, "ab", &["a_b_c_d", "ab"]).expect("must match");
    assert_eq!(
        best,
        s1.max(s2),
        "fuzzy_score_any must return the maximum, not the first match"
    );
}

/// 5.27 #3 rank_operators — `rank()` returns score-descending; later items go to bottom.
#[test]
fn rank_orders_by_score_descending() {
    let items = vec![
        "backend-api",
        "frontend-design",
        "frontend-slides",
        "totally-unrelated",
    ];
    let ranked = rank("frontend", items, |s| vec![s.to_string()]);
    // 'totally-unrelated' has no f-r-o-n-t-e-n-d subsequence (no f). Drop it.
    // 'backend-api' has no 'f' so no match either.
    // The two frontend-* items match.
    assert!(
        ranked.len() >= 2,
        "expected at least 2 matches for 'frontend', got {} ({:?})",
        ranked.len(),
        ranked
    );
    // Score-descending order
    for w in ranked.windows(2) {
        assert!(
            w[0].1 >= w[1].1,
            "rank must be score-descending: {} >= {} ({:?})",
            w[0].1,
            w[1].1,
            ranked
        );
    }
    // Non-matching items dropped
    let names: Vec<&str> = ranked.iter().map(|(s, _)| *s).collect();
    assert!(
        !names.contains(&"totally-unrelated"),
        "non-matching item should be dropped: {:?}",
        names
    );
}

/// 5.27 #3 rank with fzf prefix operator (`^needle`) ranks prefix matches above mid-string matches.
#[test]
fn rank_supports_fzf_prefix_operator() {
    // 'frontend-design' starts with 'frontend'; 'plain-frontend' has 'frontend' in the middle.
    let items = vec!["plain-frontend", "frontend-design"];
    let ranked = rank("^frontend", items, |s| vec![s.to_string()]);

    // With ^ prefix anchor, only the prefix-match should rank or it should rank first.
    assert!(!ranked.is_empty(), "^ prefix should still find matches");
    // The prefix-anchored item must be first if any items were ranked.
    let top = ranked[0].0;
    assert_eq!(
        top, "frontend-design",
        "^ prefix anchor should put prefix-matching item first, got order: {:?}",
        ranked
    );
}

/// 5.27 #3 rank with fzf exact-match operator ('needle).
#[test]
fn rank_supports_fzf_exact_operator() {
    let items = vec!["python-helper", "py-thon-spread"];
    // With ' exact-match anchor, 'python-helper' should match (contains "python" as substring),
    // and 'py-thon-spread' (only fuzzy subseq) should be dropped.
    let ranked = rank("'python", items, |s| vec![s.to_string()]);
    let names: Vec<&str> = ranked.iter().map(|(s, _)| *s).collect();
    assert!(
        names.contains(&"python-helper"),
        "exact-match 'python should keep 'python-helper': {:?}",
        names
    );
    assert!(
        !names.contains(&"py-thon-spread"),
        "exact-match 'python should drop fuzzy-only 'py-thon-spread': {:?}",
        names
    );
}

/// 5.27 #4 smart_case_handling — lowercase needle is case-insensitive.
#[test]
fn smart_case_lowercase_needle_matches_any_case() {
    let mut m = new_matcher();
    // Lowercase 'python' should match haystack 'Python-Testing' (case-insensitive).
    assert!(
        fuzzy_score(&mut m, "Python-Testing", "python").is_some(),
        "lowercase needle must match mixed-case haystack"
    );
    // Also matches all-lowercase haystack.
    let mut m2 = new_matcher();
    assert!(
        fuzzy_score(&mut m2, "python-testing", "python").is_some(),
        "lowercase needle must match lowercase haystack"
    );
}

/// 5.27 #4 smart_case via `rank()` (which uses Pattern::parse with CaseMatching::Smart).
#[test]
fn smart_case_via_rank_lowercase_is_case_insensitive() {
    let items = vec!["Python-Testing", "JavaScript-Frontend"];
    let ranked = rank("python", items, |s| vec![s.to_string()]);
    let names: Vec<&str> = ranked.iter().map(|(s, _)| *s).collect();
    assert!(
        names.contains(&"Python-Testing"),
        "lowercase 'python' query should match 'Python-Testing' via smart case: {:?}",
        names
    );
}

/// 5.27 #5 matcher_reusable — `new_matcher()` returns a Matcher that handles many scoring calls.
#[test]
fn matcher_reusable_across_many_calls() {
    let mut m = new_matcher();
    // Use the SAME matcher across many haystacks. If buffers weren't reusable,
    // we'd either panic or produce wrong scores; ensure correctness for each.
    let haystacks = [
        "frontend-design",
        "backend-api",
        "frontend-slides",
        "mobile-ui",
        "frontend-state-mgmt",
        "infra-pipeline",
    ];
    let mut matches = 0;
    let mut non_matches = 0;
    for h in &haystacks {
        match fuzzy_score(&mut m, h, "frontend") {
            Some(score) => {
                matches += 1;
                assert!(score > 0, "matched haystack '{}' must have score > 0", h);
            }
            None => {
                non_matches += 1;
            }
        }
    }
    assert!(
        matches >= 3,
        "expected at least 3 frontend-* matches across haystacks, got {}",
        matches
    );
    assert!(
        non_matches >= 1,
        "expected at least 1 non-match (backend/mobile/infra), got {}",
        non_matches
    );

    // Reuse same matcher for fuzzy_score_any too, no panic / no state corruption.
    let result = fuzzy_score_any(&mut m, "design", &["frontend-design", "mobile-ui"]);
    assert!(result.is_some(), "reused matcher must still work");
}

/// 5.27 #5 matcher_reusable — Matcher works across both fuzzy_score and fuzzy_score_any
/// without recreation between calls.
#[test]
fn matcher_reusable_mixes_score_and_score_any() {
    let mut m = new_matcher();

    // Alternating fuzzy_score and fuzzy_score_any on the same matcher.
    let s1 = fuzzy_score(&mut m, "abcdef", "ace");
    assert!(s1.is_some(), "first fuzzy_score call must match");

    let s2 = fuzzy_score_any(&mut m, "ace", &["xyz", "abcdef", "qpr"]);
    assert!(s2.is_some(), "second fuzzy_score_any call must match");

    let s3 = fuzzy_score(&mut m, "abcdef", "ace");
    assert_eq!(
        s1, s3,
        "same inputs through same Matcher must yield identical scores (no state corruption)"
    );
}

/// Sanity: rank() with empty input is empty output.
#[test]
fn rank_empty_input_yields_empty_output() {
    let items: Vec<&str> = vec![];
    let ranked = rank("anything", items, |s| vec![s.to_string()]);
    assert!(
        ranked.is_empty(),
        "rank on empty input must be empty, got {:?}",
        ranked
    );
}

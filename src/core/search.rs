//! Fuzzy matcher — thin wrapper around `nucleo-matcher` (fzf v2 algorithm).
//!
//! Centralizes fuzzy search for all four call sites — `mcp::tools::sm_search`,
//! `mcp::tools::sm_market`, `cli::Commands::Search`, `cli::Commands::Market` —
//! so the MCP tools and CLI subcommands behave identically. They must all go
//! through here, never raw `&str::contains`.
//!
//! ## Public API
//! - `new_matcher() -> Matcher` — default `Matcher` with `Config::DEFAULT`.
//! - `fuzzy_score(matcher, haystack, needle) -> Option<u32>` — score one
//!   haystack; `None` = no match, higher score = better.
//! - `fuzzy_score_any(matcher, needle, fields) -> Option<u32>` — max score
//!   across fields; `None` only when no field matched.
//! - `fuzzy_score_name_first(matcher, needle, primary, secondary) -> Option<u32>`
//!   — tiered "name-primary, secondary-fallback": a match in `primary` (the
//!   skill NAME) always outranks a `secondary`-only (description / repo_path)
//!   match. This is what `sm_search` / `runai search` / market use so name
//!   matches win; plain `fuzzy_score_any` would let a strong description match
//!   outrank a name match.
//! - `rank(needle, items, fields_of) -> Vec<(item, u32)>` — score-and-sort via
//!   `Pattern::parse`, supporting fzf operators (`^prefix`, `suffix$`,
//!   `'exact`); returns `(item, score)` sorted descending.
//!
//! ## Invariants / gotchas
//! - Case handling is `CaseMatching::Smart`: all-lowercase needle is
//!   case-insensitive, any uppercase makes it case-sensitive (fzf standard).
//!   Normalization is `Normalization::Smart`. Switch in this one place and all
//!   call sites pick it up.
//! - Score is `u32`, higher is better; sort `b.cmp(&a)` (descending). Reuse one
//!   `Matcher` across many comparisons — it caches scratch buffers.
//! - `fuzzy_score` returns `u32` (nucleo's native type) — don't wrap with
//!   `u32::from`, clippy flags it.
//! - `Pattern::parse` is heavier than `Matcher::fuzzy_match`; use `rank` only
//!   when you want fzf operators, otherwise `fuzzy_score` / `fuzzy_score_any`.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Fuzzy-match a single haystack string against a needle using nucleo (fzf v2).
/// Returns `Some(score)` when matched (higher = better), `None` otherwise.
pub fn fuzzy_score(matcher: &mut Matcher, haystack: &str, needle: &str) -> Option<u32> {
    let mut h_buf = Vec::new();
    let mut n_buf = Vec::new();
    matcher
        .fuzzy_match(
            Utf32Str::new(haystack, &mut h_buf),
            Utf32Str::new(needle, &mut n_buf),
        )
        .map(u32::from)
}

/// Tiered "name-primary, secondary-fallback" score. A match in `primary`
/// (the skill NAME) ALWAYS outranks a match that only hits a `secondary` field
/// (description / repo_path / source_label), and within a tier the fzf score
/// orders. Returns `None` only when neither the name nor any secondary matched.
///
/// This is the difference from `fuzzy_score_any`, which takes the plain max
/// across fields and so lets a strong description match outrank a name match.
pub fn fuzzy_score_name_first(
    matcher: &mut Matcher,
    needle: &str,
    primary: &str,
    secondary: &[&str],
) -> Option<u32> {
    // Far above any realistic fzf field score, so the name tier dominates.
    // `NAME_TIER + capped` stays well within u32 (≤ 2·NAME_TIER), no overflow.
    const NAME_TIER: u32 = 1 << 24;
    let cap = |s: u32| s.min(NAME_TIER - 1);
    let name_score = fuzzy_score(matcher, primary, needle);
    let sec_score = fuzzy_score_any(matcher, needle, secondary);
    match (name_score, sec_score) {
        (Some(n), _) => Some(NAME_TIER + cap(n)),
        (None, Some(d)) => Some(cap(d)),
        (None, None) => None,
    }
}

/// Best score across multiple fields. Returns `None` if no field matched.
pub fn fuzzy_score_any(matcher: &mut Matcher, needle: &str, fields: &[&str]) -> Option<u32> {
    fields
        .iter()
        .filter_map(|f| fuzzy_score(matcher, f, needle))
        .max()
}

/// Construct a default matcher (fzf v2 config, smart case, smart normalization).
pub fn new_matcher() -> Matcher {
    Matcher::new(Config::DEFAULT)
}

/// Score-rank a list of items by their best field score against `needle`.
/// Returns `(item, score)` sorted high-to-low. Items with no match are dropped.
pub fn rank<T, F>(needle: &str, items: impl IntoIterator<Item = T>, fields_of: F) -> Vec<(T, u32)>
where
    F: Fn(&T) -> Vec<String>,
{
    let mut matcher = new_matcher();
    let pattern = Pattern::parse(needle, CaseMatching::Smart, Normalization::Smart);
    let mut out: Vec<(T, u32)> = items
        .into_iter()
        .filter_map(|item| {
            let fields = fields_of(&item);
            let mut best: Option<u32> = None;
            let mut h_buf = Vec::new();
            for f in &fields {
                if let Some(score) = pattern.score(Utf32Str::new(f, &mut h_buf), &mut matcher) {
                    best = Some(best.map_or(score, |b| b.max(score)));
                }
            }
            best.map(|s| (item, s))
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typo_fronted_matches_frontend() {
        let mut m = new_matcher();
        // 1-char deletion: 'fronted' is a subsequence of 'frontend'
        assert!(fuzzy_score(&mut m, "frontend-design", "fronted").is_some());
    }

    #[test]
    fn subsequence_fnt_matches_frontend() {
        let mut m = new_matcher();
        assert!(fuzzy_score(&mut m, "frontend", "fnt").is_some());
    }

    #[test]
    fn no_match_returns_none() {
        let mut m = new_matcher();
        assert!(fuzzy_score(&mut m, "frontend", "zzz").is_none());
    }

    #[test]
    fn multi_field_takes_best() {
        let mut m = new_matcher();
        let score = fuzzy_score_any(&mut m, "figma", &["plain-name", "figma-region-loop"]);
        assert!(score.is_some());
    }

    #[test]
    fn name_match_outranks_description_only() {
        let mut m = new_matcher();
        // "foo" hits A's NAME and only B's secondary (description) — strongly.
        let a = fuzzy_score_name_first(&mut m, "foo", "foo-tool", &["unrelated text"]).unwrap();
        let b = fuzzy_score_name_first(&mut m, "foo", "tool", &["foo foo foo foo foo"]).unwrap();
        assert!(
            a > b,
            "a NAME match ({a}) must outrank a description-only match ({b})"
        );
    }

    #[test]
    fn name_first_no_match_is_none_but_secondary_alone_matches() {
        let mut m = new_matcher();
        assert!(fuzzy_score_name_first(&mut m, "zzz", "tool", &["a desc"]).is_none());
        // secondary-only still matches (just ranks below any name match).
        assert!(fuzzy_score_name_first(&mut m, "desc", "tool", &["a desc"]).is_some());
    }

    #[test]
    fn rank_orders_by_score_desc() {
        let items = vec!["frontend-design", "frontend-slides", "backend-api"];
        let ranked = rank("frontend", items, |s| vec![s.to_string()]);
        assert_eq!(ranked.len(), 2);
        assert!(ranked[0].1 >= ranked[1].1);
    }
}

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
    fn rank_orders_by_score_desc() {
        let items = vec!["frontend-design", "frontend-slides", "backend-api"];
        let ranked = rank("frontend", items, |s| vec![s.to_string()]);
        assert_eq!(ranked.len(), 2);
        assert!(ranked[0].1 >= ranked[1].1);
    }
}

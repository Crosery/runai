//! Pure math for the skill feedback radar: five 0-10 axes blended from
//! router telemetry, explicit feedback, LLM enrich scoring, and usage
//! counts. No I/O — every function here takes plain numbers/options and
//! returns a number; callers in `db::feedback` / `recommend` / `server`
//! supply the counts from SQLite.
//!
//! ## Axes
//! - **adoption** — of the sessions where the router chose this skill, how
//!   many did the main agent actually adopt (vs. just see recommended)?
//!   `(adopted_sessions + 1) / (chosen_sessions + 2) * 10`. Laplace-smoothed
//!   so a skill with zero sessions lands at the neutral midpoint (5.0)
//!   instead of a data-starved 0 or 10.
//! - **precision** — of the times this skill reached the BM25 candidate
//!   list, how often did the router LLM actually pick it?
//!   `(chosen_events + 1) / (candidate_events + 2) * 10`, same smoothing.
//! - **rating** — explicit user thumbs up/down ratio:
//!   `(positive + 1) / (positive + negative + 2) * 10`.
//! - **quality** — the enrich pass's `llm_score` (0-10) verbatim, clamped;
//!   `None` (never enriched) is neutral 5.0, not 0.
//! - **heat** — usage popularity, log-compressed against the corpus max so
//!   one wildly-popular skill doesn't flatten every other skill's heat to
//!   near-zero: `ln(1 + usage) / ln(1 + max_usage) * 10`. `max_usage == 0`
//!   (empty corpus) is defined as 0.0 for every skill.
//!
//! `feedback_factor` is a separate 0..1 helper (not part of the radar) that
//! blends adoption + rating into a single weight, for callers (e.g. the
//! router's candidate ordering) that want one scalar rather than five axes.
//! Zero data there is neutral 0.5, matching the axes' zero-data-is-neutral
//! convention.
//!
//! All axis functions are monotonic in their "good" input and bounded to
//! `[0, 10]` (`feedback_factor` to `[0, 1]`) — see the unit tests below for
//! the boundary and monotonicity pins.

/// Laplace-smoothed ratio scaled to 0-10. `hits <= total` is expected but
/// not enforced (callers pass consistent counts); the formula is still
/// well-defined and bounded for any non-negative inputs.
fn smoothed_ratio_10(hits: i64, total: i64) -> f64 {
    (hits as f64 + 1.0) / (total as f64 + 2.0) * 10.0
}

/// Adoption axis: fraction of chosen-sessions that were actually adopted.
pub fn axis_adoption(adopted_sessions: i64, chosen_sessions: i64) -> f64 {
    smoothed_ratio_10(adopted_sessions, chosen_sessions)
}

/// Precision axis: fraction of candidate-appearances that were chosen.
pub fn axis_precision(chosen_events: i64, candidate_events: i64) -> f64 {
    smoothed_ratio_10(chosen_events, candidate_events)
}

/// Rating axis: fraction of explicit feedback that was positive.
pub fn axis_rating(pos: i64, neg: i64) -> f64 {
    smoothed_ratio_10(pos, pos + neg)
}

/// Quality axis: the enrich pass's own 0-10 score, clamped. Neutral 5.0
/// when the skill has never been enriched.
pub fn axis_quality(llm_score: Option<i64>) -> f64 {
    match llm_score {
        Some(s) => s.clamp(0, 10) as f64,
        None => 5.0,
    }
}

/// Heat axis: log-compressed usage popularity relative to the corpus max.
/// Clamped so a caller passing a stale `max_usage_count` below the actual
/// usage still gets a value inside the documented [0, 10] bound.
pub fn axis_heat(usage_count: i64, max_usage_count: i64) -> f64 {
    if max_usage_count <= 0 {
        return 0.0;
    }
    let usage = usage_count.max(0) as f64;
    let max_usage = max_usage_count as f64;
    ((1.0 + usage).ln() / (1.0 + max_usage).ln() * 10.0).min(10.0)
}

/// Single 0..1 scalar blending adoption + rating, for callers that want one
/// weight rather than the full five-axis radar (e.g. router candidate
/// ordering). Zero data on both inputs is neutral 0.5.
pub fn feedback_factor(adopted_sessions: i64, chosen_sessions: i64, pos: i64, neg: i64) -> f64 {
    (axis_adoption(adopted_sessions, chosen_sessions) + axis_rating(pos, neg)) / 20.0
}

/// The five-axis radar for one skill, each already scaled to 0-10.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadarScores {
    pub adoption: f64,
    pub precision: f64,
    pub rating: f64,
    pub quality: f64,
    pub heat: f64,
}

/// Assemble a skill's full radar from raw counts. Thin wrapper over the
/// individual axis functions — kept as one call so callers don't have to
/// remember the five function names and argument orders.
#[allow(clippy::too_many_arguments)]
pub fn compute_radar(
    adopted_sessions: i64,
    chosen_sessions: i64,
    chosen_events: i64,
    candidate_events: i64,
    feedback_pos: i64,
    feedback_neg: i64,
    llm_score: Option<i64>,
    usage_count: i64,
    max_usage_count: i64,
) -> RadarScores {
    RadarScores {
        adoption: axis_adoption(adopted_sessions, chosen_sessions),
        precision: axis_precision(chosen_events, candidate_events),
        rating: axis_rating(feedback_pos, feedback_neg),
        quality: axis_quality(llm_score),
        heat: axis_heat(usage_count, max_usage_count),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    #[test]
    fn zero_data_axes_are_neutral() {
        assert!((axis_adoption(0, 0) - 5.0).abs() < EPS);
        assert!((axis_precision(0, 0) - 5.0).abs() < EPS);
        assert!((axis_rating(0, 0) - 5.0).abs() < EPS);
        assert!((axis_quality(None) - 5.0).abs() < EPS);
        assert_eq!(axis_heat(0, 0), 0.0, "empty corpus max is defined as 0.0");
        assert!((feedback_factor(0, 0, 0, 0) - 0.5).abs() < EPS);
    }

    #[test]
    fn axes_are_bounded_0_to_10() {
        for (hits, total) in [(0, 0), (0, 1000), (1000, 1000), (1, 1)] {
            let a = axis_adoption(hits, total);
            assert!((0.0..=10.0).contains(&a), "adoption out of bounds: {a}");
            let p = axis_precision(hits, total);
            assert!((0.0..=10.0).contains(&p), "precision out of bounds: {p}");
        }
        assert!((0.0..=10.0).contains(&axis_rating(0, 1000)));
        assert!((0.0..=10.0).contains(&axis_rating(1000, 0)));
        assert_eq!(axis_quality(Some(999)), 10.0, "score above range clamps");
        assert_eq!(axis_quality(Some(-5)), 0.0, "score below range clamps");
        assert!((0.0..=10.0).contains(&axis_heat(1_000_000, 1_000_000)));
    }

    #[test]
    fn feedback_factor_is_bounded_0_to_1() {
        assert!((0.0..=1.0).contains(&feedback_factor(0, 0, 0, 0)));
        assert!((0.0..=1.0).contains(&feedback_factor(1000, 1000, 1000, 0)));
        assert!((0.0..=1.0).contains(&feedback_factor(0, 1000, 0, 1000)));
    }

    #[test]
    fn one_more_positive_vote_strictly_increases_rating() {
        let before = axis_rating(3, 3);
        let after = axis_rating(4, 3);
        assert!(after > before, "{after} must exceed {before}");
    }

    #[test]
    fn one_more_negative_vote_strictly_decreases_rating() {
        let before = axis_rating(3, 3);
        let after = axis_rating(3, 4);
        assert!(after < before, "{after} must be less than {before}");
    }

    #[test]
    fn more_adopted_sessions_strictly_increases_adoption() {
        let before = axis_adoption(2, 10);
        let after = axis_adoption(3, 10);
        assert!(after > before);
    }

    #[test]
    fn more_candidate_events_with_flat_chosen_strictly_decreases_precision() {
        let before = axis_precision(5, 10);
        let after = axis_precision(5, 11);
        assert!(
            after < before,
            "showing up as a candidate more often without being chosen more should lower precision"
        );
    }

    #[test]
    fn heat_is_monotonic_in_usage_and_maxes_out_at_the_corpus_leader() {
        let low = axis_heat(1, 100);
        let mid = axis_heat(50, 100);
        let top = axis_heat(100, 100);
        assert!(low < mid);
        assert!(mid < top);
        assert!(
            (top - 10.0).abs() < EPS,
            "the corpus max always scores 10.0"
        );
    }

    #[test]
    fn heat_stays_capped_when_usage_exceeds_a_stale_corpus_max() {
        assert!((axis_heat(500, 100) - 10.0).abs() < EPS);
    }

    #[test]
    fn heat_never_negative_for_negative_usage_input() {
        // Defensive: a caller passing a corrupt negative usage_count must not
        // produce a NaN/negative axis value.
        assert_eq!(axis_heat(-5, 100), axis_heat(0, 100));
    }

    #[test]
    fn quality_neutral_differs_from_an_explicit_midpoint_score() {
        // Neutral-for-missing-data (None) happens to equal an explicit 5,
        // which is intentional: an unenriched skill should not be penalized
        // or boosted relative to a skill a human/LLM explicitly rated
        // average.
        assert_eq!(axis_quality(None), axis_quality(Some(5)));
    }

    #[test]
    fn compute_radar_assembles_all_five_axes() {
        let r = compute_radar(3, 5, 8, 20, 4, 1, Some(7), 50, 200);
        assert_eq!(r.adoption, axis_adoption(3, 5));
        assert_eq!(r.precision, axis_precision(8, 20));
        assert_eq!(r.rating, axis_rating(4, 1));
        assert_eq!(r.quality, axis_quality(Some(7)));
        assert_eq!(r.heat, axis_heat(50, 200));
    }
}

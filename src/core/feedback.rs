//! Transcript-mined feedback signals → auto user ratings.
//!
//! Approach: instead of round-tripping the LLM to score each session
//! (~$0.01 / session, slow), we exploit the structure of consecutive
//! same-session router_events. If event N picks skill X and event N+1's
//! `user_prompt` (the user's next message within ~30 min) contains negative
//! cues ("不对", "换一个", "stop"), that's a strong implicit signal X wasn't
//! useful. Conversely positive cues ("可以", "完美") right after an
//! injection bump the score.
//!
//! Token cost: 0. We only read `router_events` rows already in SQLite.
//!
//! Writes land in `resource_user_rating` with `source = 'auto'`, which is
//! *never* allowed to overwrite a manual rating (set_user_rating_auto
//! short-circuits if existing.source == 'manual'). So if you've explicitly
//! rated a skill in the dashboard, mining can't touch it.

use anyhow::Result;
use std::collections::HashMap;

use crate::core::manager::SkillManager;

/// Window during which a user's next message counts as feedback on the
/// preceding chosen skills. After this, treat as a fresh topic.
const FEEDBACK_WINDOW_SECS: i64 = 30 * 60;

/// Patterns that indicate the user rejected / disliked the recommendation.
/// All matched case-insensitively as substrings. Order doesn't matter.
const NEGATIVE_PATTERNS: &[&str] = &[
    "不对", "不是这个", "不要", "不要这个", "别用", "换一个", "换个", "重做",
    "不好用", "没用", "废", "无用", "走偏", "不行",
    "wrong", "incorrect", "useless", "bad rec", "stop using", "switch",
];

/// Positive cues that suggest the recommended skill was helpful. We weight
/// these lower than negatives because users rarely thank explicitly even
/// when content was helpful — a quiet next-prompt about a different topic
/// is the baseline, not a negative signal.
const POSITIVE_PATTERNS: &[&str] = &[
    "可以的", "可以", "对的", "对了", "对", "好的", "完美", "不错", "太对了",
    "perfect", "thanks", "thank you", "great", "exactly",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Polarity {
    Positive,
    Negative,
    Neutral,
}

fn classify_feedback(text: &str) -> Polarity {
    let lower = text.to_lowercase();
    let neg_hit = NEGATIVE_PATTERNS.iter().any(|p| lower.contains(*p));
    if neg_hit {
        return Polarity::Negative;
    }
    let pos_hit = POSITIVE_PATTERNS.iter().any(|p| lower.contains(*p));
    if pos_hit {
        return Polarity::Positive;
    }
    Polarity::Neutral
}

/// Aggregate signal counters per skill across the mining window.
#[derive(Debug, Default, Clone)]
struct SkillSignal {
    positive: i32,
    negative: i32,
    /// Sample of the most recent matched text (for the rating note).
    last_match: String,
}

#[derive(Debug, Default, Clone)]
pub struct FeedbackReport {
    /// Number of (skill_name, polarity) signals counted before aggregation.
    pub total_signals: usize,
    /// Skills that ended up with a written auto rating.
    pub ratings_written: usize,
    /// Skills whose mined rating was skipped because a manual rating exists.
    pub skipped_manual_present: usize,
    /// Distinct (skill -> final auto score) pairs we wrote, for caller log.
    pub per_skill: HashMap<String, i64>,
}

/// Walk router_events from the last `hours` hours, count feedback signals
/// per skill, and upsert auto user ratings. Returns a report. Safe to run
/// repeatedly — subsequent runs simply overwrite the previous auto rating
/// with a fresh aggregate.
pub fn mine_feedback(mgr: &SkillManager, hours: i64) -> Result<FeedbackReport> {
    let since = chrono::Utc::now().timestamp() - hours.max(1) * 3600;
    let events = mgr.db().router_events_since_ordered(since)?;

    // Group by session, then iterate pairs (i, i+1) — N+1 prompt is the
    // implicit feedback on N's chosen.
    let mut by_session: HashMap<String, Vec<&crate::core::db::RouterEvent>> = HashMap::new();
    for ev in &events {
        by_session.entry(ev.session_id.clone()).or_default().push(ev);
    }

    let mut signals: HashMap<String, SkillSignal> = HashMap::new();
    let mut total_signals = 0usize;

    for evs in by_session.values() {
        for i in 0..evs.len().saturating_sub(1) {
            let cur = evs[i];
            let next = evs[i + 1];
            let chosen: Vec<String> =
                serde_json::from_str(&cur.chosen_skills_json).unwrap_or_default();
            if chosen.is_empty() || cur.status != "ok" {
                continue;
            }
            if next.ts - cur.ts > FEEDBACK_WINDOW_SECS {
                continue;
            }
            if next.user_prompt.trim().is_empty() {
                continue;
            }
            let polarity = classify_feedback(&next.user_prompt);
            if polarity == Polarity::Neutral {
                continue;
            }
            let preview: String = next.user_prompt.chars().take(80).collect();
            for skill in &chosen {
                let entry = signals.entry(skill.clone()).or_default();
                match polarity {
                    Polarity::Negative => entry.negative += 1,
                    Polarity::Positive => entry.positive += 1,
                    Polarity::Neutral => {}
                }
                entry.last_match = preview.clone();
                total_signals += 1;
            }
        }
    }

    let mut report = FeedbackReport {
        total_signals,
        ..Default::default()
    };

    for (skill, sig) in signals.iter() {
        // Baseline 5, +1 per positive, -2 per negative (negative weighted
        // more strongly since it's a clearer dissatisfaction signal).
        let raw = 5 + sig.positive - 2 * sig.negative;
        let clamped: i64 = raw.clamp(1, 10) as i64;
        let note = format!(
            "auto: +{}/-{} | sample: {}",
            sig.positive, sig.negative, sig.last_match
        );
        match mgr.db().set_user_rating_auto(skill, clamped, &note)? {
            true => {
                report.ratings_written += 1;
                report.per_skill.insert(skill.clone(), clamped);
            }
            false => {
                report.skipped_manual_present += 1;
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_negative_cn() {
        assert_eq!(classify_feedback("不对，换一个"), Polarity::Negative);
        assert_eq!(classify_feedback("这个不要"), Polarity::Negative);
        assert_eq!(classify_feedback("不好用"), Polarity::Negative);
    }

    #[test]
    fn classify_negative_en() {
        assert_eq!(classify_feedback("That's wrong"), Polarity::Negative);
        assert_eq!(classify_feedback("Stop using that"), Polarity::Negative);
    }

    #[test]
    fn classify_positive_cn() {
        assert_eq!(classify_feedback("可以的"), Polarity::Positive);
        assert_eq!(classify_feedback("完美，就这么做"), Polarity::Positive);
    }

    #[test]
    fn classify_neutral_unrelated() {
        assert_eq!(
            classify_feedback("帮我看下这个错误日志"),
            Polarity::Neutral
        );
        assert_eq!(classify_feedback("hello world"), Polarity::Neutral);
    }

    #[test]
    fn negative_beats_positive_when_both_present() {
        // "不对" appears → Negative even if "好的" also in text
        assert_eq!(
            classify_feedback("好的，不过这个不对，换一个"),
            Polarity::Negative
        );
    }
}

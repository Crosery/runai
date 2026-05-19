//! User preferences, serialized into the `prefs_json` column.
//!
//! The on-disk shape is loose by design:
//! - empty / malformed JSON → defaults
//! - missing fields → individual `serde(default = ...)` per field
//! - `candidate_limit` is clamped to `[1, 5]` on load
//!
//! This makes the column safe to evolve: adding a field never breaks an
//! old prefs blob, and dropping one is a no-op for unknown keys.

use serde::{Deserialize, Serialize};

/// How the recommender hook integrates with the existing skill set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecommendMode {
    /// Suggest alongside whatever the user already has enabled.
    Compatible,
    /// Suggest only — actively discourage already-enabled overlaps.
    Exclusive,
    /// Disable the recommender for this user.
    Off,
}

impl RecommendMode {
    /// Lowercase string form, matching the serde serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            RecommendMode::Compatible => "compatible",
            RecommendMode::Exclusive => "exclusive",
            RecommendMode::Off => "off",
        }
    }
}

/// User preferences persisted in `prefs_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPrefs {
    #[serde(default = "default_true")]
    pub show_tradeoff: bool,
    #[serde(default = "default_true")]
    pub show_session_history: bool,
    #[serde(default = "default_true")]
    pub show_feedback_protocol: bool,
    #[serde(default = "default_recommend_mode")]
    pub recommend_mode: RecommendMode,
    #[serde(default = "default_candidate_limit")]
    pub candidate_limit: u8,
}

fn default_true() -> bool {
    true
}

fn default_candidate_limit() -> u8 {
    3
}

fn default_recommend_mode() -> RecommendMode {
    RecommendMode::Compatible
}

/// Minimum / maximum allowed value of [`UserPrefs::candidate_limit`].
const CANDIDATE_LIMIT_MIN: u8 = 1;
const CANDIDATE_LIMIT_MAX: u8 = 5;

impl Default for UserPrefs {
    fn default() -> Self {
        Self {
            show_tradeoff: default_true(),
            show_session_history: default_true(),
            show_feedback_protocol: default_true(),
            recommend_mode: default_recommend_mode(),
            candidate_limit: default_candidate_limit(),
        }
    }
}

impl UserPrefs {
    /// Lenient deserialization:
    /// - empty input → defaults
    /// - invalid JSON → defaults
    /// - partial JSON → individual fields filled with defaults via serde
    /// - `candidate_limit` clamped to `[1, 5]`
    pub fn from_json_str(s: &str) -> Self {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Self::default();
        }
        let mut prefs: Self = match serde_json::from_str(trimmed) {
            Ok(p) => p,
            Err(_) => return Self::default(),
        };
        prefs.candidate_limit = prefs
            .candidate_limit
            .clamp(CANDIDATE_LIMIT_MIN, CANDIDATE_LIMIT_MAX);
        prefs
    }

    /// Serialize as compact JSON suitable for the `prefs_json` column.
    pub fn to_json_str(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        let p = UserPrefs::default();
        assert!(p.show_tradeoff);
        assert!(p.show_session_history);
        assert!(p.show_feedback_protocol);
        assert_eq!(p.recommend_mode, RecommendMode::Compatible);
        assert_eq!(p.candidate_limit, 3);
    }

    #[test]
    fn test_partial_json_fills_defaults() {
        let p = UserPrefs::from_json_str(r#"{"show_tradeoff": false}"#);
        assert!(!p.show_tradeoff);
        assert!(p.show_session_history);
        assert!(p.show_feedback_protocol);
        assert_eq!(p.recommend_mode, RecommendMode::Compatible);
        assert_eq!(p.candidate_limit, 3);
    }

    #[test]
    fn test_invalid_json_returns_defaults() {
        assert_eq!(UserPrefs::from_json_str("not json"), UserPrefs::default());
        assert_eq!(UserPrefs::from_json_str(""), UserPrefs::default());
        assert_eq!(UserPrefs::from_json_str("   "), UserPrefs::default());
        assert_eq!(UserPrefs::from_json_str("{"), UserPrefs::default());
    }

    #[test]
    fn test_candidate_limit_clamping() {
        let hi = UserPrefs::from_json_str(r#"{"candidate_limit": 10}"#);
        assert_eq!(hi.candidate_limit, CANDIDATE_LIMIT_MAX);
        let lo = UserPrefs::from_json_str(r#"{"candidate_limit": 0}"#);
        assert_eq!(lo.candidate_limit, CANDIDATE_LIMIT_MIN);
        let ok = UserPrefs::from_json_str(r#"{"candidate_limit": 2}"#);
        assert_eq!(ok.candidate_limit, 2);
    }

    #[test]
    fn test_roundtrip() {
        let p = UserPrefs {
            show_tradeoff: false,
            show_session_history: true,
            show_feedback_protocol: false,
            recommend_mode: RecommendMode::Exclusive,
            candidate_limit: 4,
        };
        let json = p.to_json_str();
        let back = UserPrefs::from_json_str(&json);
        assert_eq!(p, back);
    }

    #[test]
    fn test_recommend_mode_serialization() {
        let cases = [
            (RecommendMode::Compatible, "compatible"),
            (RecommendMode::Exclusive, "exclusive"),
            (RecommendMode::Off, "off"),
        ];
        for (mode, expected) in cases {
            assert_eq!(mode.as_str(), expected);
            let v = serde_json::to_value(mode).unwrap();
            assert_eq!(v, serde_json::Value::String(expected.to_string()));
            let back: RecommendMode =
                serde_json::from_value(serde_json::Value::String(expected.to_string())).unwrap();
            assert_eq!(back, mode);
        }
    }
}

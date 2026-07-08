//! Plain data types returned/accepted by the `Database` query layer.
//!
//! These are pure value structs — no SQL, no I/O. The row converters that
//! build `RouterEvent` / `User` live with their queries (router.rs / users.rs)
//! because they index columns positionally and must stay next to the SELECTs.

#[derive(Debug, Clone)]
pub struct RouterEvent {
    /// SQLite rowid. None when constructed for insert; Some when read back.
    pub id: Option<i64>,
    pub ts: i64,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_tokens: i64,
    pub cache_miss_tokens: i64,
    pub latency_ms: i64,
    pub chosen_skills_json: String,
    pub candidate_count: i64,
    pub status: String,
    pub error_msg: Option<String>,
    pub session_id: String,
    pub mode: String,
    /// Original user prompt that triggered this router call. Empty for legacy
    /// rows written before schema v7. Capped at ~2 KB on insert to bound DB size.
    pub user_prompt: String,
    /// Working directory the hook was invoked in (cwd from Claude Code hook JSON).
    /// Empty for legacy rows.
    pub cwd: String,
    /// How many candidates remained after BM25 prefilter (= candidate_count when
    /// prefilter was bypassed). Lets dashboards see prefilter efficacy.
    pub bm25_kept: i64,
    /// Raw text the router LLM returned (the first ~2 KB) — the mode tag line
    /// plus skill names, before any post-processing. Empty for legacy rows.
    /// Lets users see "what did the model literally say" in the dashboard.
    pub llm_raw_response: String,
    /// The hook stdout that runai injected into Claude Code (the markdown block
    /// the main agent receives). Capped at ~6 KB. Empty for rows where the
    /// hook didn't inject anything (chosen=[]) or pre-schema-v8.
    pub hook_output: String,
    /// Full user-message string sent to the router LLM (history block +
    /// already_routed list + candidate listing + current user prompt).
    /// Capped at ~64 KB. Empty for pre-schema-v13 rows. Useful for
    /// diagnosing mis-routes — answers "what did the second-wave model see?".
    pub llm_input: String,
    /// Full first-wave intent-recognition prompt sent to the same recommend
    /// model. Capped at ~16 KB. Empty for pre-schema-v25 rows.
    pub intent_llm_input: String,
    /// Compact first-wave intent output used as the BM25 query source and the
    /// current-session intent-memory row. Capped at ~2 KB.
    pub intent_llm_output: String,
    /// First-wave status: `ok` when the model returned a usable intent,
    /// `fallback` when deterministic compression was used, empty for legacy.
    pub intent_status: String,
    /// First-wave error message when `intent_status == "fallback"`.
    pub intent_error_msg: Option<String>,
    /// Ordered candidate names after deterministic gates + BM25 prefilter,
    /// before the second-wave router LLM picks the final set.
    pub bm25_candidates_json: String,
    /// Authenticated user_id this event belongs to. None for pre-schema-v15
    /// rows and for unauthenticated requests during the compat window
    /// (prefs.require_auth=false). Per-user dashboard views filter on this.
    pub user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct User {
    pub user_id: String,
    pub username: String,
    pub password_hash: String,
    pub api_key_hash: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub prefs_json: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouterIntentMemoryItem {
    pub id: i64,
    pub ts: i64,
    pub session_id: String,
    pub user_id: Option<String>,
    pub client_kind: String,
    pub memory: String,
}

#[derive(Debug, Clone)]
pub struct RouterModelStat {
    pub model: String,
    pub calls: i64,
    pub total_tokens: i64,
    /// Mean `latency_ms` across ALL of this model's rows (not ok-only —
    /// per-model latency is a spend/perf signal, error rows included). `None`
    /// when the model has no rows in scope.
    pub avg_latency_ms: Option<f64>,
    /// Rows where `chosen_skills_json` is non-empty and not `'[]'` — i.e. the
    /// router actually injected at least one skill. Powers the dashboard
    /// model-usage panel's hit-rate column without a second table scan.
    pub hits: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineBucket {
    pub ts_start: i64,
    pub total: i64,
    pub hits: i64,
    pub errors: i64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Clone)]
pub struct RouterStatsSummary {
    pub total_calls: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_reasoning_tokens: i64,
    pub total_tokens: i64,
    pub errors: i64,
    pub avg_latency_ms: Option<f64>,
    pub per_model: Vec<RouterModelStat>,
}

/// One `skill_feedback` row — an explicit ±1 verdict on a skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFeedbackRow {
    pub id: i64,
    pub ts: i64,
    pub skill_name: String,
    /// Owner-pool scope of the skill this feedback is about. `None` = public
    /// pool, `Some(uid)` = that user's private skill — same convention as
    /// `resources.owner_user_id`.
    pub owner_user_id: Option<String>,
    /// The feedback author, when known. `None` for unauthenticated requests.
    pub user_id: Option<String>,
    pub session_id: Option<String>,
    /// Loosely references the `router_events` row that produced the judged
    /// recommendation. No FK constraint — the row may reference an id that
    /// has since been pruned.
    pub event_id: Option<i64>,
    /// Always exactly `1` or `-1`; enforced at write time by
    /// `Database::record_skill_feedback`.
    pub verdict: i64,
    pub note: Option<String>,
}

/// Per-skill router funnel counts computed from `router_events` +
/// `router_session_adoptions` since a given timestamp. See
/// `Database::skill_router_stats`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RouterSkillStats {
    /// Events where this skill appeared in the BM25 candidate set.
    pub candidate_events: i64,
    /// Events where the router LLM actually chose this skill.
    pub chosen_events: i64,
    /// Distinct sessions in which this skill was chosen at least once.
    pub chosen_sessions: i64,
    /// Of `chosen_sessions`, how many also recorded a
    /// `router_session_adoptions` row for this skill (i.e. the main agent
    /// actually adopted it, not just saw it recommended).
    pub adopted_sessions: i64,
}

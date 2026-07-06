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
    /// Capped at ~16 KB. Empty for pre-schema-v13 rows. Useful for
    /// diagnosing mis-routes — answers "what did the model see?".
    pub llm_input: String,
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

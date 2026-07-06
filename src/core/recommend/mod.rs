//! Opt-in LLM skill router for the `UserPromptSubmit` hook.
//!
//! Thin façade over the cohesive submodules below. Every item previously
//! reachable as `crate::core::recommend::X` is re-exported here unchanged so
//! consumers (cli / server / mcp) need zero edits. The split:
//! - `config` — `RecommendConfig` + provider library + toml load/save
//! - `router` — `recommend()` / `recommend_for_user()` + intent-memory BM25 prefilter + domain types
//! - `intent` — bounded current-session short memory + compact BM25 query text
//! - `enrich` — `enrich_skills()` worker pool + `reevaluate_skill()`
//! - `lang_validation` — deterministic per-field language enforcement
//! - `prompts` — enrich/feedback prompt builders + router system prompt
//! - `llm_call` — OpenAI-compat / Anthropic / Claude CLI transports + token accounting
//! - `hook_output` — hook stdout renderers + bootstrap guide
//! - `project_context` — CLAUDE.md `@`-reference parsing + injection
//! - `transcript` — session transcript reading for history / BM25
//! - `settings_hooks` — Claude settings.json hook install/uninstall
//! - `server_helpers` — local IPv4 / default local server URL

mod config;
mod enrich;
mod hook_output;
mod intent;
mod lang_validation;
mod llm_call;
mod project_context;
mod prompts;
mod router;
mod server_helpers;
mod session_id;
mod settings_hooks;
mod transcript;

// ── Public surface (external code depends on these exact paths) ──
pub use config::{Provider, ProviderEntry, RecommendConfig, SessionMode};
pub use enrich::{EnrichMode, EnrichReport, FeedbackReport, enrich_skills, reevaluate_skill};
pub use hook_output::{
    bootstrap_guide, format_for_hook, format_for_hook_full, format_for_hook_with_session,
};
pub use lang_validation::{find_language_mismatched_skills, summary_matches_lang};
pub use llm_call::{ProviderTestResult, test_provider_request};
pub use router::{
    RecommendedSkill, RouterDecision, RouterMode, RouterTurn, recommend, recommend_for_user,
    recommend_for_user_with_client,
};
pub use server_helpers::{default_local_server_url, local_ipv4};
pub use session_id::{is_runai_session_id, runai_session_id_from_native};
pub use settings_hooks::{
    HookInstallStatus, install_claude_hook, install_session_start_hook, uninstall_claude_hook,
    uninstall_session_start_hook,
};
pub use transcript::{
    recent_transcript_messages, recent_transcript_pairs, recent_user_prompts_for_bm25,
};

#[cfg(test)]
mod tests;

//! Centralised prompt template registry (PLANNING §1.3).
//!
//! Every LLM prompt template used anywhere in the binary lives under
//! `src/core/prompts/*.md` and is exposed here as a `pub const PROMPT_<NAME>: &str`.
//! `include_str!` keeps them compile-time bytes — runtime user override of the
//! body is intentionally NOT supported. Users can only toggle injection per
//! template via [`crate::core::prefs::UserPrefs::prompt_injection_flags`]; the
//! wording is part of the binary and can be read straight from `src/core/prompts/`.
//!
//! The canonical name (the `<NAME>` part of `PROMPT_<NAME>`) is the filename
//! stem in UPPER_SNAKE_CASE — `recommend_history_prefix.md` → `PROMPT_RECOMMEND_HISTORY_PREFIX`.
//! Use [`PROMPT_NAMES`] to enumerate every prompt for prefs UI / tests.
//!
//! See `src/core/prompts/AGENTS.md` for the per-prompt caller / variable /
//! output contract index.

/// recommend_system.md — router LLM system prompt (intent rules + decision
/// tree + few-shots + output format). Always injected when the recommend
/// feature is enabled — there is no toggle for this one because turning it
/// off leaves the router LLM with no instructions and produces garbage.
pub const PROMPT_RECOMMEND_SYSTEM: &str = include_str!("recommend_system.md");

/// recommend_user.md — user-turn scaffold built per request, with placeholders
/// `{USER_PROMPT}` / `{CWD_BLOCK}` / `{PROJECT_CONTEXT_BLOCK}` /
/// `{HISTORY_BLOCK}` / `{ALREADY_ROUTED_BLOCK}` / `{CANDIDATE_LISTING}` /
/// `{TOP_K}`. Always rendered — toggling the surrounding blocks is what users
/// configure, not this skeleton.
pub const PROMPT_RECOMMEND_USER: &str = include_str!("recommend_user.md");

/// recommend_history_prefix.md — recent transcript history block (`{HISTORY}`
/// placeholder). Optional: gated by
/// `prompt_injection_flags["recommend_history_prefix"]` (default true).
pub const PROMPT_RECOMMEND_HISTORY_PREFIX: &str = include_str!("recommend_history_prefix.md");

/// recommend_already_routed.md — names already routed in this session
/// (`{ALREADY_ROUTED}` placeholder). Optional: gated by
/// `prompt_injection_flags["recommend_already_routed"]` (default true).
pub const PROMPT_RECOMMEND_ALREADY_ROUTED: &str = include_str!("recommend_already_routed.md");

/// recommend_cwd_prefix.md — current working directory hint (`{CWD}`
/// placeholder). Optional: gated by
/// `prompt_injection_flags["recommend_cwd_prefix"]` (default true).
pub const PROMPT_RECOMMEND_CWD_PREFIX: &str = include_str!("recommend_cwd_prefix.md");

/// recommend_project_context.md — CLAUDE.md + `@`-referenced docs block
/// (`{PROJECT_DOCS}` placeholder). Optional: gated by
/// `prompt_injection_flags["recommend_project_context"]` (default true).
/// Also gated by the legacy `read_claude_md` flag on `UserPrefs` /
/// `RecommendConfig` — both must be true for the block to render.
pub const PROMPT_RECOMMEND_PROJECT_CONTEXT: &str = include_str!("recommend_project_context.md");

/// hook_output.md — single unified renderer template for the
/// `UserPromptSubmit` hook output. Always rendered when recommend produces
/// non-empty skills — no per-user toggle because suppressing this is what
/// the master `recommend_enabled` flag is for.
pub const PROMPT_HOOK_OUTPUT: &str = include_str!("hook_output.md");

/// Canonical list of every prompt template name. Used by prefs UI to
/// enumerate toggles and by tests to assert coverage.
pub const PROMPT_NAMES: &[&str] = &[
    "recommend_system",
    "recommend_user",
    "recommend_history_prefix",
    "recommend_already_routed",
    "recommend_cwd_prefix",
    "recommend_project_context",
    "hook_output",
];

/// Subset of [`PROMPT_NAMES`] that users can actually toggle off. The
/// remaining names are structurally required when the recommend feature is
/// enabled — there is no meaningful "off" for them short of disabling the
/// whole feature (`recommend_enabled = false`).
pub const TOGGLEABLE_PROMPT_NAMES: &[&str] = &[
    "recommend_history_prefix",
    "recommend_already_routed",
    "recommend_cwd_prefix",
    "recommend_project_context",
];

/// Return whether `name` is on the user-toggleable list.
pub fn is_toggleable(name: &str) -> bool {
    TOGGLEABLE_PROMPT_NAMES.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_constants_are_non_empty() {
        assert!(!PROMPT_RECOMMEND_SYSTEM.trim().is_empty());
        assert!(!PROMPT_RECOMMEND_USER.trim().is_empty());
        assert!(!PROMPT_RECOMMEND_HISTORY_PREFIX.trim().is_empty());
        assert!(!PROMPT_RECOMMEND_ALREADY_ROUTED.trim().is_empty());
        assert!(!PROMPT_RECOMMEND_CWD_PREFIX.trim().is_empty());
        assert!(!PROMPT_RECOMMEND_PROJECT_CONTEXT.trim().is_empty());
        assert!(!PROMPT_HOOK_OUTPUT.trim().is_empty());
    }

    #[test]
    fn prompt_names_match_toggleable_subset() {
        for n in TOGGLEABLE_PROMPT_NAMES {
            assert!(
                PROMPT_NAMES.contains(n),
                "toggleable name {n} missing from PROMPT_NAMES"
            );
            assert!(is_toggleable(n));
        }
        assert!(!is_toggleable("recommend_system"));
        assert!(!is_toggleable("recommend_user"));
        assert!(!is_toggleable("hook_output"));
    }

    #[test]
    fn placeholders_present_in_each_template() {
        // Each optional block exposes its expected placeholder so callers
        // don't have to grep — locking the contract here means renaming a
        // placeholder fails the build, not silently.
        assert!(PROMPT_RECOMMEND_HISTORY_PREFIX.contains("{HISTORY}"));
        assert!(PROMPT_RECOMMEND_ALREADY_ROUTED.contains("{ALREADY_ROUTED}"));
        assert!(PROMPT_RECOMMEND_CWD_PREFIX.contains("{CWD}"));
        assert!(PROMPT_RECOMMEND_PROJECT_CONTEXT.contains("{PROJECT_DOCS}"));
        assert!(PROMPT_RECOMMEND_USER.contains("{USER_PROMPT}"));
        assert!(PROMPT_HOOK_OUTPUT.contains("{CANDIDATES_BLOCK}"));
    }
}

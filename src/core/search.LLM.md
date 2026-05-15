---
module: core::search
file: src/core/search.rs
role: utility
---

# core::search — fuzzy matcher

## Purpose
Thin wrapper around `nucleo-matcher` (fzf v2 algorithm) for the four fuzzy-search call sites: `mcp::tools::sm_search`, `mcp::tools::sm_market`, `cli::Commands::Search`, `cli::Commands::Market`. Centralizes the matcher so all four behave identically.

## Public API
- `new_matcher() -> Matcher` — constructs a default `Matcher` with `Config::DEFAULT`.
- `fuzzy_score(matcher, haystack, needle) -> Option<u32>` — score a single haystack. `None` = no match, higher score = better fit.
- `fuzzy_score_any(matcher, needle, fields) -> Option<u32>` — max score across multiple fields (returns `None` only when no field matched).
- `rank(needle, items, fields_of) -> Vec<(item, u32)>` — score-and-sort helper using `Pattern::parse` (supports fzf operators like `^prefix`, `suffix$`, `'exact`); returns `(item, score)` sorted high-to-low.

## Key invariants
- All four call sites must use this module, not raw `&str::contains`. Centralization is what guarantees consistent behavior between the MCP tools and the CLI subcommands.
- Default case handling is `CaseMatching::Smart`: an all-lowercase needle is case-insensitive; any uppercase character makes the needle case-sensitive. This is fzf's standard behavior.
- Normalization is `Normalization::Smart` (Unicode normalization without altering ASCII).
- Score is `u32`; higher is better. Sort order should always be `b.cmp(&a)` (descending).
- Each call site that needs many comparisons should reuse one `Matcher` instance — internally it caches scratch buffers.

## Touch points
- **Downstream**: `nucleo_matcher::{Matcher, Config, Utf32Str}`, `nucleo_matcher::pattern::{Pattern, CaseMatching, Normalization}`.
- **Upstream**: `mcp::tools` (sm_search / sm_market), `cli::mod` (Commands::Search / Commands::Market).

## Gotchas
- `fuzzy_score` returns `u32`; nucleo's underlying type is also `u32` — do not wrap with `u32::from`, clippy flags it.
- `Pattern::parse` is heavier than direct `Matcher::fuzzy_match`. Use `rank` only when you want fzf-style operators in the needle; for plain fuzzy use `fuzzy_score` / `fuzzy_score_any`.
- Smart case can surprise users who type `FrontEnd` expecting to find lowercase `frontend-*` — score returns `None`. If product wants always-insensitive, switch to `CaseMatching::Ignore` here in one place and all four call sites pick it up.

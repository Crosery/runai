# core::prompts — LLM prompt template registry

> This folder (`src/core/prompts/`). One-liner: every LLM prompt template
> used in the binary, plus the typed registry that exposes them.

## Purpose
Hold the **wording** of every prompt the binary sends to an LLM in one
folder, exposed as `pub const PROMPT_<NAME>: &str` in `mod.rs` via
`include_str!`. Editing wording = editing the `.md` file. Runtime user
override of the prompt body is intentionally NOT supported — the binary
is the source of truth and stays grep-able from `src/core/prompts/`.

Users CAN toggle injection per template via
`UserPrefs::prompt_injection_flags: HashMap<String, bool>` (PLANNING §1.3).
Per-user flags are read at `/recommend` request time, scoped to the
authenticated user, and only the subset listed in `TOGGLEABLE_PROMPT_NAMES`
is meaningfully gated — the rest (system / user / hook output) are
structurally required as long as the recommend feature is enabled
(`UserPrefs::recommend_enabled` is the master switch for the whole feature).

## Layout
| File | Public const in `mod.rs` | Caller | Variables (`{PLACEHOLDER}`) | Output contract |
|---|---|---|---|---|
| `recommend_intent.md` | `PROMPT_RECOMMEND_INTENT` | Precise-only fixed Stage-1 system message | none | Byte-identical query-expansion contract. Fast never sends it. Precise sends bounded current input plus optional scoped intent memory/cwd and stores raw and cleaned outputs separately. |
| `recommend_system.md` | `PROMPT_RECOMMEND_SYSTEM` | router model fixed system message | none | Short routing contract: direct action admission, broad-domain action hit at most one, complementary workflow = compatible, alternatives = exclusive, `not-for` veto, follow-up exclusions, minimal sufficient set, and short-ID-only JSON output. Ranking tags never create admission. |
| `recommend_user.md` | `PROMPT_RECOMMEND_USER` | `recommend::router::recommend_for_user_with_client` | `{TASK_ANCHOR}`, `{INTENT_SUMMARY}`, `{BM25_CANDIDATE_LIMIT}`, `{CWD_BLOCK}`, `{PROJECT_CONTEXT_BLOCK}`, `{HISTORY_BLOCK}`, `{CANDIDATE_LISTING}` | Dynamic message carrying the bounded original task anchor in parallel with expansion and request-local candidate IDs. Fast leaves project/history blocks empty; Precise may inject them by per-user toggles. Candidate cards always label independently bounded `task/triggers/inputs/outputs/not-for`. |
| `recommend_history_prefix.md` | `PROMPT_RECOMMEND_HISTORY_PREFIX` | `recommend::router::recommend_for_user` (subbed into `{HISTORY_BLOCK}`) | `{HISTORY}` | block dropped to empty string when `prompt_injection_flags["recommend_history_prefix"] == false` or transcript history is empty |
| `recommend_cwd_prefix.md` | `PROMPT_RECOMMEND_CWD_PREFIX` | `recommend::router::recommend_for_user` (subbed into `{CWD_BLOCK}`) | `{CWD}` | block dropped to empty string when `prompt_injection_flags["recommend_cwd_prefix"] == false` or cwd is empty |
| `recommend_project_context.md` | `PROMPT_RECOMMEND_PROJECT_CONTEXT` | `recommend::project_context::read_project_context` (subbed into `{PROJECT_CONTEXT_BLOCK}`) | `{PROJECT_DOCS}` | block dropped to empty string when `prompt_injection_flags["recommend_project_context"] == false`, or `read_claude_md == false`, or CLAUDE.md is absent |
| `hook_output.md` | `PROMPT_HOOK_OUTPUT` | `recommend::hook_output::render_hook_output` | `{MODE}`, `{REASONING_BLOCK}`, `{CANDIDATES_BLOCK}`, `{SESSION_ID_ARG}`, `{ACTIVATION_DIRECTIVE}`, `{FEEDBACK_PROTOCOL_BLOCK}` | slimmed text returned as the `UserPromptSubmit` hook stdout; the 已推参考池 recall block and skip-reminder block were removed; COMPATIBLE defaults to activating all listed skills and only asks a minimal question for missing inputs/permissions/risk |

## Per-user toggle resolution
- The map lives in `UserPrefs::prompt_injection_flags: HashMap<String, bool>`,
  serialised inside `users.prefs_json`.
- Missing key → defaults to **true** (every prompt injected). This is the
  "fresh account / never visited prefs UI" path.
- Unauthenticated request (no Bearer / unknown api_key) → server passes
  `user_id_opt = None` to `recommend_for_user`; the router loads no user
  prefs and uses the default map = every toggleable prompt enabled.
- `recommend_for_user_with_client` reads `find_user_by_id(uid).prefs_json` **on every
  request**, freshly per call. There is no in-process prefs cache —
  switching the logged-in api_key picks up the new account's prefs on the
  very next `/recommend` hit (covered by `tests/prompts_multiuser_e2e.rs::switch_account_no_stale_prefs`).

## Frontmatter
Each `.md` file carries a single HTML-comment frontmatter line on its
first line of the form:

```
<!-- prompt: <name> | callers: <module>::<fn> | vars: {A},{B} -->
```

The frontmatter is documentation only. Raw constants include it, but
runtime prompt builders must call `crate::core::prompts::template_body(...)`
before sending text to an LLM or hook stdout; that helper strips exactly this
first-line metadata comment and leaves the template body untouched.
Tests in `mod.rs` assert each file is non-empty, contains its declared
placeholder, and strips frontmatter for runtime use.

## Touch points
- **Upstream**: `src/core/recommend/*` (every submodule that builds an LLM
  message), `src/core/prefs.rs` (`UserPrefs::prompt_injection_flags`),
  `src/server/prefs.rs` (GET/POST `/api/prefs`).
- **Downstream**: none — the `.md` files are leaves of the dependency
  graph.

## Tests
- Inline `#[cfg(test)] mod tests` in `mod.rs` pins every constant
  non-empty, the toggleable subset is a subset of `PROMPT_NAMES`, each
  template contains its declared placeholder, and `template_body` strips
  first-line frontmatter.
- `tests/prompts_multiuser_e2e.rs` exercises the per-user toggle plumbing
  end-to-end: A turns off a prompt, B doesn't; concurrent `recommend_for_user`
  calls see A's stripped LLM input and B's full one with zero cross-talk;
  switching the logged-in user picks up the new prefs immediately.

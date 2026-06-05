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
| `recommend_system.md` | `PROMPT_RECOMMEND_SYSTEM` | `recommend::llm_call::call_openai_compat` / `call_anthropic` / `call_claude_cli` (via `recommend::prompts::SYSTEM_PROMPT_TEMPLATE`) | none | system message body sent verbatim |
| `recommend_user.md` | `PROMPT_RECOMMEND_USER` | `recommend::router::recommend_for_user` | `{USER_PROMPT}` (×2), `{CWD_BLOCK}`, `{PROJECT_CONTEXT_BLOCK}`, `{HISTORY_BLOCK}`, `{ALREADY_ROUTED_BLOCK}`, `{CANDIDATE_LISTING}`, `{TOP_K}` | user message body sent to router LLM |
| `recommend_history_prefix.md` | `PROMPT_RECOMMEND_HISTORY_PREFIX` | `recommend::router::recommend_for_user` (subbed into `{HISTORY_BLOCK}`) | `{HISTORY}` | block dropped to empty string when `prompt_injection_flags["recommend_history_prefix"] == false` or transcript history is empty |
| `recommend_already_routed.md` | `PROMPT_RECOMMEND_ALREADY_ROUTED` | `recommend::router::recommend_for_user` (subbed into `{ALREADY_ROUTED_BLOCK}`) | `{ALREADY_ROUTED}` | block dropped to empty string when `prompt_injection_flags["recommend_already_routed"] == false` or already_routed list is empty |
| `recommend_cwd_prefix.md` | `PROMPT_RECOMMEND_CWD_PREFIX` | `recommend::router::recommend_for_user` (subbed into `{CWD_BLOCK}`) | `{CWD}` | block dropped to empty string when `prompt_injection_flags["recommend_cwd_prefix"] == false` or cwd is empty |
| `recommend_project_context.md` | `PROMPT_RECOMMEND_PROJECT_CONTEXT` | `recommend::project_context::read_project_context` (subbed into `{PROJECT_CONTEXT_BLOCK}`) | `{PROJECT_DOCS}` | block dropped to empty string when `prompt_injection_flags["recommend_project_context"] == false`, or `read_claude_md == false`, or CLAUDE.md is absent |
| `hook_output.md` | `PROMPT_HOOK_OUTPUT` | `recommend::hook_output::render_hook_output` | `{MODE}`, `{REASONING_BLOCK}`, `{CANDIDATES_BLOCK}`, `{ACTIVATION_DIRECTIVE}`, `{SKIP_REMINDER_BLOCK}`, `{SERVER_URL}`, `{USER_HEADER}`, `{SESSION_HISTORY_BLOCK}`, `{FEEDBACK_PROTOCOL_BLOCK}` | rendered text returned as the `UserPromptSubmit` hook stdout |

## Per-user toggle resolution
- The map lives in `UserPrefs::prompt_injection_flags: HashMap<String, bool>`,
  serialised inside `users.prefs_json`.
- Missing key → defaults to **true** (every prompt injected). This is the
  "fresh account / never visited prefs UI" path.
- Unauthenticated request (no Bearer / unknown api_key) → server passes
  `user_id_opt = None` to `recommend_for_user`; the router loads no user
  prefs and uses the default map = every toggleable prompt enabled.
- `recommend_for_user` reads `find_user_by_id(uid).prefs_json` **on every
  request**, freshly per call. There is no in-process prefs cache —
  switching the logged-in api_key picks up the new account's prefs on the
  very next `/recommend` hit (covered by `tests/prompts_multiuser_e2e.rs::switch_account_no_stale_prefs`).

## Frontmatter
Each `.md` file carries a single HTML-comment frontmatter line on its
first line of the form:

```
<!-- prompt: <name> | callers: <module>::<fn> | vars: {A},{B} -->
```

The frontmatter is documentation only — it is not parsed at runtime.
`include_str!` slurps the whole file body, so the comment travels with
the template into the LLM message (HTML comments are ignored by LLMs
and don't confuse them when sent). Tests in `mod.rs` assert each file
is non-empty and contains its declared placeholder.

## Touch points
- **Upstream**: `src/core/recommend/*` (every submodule that builds an LLM
  message), `src/core/prefs.rs` (`UserPrefs::prompt_injection_flags`),
  `src/server/prefs.rs` (GET/POST `/api/prefs`).
- **Downstream**: none — the `.md` files are leaves of the dependency
  graph.

## Tests
- Inline `#[cfg(test)] mod tests` in `mod.rs` pins every constant
  non-empty, the toggleable subset is a subset of `PROMPT_NAMES`, and
  each template contains its declared placeholder.
- `tests/prompts_multiuser_e2e.rs` exercises the per-user toggle plumbing
  end-to-end: A turns off a prompt, B doesn't; concurrent `recommend_for_user`
  calls see A's stripped LLM input and B's full one with zero cross-talk;
  switching the logged-in user picks up the new prefs immediately.

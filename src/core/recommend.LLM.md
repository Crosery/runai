---
module: core::recommend
file: src/core/recommend.rs
role: feature
---

# core::recommend — LLM skill router

## Purpose
Opt-in skill auto-routing. A small LLM (default `deepseek-v4-flash` via OpenAI-compatible API) looks at the user prompt + the list of installed skills (name + description) and returns the top-K most relevant skills. The CLI subcommand emits each chosen skill's full `SKILL.md` content + filesystem path as plain markdown on stdout, which `UserPromptSubmit` hook then injects into the main Claude Code prompt as additional context.

Disabled by default. User must run `runai recommend setup` (interactive) or write `~/.runai/config.toml` manually before any LLM call happens.

## Public API
- `struct RecommendConfig` — `enabled`, `provider`, `base_url`, `model`, `api_key`, `top_k`, `min_prompt_len`. Defaults: disabled, openai-compat, DeepSeek endpoint, `deepseek-v4-flash`, top_k=3, min_prompt_len=0.
- `enum Provider` — `OpenaiCompat` (default) or `Anthropic`.
- `enum RouterMode` — `Compatible` (skills co-loadable, all primaries inlined) or `Exclusive` (user must pick one).
- `RecommendConfig::load(paths)` / `save(paths)` — toml at `~/.runai/config.toml`. Save sets `0o600` on unix.
- `RecommendConfig::effective_api_key()` — config field first, then `RUNAI_RECOMMEND_API_KEY` env.
- `recommend(mgr, prompt, transcript_path, session_id) -> RouterDecision` — top-level entry. `transcript_path` is the session jsonl (from Claude Code hook stdin); the last 6 user/assistant text messages get appended so the router can recognize replies like "use figma-component-mapping". `session_id` (also from stdin JSON) drives **per-session memory**: every skill name this session has already been recommended is queried from the DB and injected into the LLM prompt as "already_routed", plus runai filters them out a second time on the wire so the same skill can't be re-pushed within one Claude Code session even if the LLM tries.
- `recent_transcript_messages(path, n)` — read the last `n` user/assistant text messages from a Claude Code transcript jsonl, oldest-first. Tool calls/results filtered out; only plain text kept.
- `format_for_hook(decision) -> String` — markdown formatter for hook stdout. Behavior depends on `RouterMode`:
  - **Single skill**: inline full SKILL.md if `≤ 8 KB`, otherwise pointer mode (`Read once at path`).
  - **Compatible multi**: every skill inlined as its own section (each ≤ 4 KB per skill, total ≤ 9 KB hard cap); skills that don't fit fall back to pointer lines.
  - **Exclusive multi**: show name + description for each candidate; instruct main agent NOT to pick — let the user choose, then runai injects the chosen full SKILL.md on the next prompt round automatically.
- `struct RecommendedSkill { name, description, path, content }` — content is the primary's full SKILL.md (single-match) or each skill's full SKILL.md (compatible-multi); empty for alternates in exclusive-multi mode.
- `struct RouterDecision { mode, skills }` — what `recommend()` returns; `format_for_hook` takes this.

## Key invariants
- **Disabled by default.** `RecommendConfig::default().enabled == false`. Loading a missing config returns default. `recommend()` returns an empty `RouterDecision` when disabled — no LLM call, no network, no log.
- **Per-session de-duplication is enforced on the wire, not only in the prompt.** When `session_id` is present, `db.router_session_routed_skills(sid)` returns the union of every chosen skill across this session's prior `router_events` rows. Two layers of defense:
  1. Inject `ALREADY_ROUTED: [a, b, c]` into the LLM user message — the router system prompt instructs the model to skip these.
  2. Post-process: even if the LLM ignores the instruction and re-suggests a previously-routed skill, runai filters it out before `format_for_hook` runs. Net effect: no skill can repeat within one Claude Code session.
- **Mode tag comes from the LLM, defaults to `Exclusive` on parse failure.** First line of LLM content must be `COMPATIBLE` or `EXCLUSIVE`. Missing/unknown tag → defaults to `Exclusive` (safer — main agent will ask user to pick). `split_mode_and_names` handles the parsing.
- **LLM output is filtered against installed skills.** Names returned by the model are intersected with `list_resources(Skill, _)`; hallucinated names are dropped silently.
- **SKILL.md emission policy depends on mode**:
  - Exclusive single → full SKILL.md inlined (or pointer mode if >8 KB).
  - Exclusive multi → no full content; candidate list only; user picks → next round becomes single match → full content.
  - Compatible multi → all skills' full content inlined under a combined 9 KB cap; over-cap skills get pointer lines.
- **API key never logged or echoed.** `recommend status` shows only `set in config` / `set via env` / `missing`. Config file is `0o600`.
- **Telemetry rows include session_id and mode** (DB schema v6). `runai recommend stats` / `sm_recommend_stats` can slice per-session usage and per-mode distribution.
- **Returns success even when LLM call fails.** Errors go to stderr prefixed with `# runai recommend skipped:` so the hook stdout stays parseable; main Claude continues unimpaired. The failed call is still persisted with `status='error'` for audit.

## Touch points
- **Upstream**: `cli::Commands::Recommend` dispatch; user-facing subcommands `runai recommend <prompt>` / `setup` / `status` / `hook-snippet`.
- **Downstream**: `SkillManager::list_resources`, `AppPaths::{config_path, skills_dir}`, `reqwest::blocking::Client` (POSTs to `{base_url}/chat/completions` for openai-compat or `{base_url}/v1/messages` for anthropic).
- **External integration**: Claude Code's `UserPromptSubmit` hook in `~/.claude/settings.json`. The hook command is just `runai recommend` (no args). Claude Code pipes its standard hook JSON to stdin (`{prompt, transcript_path, session_id, cwd, hook_event_name}`). runai reads it, optionally reads recent transcript messages, calls the router, and emits markdown to stdout. Claude Code injects that as additional prompt context. Runs in <2s with DeepSeek.

## Gotchas
- Anthropic provider hits `{base_url}/v1/messages` — pass the host without `/v1` (e.g. `https://api.anthropic.com`) since the `/v1/messages` suffix is appended in code. Openai-compat hits `{base_url}/chat/completions` and expects `base_url` to already include the API version segment (e.g. `https://api.deepseek.com/v1`).
- Setup prompts for the API key in plain text — there is no hidden input. Tradeoff for portability; the config file gets `0o600` afterwards.
- Hook stdout becomes part of the main Claude's prompt — keep `top_k` low to avoid blowing the context budget on giant SKILL.md files.
- `Provider::OpenaiCompat` works with any OpenAI-compatible service (DeepSeek, Moonshot, Groq, vLLM, etc.) but the LLM must follow the "one name per line" output convention from `SYSTEM_PROMPT`. Smaller / less instruction-tuned models may need a stricter prompt.
- The setup wizard reads stdin line-by-line; piping in `runai recommend setup < answers.txt` works for automation.

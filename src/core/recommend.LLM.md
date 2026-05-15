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
- `struct RecommendConfig` — `enabled`, `provider`, `base_url`, `model`, `api_key`, `top_k`, `min_prompt_len`. Defaults: disabled, openai-compat, DeepSeek endpoint, `deepseek-v4-flash`, top_k=3, min_prompt_len=10.
- `enum Provider` — `OpenaiCompat` (default) or `Anthropic`.
- `RecommendConfig::load(paths)` / `save(paths)` — toml at `~/.runai/config.toml`. Save sets `0o600` on unix.
- `RecommendConfig::effective_api_key()` — config field first, then `RUNAI_RECOMMEND_API_KEY` env.
- `recommend(mgr, prompt, transcript_path) -> Vec<RecommendedSkill>` — top-level entry. `transcript_path` is the session jsonl (from Claude Code hook stdin); when present, the last 6 user/assistant text messages are appended to the LLM input so the router can recognize replies like "use figma-component-mapping" and pick the right skill on the next round. Returns empty when disabled, prompt too short, or no skills installed.
- `recent_transcript_messages(path, n)` — read the last `n` user/assistant text messages from a Claude Code transcript jsonl, oldest-first. Tool calls/results filtered out; only plain text kept.
- `format_for_hook(skills) -> String` — markdown formatter for hook stdout. First skill is `Primary` with full SKILL.md content injected; the rest are `Alternates` with only name+description so the main agent can ask the user which to load.
- `struct RecommendedSkill { name, description, path, content }` — content is empty for alternates, full SKILL.md for the primary pick.

## Key invariants
- **Disabled by default.** `RecommendConfig::default().enabled == false`. Loading a missing config returns default. `recommend()` returns `Ok(vec![])` when disabled — no LLM call, no network, no log.
- **No LLM call below `min_prompt_len` chars.** Default 10. Short prompts (greetings, yes/no) silently skip routing.
- **LLM output is filtered against installed skills.** Names returned by the model are intersected with `list_resources(Skill, _)`; hallucinated names are dropped.
- **Only `SKILL.md` is emitted, and only for the primary pick.** Resolved as `paths.skills_dir().join(name).join("SKILL.md")`. If the file is unreadable, the primary is silently dropped (not erroring the hook). Alternates only surface name+description; their full content arrives on a later prompt round when the user picks one.
- **API key never logged or echoed.** `recommend status` shows only `set in config` / `set via env` / `missing`. Config file is `0o600`.
- **Returns success even when LLM call fails.** Errors go to stderr prefixed with `# runai recommend skipped:` so the hook stdout stays parseable; main Claude continues unimpaired.

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

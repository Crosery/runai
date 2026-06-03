//! Backend LLM transports + token accounting.
//!
//! Three providers behind one shape: OpenAI-compatible (`/chat/completions`),
//! Anthropic (`/v1/messages`), and the `claude` CLI subprocess. `RouterTurn`
//! history is threaded into the messages array for Conversation mode; the
//! summarisation path (`call_summary_llm`) is always oneshot. `RouterCallStats`
//! captures per-provider token usage for telemetry.

use anyhow::{Context, Result, bail};

use super::config::{Provider, RecommendConfig};
use super::prompts::SYSTEM_PROMPT_TEMPLATE;
use super::router::RouterTurn;

#[derive(Debug, Default, Clone)]
pub(super) struct RouterCallStats {
    pub(super) prompt_tokens: i64,
    pub(super) completion_tokens: i64,
    pub(super) reasoning_tokens: i64,
    pub(super) total_tokens: i64,
    pub(super) cache_hit_tokens: i64,
    pub(super) cache_miss_tokens: i64,
}

/// Dedicated summarisation LLM call. Reuses the configured backend but with
/// a tighter timeout (no thinking, short output) and returns the raw text.
pub(super) fn call_summary_llm(
    cfg: &RecommendConfig,
    api_key: &str,
    user_msg: &str,
) -> Result<String> {
    // Enrich passes are always oneshot — they index a single SKILL.md
    // without conversational state, so no history is ever threaded.
    let no_history: &[RouterTurn] = &[];
    let (raw, _stats) = match cfg.provider {
        Provider::OpenaiCompat => call_openai_compat(cfg, api_key, user_msg, no_history)?,
        Provider::Anthropic => call_anthropic(cfg, api_key, user_msg, no_history)?,
        Provider::ClaudeCli => call_claude_cli(cfg, user_msg)?,
    };
    Ok(raw)
}

/// Run the router via `claude -p --model <model>`. Uses the user's Claude
/// Code session (cookies + Max plan quota), no API key. Slower than direct
/// API because every spawn boots Claude Code's full system prompt.
pub(super) fn call_claude_cli(
    cfg: &RecommendConfig,
    user_msg: &str,
) -> Result<(String, RouterCallStats)> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let combined = format!("{SYSTEM_PROMPT_TEMPLATE}\n\n{user_msg}");
    let mut child = Command::new("claude")
        .arg("-p")
        .arg("--model")
        .arg(&cfg.model)
        .arg("--output-format")
        .arg("json")
        .arg("--no-session-persistence")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn `claude` — make sure Claude Code CLI is on PATH")?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(combined.as_bytes())
            .context("write prompt to claude stdin")?;
    }
    let out = child.wait_with_output().context("wait for claude")?;
    if !out.status.success() {
        bail!(
            "claude exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| {
        anyhow::anyhow!(
            "decode claude json: {e}; first 200 bytes: {:?}",
            String::from_utf8_lossy(&out.stdout[..out.stdout.len().min(200)])
        )
    })?;
    let content = v["result"].as_str().unwrap_or_default();
    if std::env::var("RUNAI_RECOMMEND_DEBUG").is_ok() {
        eprintln!(
            "[recommend debug] claude raw result: {:?}; duration_ms: {} usage: {}",
            content,
            v.get("duration_ms")
                .map(|x| x.to_string())
                .unwrap_or_default(),
            v.get("usage").map(|u| u.to_string()).unwrap_or_default()
        );
    }
    let usage = v.get("usage");
    let get_i64 = |k: &str| -> i64 {
        usage
            .and_then(|u| u.get(k))
            .and_then(|x| x.as_i64())
            .unwrap_or(0)
    };
    let input = get_i64("input_tokens");
    let output = get_i64("output_tokens");
    let cache_read = get_i64("cache_read_input_tokens");
    let cache_create = get_i64("cache_creation_input_tokens");
    let stats = RouterCallStats {
        prompt_tokens: input + cache_read + cache_create,
        completion_tokens: output,
        reasoning_tokens: 0,
        total_tokens: input + cache_read + cache_create + output,
        cache_hit_tokens: cache_read,
        cache_miss_tokens: cache_create,
    };
    Ok((content.to_string(), stats))
}

fn parse_openai_usage(v: &serde_json::Value) -> RouterCallStats {
    let u = match v.get("usage") {
        Some(u) => u,
        None => return RouterCallStats::default(),
    };
    let get_i64 = |k: &str| -> i64 { u.get(k).and_then(|x| x.as_i64()).unwrap_or(0) };
    let reasoning = u
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    RouterCallStats {
        prompt_tokens: get_i64("prompt_tokens"),
        completion_tokens: get_i64("completion_tokens"),
        reasoning_tokens: reasoning,
        total_tokens: get_i64("total_tokens"),
        cache_hit_tokens: get_i64("prompt_cache_hit_tokens"),
        cache_miss_tokens: get_i64("prompt_cache_miss_tokens"),
    }
}

fn parse_anthropic_usage(v: &serde_json::Value) -> RouterCallStats {
    let u = match v.get("usage") {
        Some(u) => u,
        None => return RouterCallStats::default(),
    };
    let get_i64 = |k: &str| -> i64 { u.get(k).and_then(|x| x.as_i64()).unwrap_or(0) };
    let input = get_i64("input_tokens");
    let output = get_i64("output_tokens");
    RouterCallStats {
        prompt_tokens: input,
        completion_tokens: output,
        reasoning_tokens: 0,
        total_tokens: input + output,
        cache_hit_tokens: get_i64("cache_read_input_tokens"),
        cache_miss_tokens: get_i64("cache_creation_input_tokens"),
    }
}

pub(super) fn call_openai_compat(
    cfg: &RecommendConfig,
    api_key: &str,
    user_msg: &str,
    history: &[RouterTurn],
) -> Result<(String, RouterCallStats)> {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    // Disable thinking on reasoning models so the router answers instantly.
    // DeepSeek V4 honors `thinking.type=disabled` (drops reasoning_tokens to
    // None). For non-reasoning models or other OpenAI-compat backends this
    // field is silently ignored, so it's safe to always send.
    // max_tokens is intentionally omitted — let the model use its full budget.
    let mut messages = Vec::with_capacity(1 + history.len() * 2 + 1);
    messages.push(serde_json::json!({
        "role": "system",
        "content": SYSTEM_PROMPT_TEMPLATE,
    }));
    for turn in history {
        messages.push(serde_json::json!({"role": "user", "content": turn.user}));
        messages.push(serde_json::json!({"role": "assistant", "content": turn.assistant}));
    }
    messages.push(serde_json::json!({"role": "user", "content": user_msg}));
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "thinking": {"type": "disabled"},
        "stream": false,
    });
    let resp = reqwest::blocking::Client::builder()
        // 60s timeout accommodates OpenRouter free tier which routes to
        // third-party providers and can take 5-10s. DeepSeek direct stays at
        // ~0.6s. Long-tail bound to keep hook from hanging the main agent.
        .timeout(std::time::Duration::from_secs(60))
        .build()?
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        bail!(
            "router HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    // OpenRouter sends SSE-style keep-alive blanks before the final JSON, so
    // `resp.json()` chokes. Read as text and parse the trimmed body — works
    // for DeepSeek direct (single JSON line) and OpenRouter (blanks + JSON).
    let raw = resp.text().context("read router body")?;
    let trimmed = raw.trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        anyhow::anyhow!(
            "decode router json: {e}; first 200 bytes: {:?}",
            &trimmed.chars().take(200).collect::<String>()
        )
    })?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default();
    if std::env::var("RUNAI_RECOMMEND_DEBUG").is_ok() {
        eprintln!(
            "[recommend debug] LLM raw content: {:?}; usage: {}",
            content,
            v.get("usage").map(|u| u.to_string()).unwrap_or_default()
        );
    }
    Ok((content.to_string(), parse_openai_usage(&v)))
}

pub(super) fn call_anthropic(
    cfg: &RecommendConfig,
    api_key: &str,
    user_msg: &str,
    history: &[RouterTurn],
) -> Result<(String, RouterCallStats)> {
    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let mut messages = Vec::with_capacity(history.len() * 2 + 1);
    for turn in history {
        messages.push(serde_json::json!({"role": "user", "content": turn.user}));
        messages.push(serde_json::json!({"role": "assistant", "content": turn.assistant}));
    }
    messages.push(serde_json::json!({"role": "user", "content": user_msg}));
    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 256,
        "system": SYSTEM_PROMPT_TEMPLATE,
        "messages": messages,
    });
    let resp = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        bail!(
            "router HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let v: serde_json::Value = resp.json().context("decode router json")?;
    let content = v["content"][0]["text"].as_str().unwrap_or_default();
    Ok((content.to_string(), parse_anthropic_usage(&v)))
}

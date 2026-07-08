//! Backend LLM transports + token accounting.
//!
//! Three providers behind one shape: OpenAI-compatible (`/chat/completions`),
//! Anthropic (`/v1/messages`), and the `claude` CLI subprocess. `RouterTurn`
//! history is threaded into the messages array for Conversation mode; the
//! summarisation path (`call_summary_llm`) is always oneshot. `RouterCallStats`
//! captures per-provider token usage for telemetry.

use anyhow::{Context, Result, bail};
use std::time::{Duration, Instant};

use super::config::{Provider, RecommendConfig};
use super::prompts::system_prompt_template;
use super::router::RouterTurn;

const PROVIDER_TEST_PROMPT: &str = "Reply with exactly OK.";
const PROVIDER_TEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Output-token ceiling for the Stage-1 intent condensation call. Its output
/// is a short BM25 artifact (`intent:` + a few include/exclude lines, capped at
/// 800 chars in the prompt), so a tight cap prevents a weak model from running
/// away and cuts tail latency.
pub(super) const INTENT_MAX_TOKENS: u32 = 512;
/// Output-token ceiling for the Stage-2 router call. Output is a mode tag + one
/// `reasoning:` line + a short skill-name list — never long prose.
pub(super) const ROUTER_MAX_TOKENS: u32 = 400;
/// Fallback `max_tokens` for Anthropic when the caller passes `None` (the
/// enrich/summary path, which needs room for the 6-line summary contract).
/// OpenAI-compatible omits `max_tokens` entirely on `None` (full budget).
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderTestResult {
    pub reply: String,
}

/// Send a tiny real request through the configured provider/model.
///
/// The dashboard Admin pane uses this as a connectivity check. It proves the
/// selected model can answer through the same transport family the router uses,
/// without running the full skill-selection prompt.
pub fn test_provider_request(cfg: &RecommendConfig) -> Result<ProviderTestResult> {
    if cfg.model.trim().is_empty() {
        bail!("model is required");
    }
    match cfg.provider {
        Provider::OpenaiCompat => {
            let api_key = cfg
                .effective_api_key()
                .context("api_key not configured for provider test")?;
            test_openai_compat(cfg, &api_key)
        }
        Provider::Anthropic => {
            let api_key = cfg
                .effective_api_key()
                .context("api_key not configured for provider test")?;
            test_anthropic(cfg, &api_key)
        }
        Provider::ClaudeCli => test_claude_cli(cfg),
    }
}

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
    // Enrich/summary needs the full output budget for the 6-line summary
    // contract, so pass `None` (OpenAI omits max_tokens; Anthropic falls back
    // to ANTHROPIC_DEFAULT_MAX_TOKENS).
    let (raw, _stats) = match cfg.provider {
        Provider::OpenaiCompat => call_openai_compat(cfg, api_key, user_msg, no_history, None)?,
        Provider::Anthropic => call_anthropic(cfg, api_key, user_msg, no_history, None)?,
        Provider::ClaudeCli => call_claude_cli(cfg, user_msg)?,
    };
    Ok(raw)
}

fn claude_cli_json(
    cfg: &RecommendConfig,
    prompt: &str,
    timeout: Option<Duration>,
) -> Result<serde_json::Value> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::thread;

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
            .write_all(prompt.as_bytes())
            .context("write prompt to claude stdin")?;
    }
    drop(child.stdin.take());

    let out = if let Some(limit) = timeout {
        let started = Instant::now();
        loop {
            match child.try_wait().context("poll claude")? {
                Some(_status) => {
                    break child.wait_with_output().context("collect claude output")?;
                }
                None if started.elapsed() >= limit => {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("claude provider test timed out after {}s", limit.as_secs());
                }
                None => thread::sleep(Duration::from_millis(50)),
            }
        }
    } else {
        child.wait_with_output().context("wait for claude")?
    };
    if !out.status.success() {
        bail!(
            "claude exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    serde_json::from_slice(&out.stdout).map_err(|e| {
        anyhow::anyhow!(
            "decode claude json: {e}; first 200 bytes: {:?}",
            String::from_utf8_lossy(&out.stdout[..out.stdout.len().min(200)])
        )
    })
}

/// Run the router via `claude -p --model <model>`. Uses the user's Claude
/// Code session (cookies + Max plan quota), no API key. Slower than direct
/// API because every spawn boots Claude Code's full system prompt.
pub(super) fn call_claude_cli(
    cfg: &RecommendConfig,
    user_msg: &str,
) -> Result<(String, RouterCallStats)> {
    call_claude_cli_with_system(cfg, system_prompt_template(), user_msg)
}

pub(super) fn call_claude_cli_with_system(
    cfg: &RecommendConfig,
    system_prompt: &str,
    user_msg: &str,
) -> Result<(String, RouterCallStats)> {
    let combined = format!("{system_prompt}\n\n{user_msg}");
    let v = claude_cli_json(cfg, &combined, None)?;
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

fn test_claude_cli(cfg: &RecommendConfig) -> Result<ProviderTestResult> {
    let v = claude_cli_json(cfg, PROVIDER_TEST_PROMPT, Some(PROVIDER_TEST_TIMEOUT))?;
    Ok(ProviderTestResult {
        reply: v["result"].as_str().unwrap_or_default().trim().to_string(),
    })
}

fn test_openai_compat(cfg: &RecommendConfig, api_key: &str) -> Result<ProviderTestResult> {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            {"role": "user", "content": PROVIDER_TEST_PROMPT}
        ],
        "stream": false,
        "max_tokens": 8,
        "thinking": {"type": "disabled"},
    });
    let resp = reqwest::blocking::Client::builder()
        .timeout(PROVIDER_TEST_TIMEOUT)
        .build()?
        .post(&url)
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        bail!(
            "provider test HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let raw = resp.text().context("read provider test body")?;
    let trimmed = raw.trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        anyhow::anyhow!(
            "decode provider test json: {e}; first 200 bytes: {:?}",
            &trimmed.chars().take(200).collect::<String>()
        )
    })?;
    Ok(ProviderTestResult {
        reply: v["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
    })
}

fn test_anthropic(cfg: &RecommendConfig, api_key: &str) -> Result<ProviderTestResult> {
    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 8,
        "messages": [
            {"role": "user", "content": PROVIDER_TEST_PROMPT}
        ],
    });
    let resp = reqwest::blocking::Client::builder()
        .timeout(PROVIDER_TEST_TIMEOUT)
        .build()?
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .with_context(|| format!("POST {url}"))?;
    if !resp.status().is_success() {
        bail!(
            "provider test HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        );
    }
    let v: serde_json::Value = resp.json().context("decode provider test json")?;
    Ok(ProviderTestResult {
        reply: v["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string(),
    })
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
    max_tokens: Option<u32>,
) -> Result<(String, RouterCallStats)> {
    call_openai_compat_with_system(
        cfg,
        api_key,
        system_prompt_template(),
        user_msg,
        history,
        max_tokens,
    )
}

pub(super) fn call_openai_compat_with_system(
    cfg: &RecommendConfig,
    api_key: &str,
    system_prompt: &str,
    user_msg: &str,
    history: &[RouterTurn],
    max_tokens: Option<u32>,
) -> Result<(String, RouterCallStats)> {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    // Disable thinking on reasoning models so the router answers instantly.
    // DeepSeek V4 honors `thinking.type=disabled` (drops reasoning_tokens to
    // None). For non-reasoning models or other OpenAI-compat backends this
    // field is silently ignored, so it's safe to always send.
    let mut messages = Vec::with_capacity(1 + history.len() * 2 + 1);
    messages.push(serde_json::json!({
        "role": "system",
        "content": system_prompt,
    }));
    for turn in history {
        messages.push(serde_json::json!({"role": "user", "content": turn.user}));
        messages.push(serde_json::json!({"role": "assistant", "content": turn.assistant}));
    }
    messages.push(serde_json::json!({"role": "user", "content": user_msg}));
    // temperature=0 makes the router/intent output deterministic (better prefix
    // cache reuse, no run-to-run drift). max_tokens is only sent when the caller
    // bounds it (Stage-1/Stage-2); enrich passes None to keep the full budget.
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "thinking": {"type": "disabled"},
        "stream": false,
        "temperature": 0,
    });
    if let Some(mt) = max_tokens {
        body["max_tokens"] = serde_json::json!(mt);
    }
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
    max_tokens: Option<u32>,
) -> Result<(String, RouterCallStats)> {
    call_anthropic_with_system(
        cfg,
        api_key,
        system_prompt_template(),
        user_msg,
        history,
        max_tokens,
    )
}

pub(super) fn call_anthropic_with_system(
    cfg: &RecommendConfig,
    api_key: &str,
    system_prompt: &str,
    user_msg: &str,
    history: &[RouterTurn],
    max_tokens: Option<u32>,
) -> Result<(String, RouterCallStats)> {
    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let mut messages = Vec::with_capacity(history.len() * 2 + 1);
    for turn in history {
        messages.push(serde_json::json!({"role": "user", "content": turn.user}));
        messages.push(serde_json::json!({"role": "assistant", "content": turn.assistant}));
    }
    messages.push(serde_json::json!({"role": "user", "content": user_msg}));
    // Anthropic requires max_tokens; None (enrich) falls back to the summary
    // budget. temperature=0 for deterministic routing.
    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": max_tokens.unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS),
        "temperature": 0,
        "system": system_prompt,
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

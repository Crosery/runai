//! Claude Code session transcript reading for history / BM25 recall.
//!
//! Pulls the most recent user/assistant text turns out of a session jsonl,
//! dropping tool calls/results. Used both to thread short history into the
//! router LLM input and to stitch earlier user turns into the BM25 prefilter
//! query (so "换一个" style follow-ups keep the original topic in scope).

use std::path::Path;

/// Read the most recent `n` user/assistant text messages from a Claude Code
/// session jsonl, oldest-first. Tool calls/results are dropped; only plain
/// text is kept. Returns empty string on any read or parse error.
pub fn recent_transcript_messages(transcript_path: &Path, n: usize) -> String {
    let msgs = recent_transcript_pairs(transcript_path, n);
    msgs.iter()
        .map(|(r, t)| format!("[{r}] {t}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Return the most recent `n` user+assistant turns as (role, text) pairs,
/// oldest-first. Tool calls / results filtered out. Each text capped at
/// 400 chars. Used by `recent_transcript_messages` (renders to a single
/// string for the LLM) and `recent_user_prompts_for_bm25` (returns only
/// the user-side strings for keyword recall).
pub fn recent_transcript_pairs(transcript_path: &Path, n: usize) -> Vec<(String, String)> {
    let raw = match std::fs::read_to_string(transcript_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut msgs: Vec<(String, String)> = Vec::new();
    for line in raw.lines() {
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if t != "user" && t != "assistant" {
            continue;
        }
        let m = match v.get("message") {
            Some(m) => m,
            None => continue,
        };
        let role = m
            .get("role")
            .and_then(|x| x.as_str())
            .unwrap_or(t)
            .to_string();
        let text = match m.get("content") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|block| {
                    if block.get("type").and_then(|x| x.as_str()) == Some("text") {
                        block.get("text").and_then(|x| x.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let truncated: String = trimmed.chars().take(400).collect();
        msgs.push((role, truncated));
    }
    let take_from = msgs.len().saturating_sub(n);
    msgs[take_from..].to_vec()
}

/// Return the last `n` user messages concatenated as a single string,
/// usable as extra BM25 prefilter input. Assistant messages are dropped
/// (they're the main agent's output — feeding them back would self-bias
/// the prefilter toward whatever the agent just talked about).
///
/// Why this exists: BM25 prefilter sees only `user_prompt`. Short
/// follow-up prompts like "不对换一个" / "有没有其他的 ppt" carry zero
/// keywords on their own — the topic ("ppt") lives in earlier user
/// turns. Without history, ppt-related skills get filtered out of the
/// bounded top-K candidate set before the LLM router ever sees them.
pub fn recent_user_prompts_for_bm25(transcript_path: &Path, n: usize) -> String {
    let pairs = recent_transcript_pairs(transcript_path, n);
    pairs
        .into_iter()
        .filter(|(role, _)| role == "user")
        .map(|(_, t)| t)
        .collect::<Vec<_>>()
        .join(" ")
}

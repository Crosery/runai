//! Bounded current-session intent memory and BM25 query construction.
//!
//! This layer keeps the router's context stable: every turn can add one short
//! memory item, but the DB layer trims the queue to the configured limit. The
//! BM25 retriever uses the compact summary here instead of the raw hook payload,
//! so pasted history or huge prompts do not dominate candidate recall.

const MEMORY_ITEM_CHAR_LIMIT: usize = 240;

fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn build_intent_memory_from_prompt(prompt: &str) -> String {
    normalize_ws(prompt)
        .chars()
        .take(MEMORY_ITEM_CHAR_LIMIT)
        .collect()
}

pub(super) fn build_intent_summary(
    user_prompt: &str,
    cwd: Option<&str>,
    client_kind: &str,
    memory: &[String],
) -> String {
    let mut lines = Vec::new();
    let prompt = build_intent_memory_from_prompt(user_prompt);
    if !prompt.is_empty() {
        lines.push(format!("intent: {prompt}"));
    }
    if let Some(cwd) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        lines.push(format!("cwd: {cwd}"));
    }
    let client = client_kind.trim();
    if !client.is_empty() {
        lines.push(format!("agent_cli: {client}"));
    }
    if !memory.is_empty() {
        lines.push("session_memory:".to_string());
        for (idx, item) in memory.iter().filter(|s| !s.trim().is_empty()).enumerate() {
            lines.push(format!("{}. {}", idx + 1, normalize_ws(item)));
        }
    }
    lines.join("\n")
}

//! `UserPromptSubmit` hook stdout rendering + bootstrap guide.
//!
//! `render_hook_output` is the single template-driven renderer for
//! `hook_output.md`; the three `format_for_hook*` wrappers vary only in how
//! much session/skip-reminder context they thread through. Every activation
//! the main agent sees is a `curl` against `/skills/get/<name>` — no
//! filesystem paths, no SKILL.md bytes.

use super::router::{RouterDecision, RouterMode};

const HOOK_OUTPUT_TEMPLATE: &str = crate::core::prompts::PROMPT_HOOK_OUTPUT;

/// Format the router decision as the `UserPromptSubmit` hook stdout. Single
/// unified template (`hook_output.md`) that renders **exactly one**
/// activation flavour — `curl` against a runai server URL. Every
/// instruction the main Claude agent ever sees (activation, recall,
/// feedback) uses this HTTP shape, so there is no per-machine "do I have
/// the binary on PATH?" branch. Local users still have the `runai`
/// CLI available for scripts and manual use, but the agent-facing
/// protocol is uniformly HTTP.
///
/// `server_url` is the base of the runai server the agent should curl —
/// `http://127.0.0.1:17888` for local users (the dashboard server already
/// runs there via `ensure_running`), or the LAN URL when a teammate's
/// hook proxied through it.
///
/// `user_header` is the literal CLI arg fragment to attach to every
/// curl call. Empty means no header; otherwise it's of the form
/// ` -H 'X-Runai-User: <user>@<host>'` and gets pasted straight after
/// the URL.
pub fn format_for_hook(decision: &RouterDecision, server_url: &str, user_header: &str) -> String {
    render_hook_output(decision, "", &[], server_url, user_header, "")
}

/// Same as `format_for_hook` but with an explicit session id used in the
/// session-history recall block.
pub fn format_for_hook_with_session(
    decision: &RouterDecision,
    session_id: &str,
    server_url: &str,
    user_header: &str,
) -> String {
    render_hook_output(decision, session_id, &[], server_url, user_header, "")
}

/// Full variant: also renders this-session recall (`session_history` from
/// `router_session_recommended_skills`) and an optional skip-reminder
/// instruction injected after the activation directive. `skip_reminder` is
/// the literal text to render — pass empty string when the toggle is off.
pub fn format_for_hook_full(
    decision: &RouterDecision,
    session_id: &str,
    session_history: &[String],
    server_url: &str,
    user_header: &str,
    skip_reminder: &str,
) -> String {
    render_hook_output(
        decision,
        session_id,
        session_history,
        server_url,
        user_header,
        skip_reminder,
    )
}

fn render_hook_output(
    decision: &RouterDecision,
    session_id: &str,
    session_history: &[String],
    server_url: &str,
    user_header: &str,
    skip_reminder: &str,
) -> String {
    let skills = &decision.skills;
    if skills.is_empty() {
        return String::new();
    }

    let candidates_block: String = skills
        .iter()
        .map(|s| format!("- **{}** — {}", s.name, s.description))
        .collect::<Vec<_>>()
        .join("\n");

    let activation_directive = match (decision.mode, skills.len()) {
        (RouterMode::Exclusive, 1) => "对口就跑命令激活；不对口忽略即可。".to_string(),
        (RouterMode::Exclusive, _) => {
            "一句话让用户挑（单选或多选都行），用户挑完对每个选中的 skill 各跑一次激活 curl。"
                .to_string()
        }
        (RouterMode::Compatible, _) => {
            "互补激活：对每个候选 skill 各跑一次激活 curl，跑完立即组合执行用户原 prompt。"
                .to_string()
        }
    };

    // reasoning is mandatory per recommend_system.md. When the router LLM
    // skips it anyway, render a visible "missing" marker rather than
    // silently hiding the block — that nudge propagates back to the model
    // (in Conversation mode it sees its own past outputs) and to humans
    // reading the dashboard so the format-error is visible and fixable.
    let reasoning_block = if decision.reasoning.trim().is_empty() {
        "router 判断：(router 没给出推理 — 格式错误)\n\n".to_string()
    } else {
        format!("router 判断：{}\n\n", decision.reasoning.trim())
    };

    // Session-recall list: names the router has shown earlier in this
    // session, minus the ones currently on screen. Uses the same curl
    // activation shape as the primary block so the agent never has to
    // learn two protocols.
    let current: std::collections::HashSet<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    let history_filtered: Vec<&str> = session_history
        .iter()
        .map(|s| s.as_str())
        .filter(|n| !current.contains(n))
        .take(10)
        .collect();
    let _ = session_id; // session id is currently not embedded in the recall block; reserved for future use
    let session_history_block = if history_filtered.is_empty() {
        String::new()
    } else {
        format!(
            "\n本 session runai 已经看过的 skill（**参考池，用户随时可挑这里的任何一个，没被排除**）：{}\n如果用户当前 prompt 跟这里某个对口（包括 \"换一个 / 有其他的吗 / 找补充\" 这种 follow-up），直接跑 `curl -s -X POST '{server_url}/skills/get/<name>'{user_header}` 激活。\n",
            history_filtered.join(", ")
        )
    };

    let names = skills
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let feedback_protocol_block = format!(
        "\n反馈协议（被动）：用户明确正向（\"完美 / 好用 / 就这个\"）或负向（\"不对 / 换一个 / 不好用\"）评价时，在回复末尾跑：\n  curl -s -X POST '{server_url}/feedback'{user_header} -H 'Content-Type: application/json' -d '{{\"skill\":\"<skill-name>\",\"note\":\"<场景或原话>\"}}'\n用户没评价就不调用。\n当前推的 skill: {names}\n"
    );

    let skip_reminder_block = if skip_reminder.trim().is_empty() {
        String::new()
    } else {
        format!("\n{}\n", skip_reminder.trim())
    };

    crate::core::prompts::template_body(HOOK_OUTPUT_TEMPLATE)
        .replace("{MODE}", decision.mode.as_str())
        .replace("{REASONING_BLOCK}", &reasoning_block)
        .replace("{CANDIDATES_BLOCK}", &candidates_block)
        .replace("{ACTIVATION_DIRECTIVE}", &activation_directive)
        .replace("{SKIP_REMINDER_BLOCK}", &skip_reminder_block)
        .replace("{SERVER_URL}", server_url)
        .replace("{USER_HEADER}", user_header)
        .replace("{SESSION_HISTORY_BLOCK}", &session_history_block)
        .replace("{FEEDBACK_PROTOCOL_BLOCK}", &feedback_protocol_block)
}

/// First-run guidance shown as hook stdout when the router is not yet
/// configured. The main Claude Code agent receives this as additional
/// context and walks the user through one-shot setup.
pub fn bootstrap_guide() -> String {
    r#"# runai router not configured yet

The skill router hook is installed but **no API key / provider is configured**, so it's currently a no-op.

**Action required** — tell the user runai needs first-run setup before it can route skills, and offer to walk them through it:

1. Pick a provider (DeepSeek v4 flash is the default — cheap, fast, ~$0.0001/call). Other options: any OpenAI-compatible endpoint, Anthropic API, or `claude-cli` (uses their Max plan, no extra cost but slower).
2. Run interactive setup in their terminal:

```
runai recommend setup
```

3. After setup, runai will automatically:
   - Generate bilingual AI summaries for all 341 skills (~10 min background)
   - Auto-launch the http://127.0.0.1:17888 dashboard on every Claude Code session
   - Start routing skills on every prompt

The router is fully optional — if they don't want it, no action needed; this message won't repeat.

Do NOT proceed with their actual prompt until they decide whether to set up the router. Ask them a short yes/no question.
"#.to_string()
}

//! `UserPromptSubmit` hook stdout rendering + bootstrap guide.
//!
//! `render_hook_output` is the single template-driven renderer for
//! `hook_output.md`; the three `format_for_hook*` wrappers vary only in how
//! much session/skip-reminder context they thread through. Every activation
//! the main agent sees is `runai-client activate <name>`; support-file reads
//! inside the skill bundle go through `runai-client file <name> <relpath>` —
//! no server URL, no filesystem paths, no SKILL.md bytes. Runtime/user-home
//! paths remain local filesystem data and must not be implied to live in the
//! bundle cache.

use super::router::{RouterDecision, RouterMode};
use super::session_id::is_runai_session_id;

const HOOK_OUTPUT_TEMPLATE: &str = crate::core::prompts::PROMPT_HOOK_OUTPUT;

/// Format the router decision as the `UserPromptSubmit` hook stdout. Single
/// unified template (`hook_output.md`) that renders **exactly one**
/// activation flavour — `runai-client activate <skill>`. Every instruction
/// the main Claude agent ever sees (activation, recall, feedback) uses the
/// local companion command, so hook output never exposes server URLs, API
/// keys, raw filesystem paths, or SKILL.md bytes. `server_url` and
/// `user_header` remain in the public function signature for older callers,
/// but the renderer deliberately ignores them.
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
    // Session no-repeat suppression was removed: the hook output no longer
    // renders a "已推参考池" recall block or a skip-reminder block, so both of
    // these are accepted for caller/signature compatibility but ignored.
    _session_history: &[String],
    _server_url: &str,
    _user_header: &str,
    _skip_reminder: &str,
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

    let session_id_arg = if is_runai_session_id(session_id) {
        format!(" --session-id \"{session_id}\"")
    } else {
        String::new()
    };

    let activation_directive = match (decision.mode, skills.len()) {
        (RouterMode::Exclusive, 1) => "对口就跑命令激活；不对口忽略即可。".to_string(),
        (RouterMode::Exclusive, _) => {
            "一句话让用户挑（单选或多选都行），用户挑完对每个选中的 skill 各跑一次 runai-client activate。"
                .to_string()
        }
        (RouterMode::Compatible, _) => {
            "COMPATIBLE 互补组合：默认按候选顺序全部激活，对每个 skill 各跑一次 runai-client activate；不要把它当成工具选择题。若执行前发现缺关键输入、权限确认、高成本或不可逆操作风险，只问一个最小必要问题，再组合执行用户原 prompt。"
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

    // One-line feedback protocol (slimmed from the old multi-line block). The
    // command, verb, and the current skill names stay — only invoked when the
    // user explicitly reacts.
    let names = skills
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let feedback_protocol_block = format!(
        "\n反馈（仅用户明确正/负评价时，回复末尾跑一次）：`runai-client feedback <skill-name> --note \"<原话>\"`。当前推的 skill: {names}\n"
    );

    crate::core::prompts::template_body(HOOK_OUTPUT_TEMPLATE)
        .replace("{MODE}", decision.mode.as_str())
        .replace("{REASONING_BLOCK}", &reasoning_block)
        .replace("{CANDIDATES_BLOCK}", &candidates_block)
        .replace("{SESSION_ID_ARG}", &session_id_arg)
        .replace("{ACTIVATION_DIRECTIVE}", &activation_directive)
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

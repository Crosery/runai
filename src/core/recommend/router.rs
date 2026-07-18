//! Top-level routing: BM25 prefilter → LLM router call → telemetry persist.
//!
//! `recommend` / `recommend_for_user` are the public entry points. The flow:
//! build the candidate set (owner-filtered for v15 users), hybrid-rank with
//! BM25 + llm_score, thread optional Conversation history, call the router
//! LLM, drop hallucinated / already-routed names, render the hook output, and
//! persist a `RouterEvent` regardless of success/failure. The domain types
//! (`RouterMode` / `RouterDecision` / `RecommendedSkill` / `RouterTurn`) live
//! here too.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::time::Instant;

use crate::core::bm25;
use crate::core::db::{Database, RouterEvent};
use crate::core::manager::SkillManager;
use crate::core::paths::AppPaths;
use crate::core::resource::ResourceKind;

use super::config::{Provider, RecommendConfig, SessionMode};
use super::hook_output::format_for_hook_full;
use super::intent::{
    RecognizedIntent, ScenarioConstraint, build_intent_memory_from_prompt, build_intent_summary,
    compact_true_intent, recognize_intent,
};
use super::llm_call::{
    INTENT_MAX_TOKENS, ROUTER_MAX_TOKENS, RouterCallStats, call_anthropic,
    call_anthropic_with_system, call_claude_cli, call_claude_cli_with_system, call_openai_compat,
    call_openai_compat_with_system,
};
use super::project_context::read_project_context;
use super::prompts::intent_prompt_template;
use super::server_helpers::default_local_server_url;
use super::session_id::runai_session_id_from_native;
use super::transcript::recent_transcript_messages;

/// Safety bound for the optional `RUNAI_BM25_TOP_K=N` debug override and the
/// per-user BM25 candidate limit. Final hook output remains controlled by
/// `RecommendConfig::top_k`; this limit controls how many retrieved skills the
/// router LLM may inspect before choosing.
const BM25_TOP_K_MAX: usize = 100;
/// If the user prompt tokenizes to fewer than this many terms, skip BM25 and
/// pass the full candidate set. With the default `bm25_hybrid` mode this is
/// only triggered for **empty** queries — hybrid scoring is
/// `bm25 * 0.4 + llm_score/10 * 0.6`, so even single-token prompts where BM25
/// degenerates to "any doc containing that token" still produce a bounded
/// top-K sorted by `llm_score`, far better than dumping every candidate into
/// the router prompt.
const BM25_MIN_QUERY_TERMS: usize = 1;
/// Minimum positive-score BM25 hits to trust the prefilter. Below this the
/// query likely has zero / near-zero term overlap with the skill corpus —
/// the most common cause is cross-language search (CJK prompt against an
/// English-only skill description), where the BM25 tokenizer can't bridge.
/// In that case fall back to passing the full candidate set so the LLM can
/// do semantic matching instead. LLM rerank on 343 candidates still works
/// fine (it's the previous default); the BM25 path is a token-saving
/// optimisation, not a correctness gate.
const BM25_MIN_POSITIVE_HITS: usize = 5;

/// Reasoning stored on the decision (and surfaced in logs) when the relevance
/// cutoff leaves zero candidates: Stage-1 ran but no installed skill shares any
/// query-term overlap with the intent, so Stage-2 is skipped entirely. Kept as
/// a recognizable literal so the dashboard can distinguish "router chose
/// nothing" from "router never got to choose".
const NO_RELEVANT_CANDIDATE_REASONING: &str = "无文本相关候选：BM25 检索零命中，跳过路由 LLM";
/// Placeholder written to `router_events.llm_input` for a cutoff-skipped round.
/// No Stage-2 prompt was ever built, so storing the full candidate listing here
/// would be misleading (and there is none) — the whole point is the token save.
const STAGE2_SKIPPED_LLM_INPUT: &str = "[stage-2 skipped: no BM25-relevant candidate]";

// All router prompts and hook output templates live in src/core/prompts/ and
// are exposed as `PROMPT_<NAME>` consts via the centralised registry
// (`crate::core::prompts`). Edit the .md files to retune wording.
const USER_MSG_TEMPLATE: &str = crate::core::prompts::PROMPT_RECOMMEND_USER;
const HISTORY_PREFIX_TEMPLATE: &str = crate::core::prompts::PROMPT_RECOMMEND_HISTORY_PREFIX;
const CWD_PREFIX_TEMPLATE: &str = crate::core::prompts::PROMPT_RECOMMEND_CWD_PREFIX;

/// Upper bound (in chars) on the raw user prompt copied into the Precise
/// Stage-1 message. Fast skips Stage-1; both modes separately carry a much
/// smaller bounded task anchor into the router. The cap protects against
/// tens-of-thousands-char pastes while preserving source evidence; telemetry
/// (`RouterEvent.user_prompt`) always stores the full text. Over the cap we keep
/// a head AND a tail (see `truncate_prompt_for_llm`).
const LLM_PROMPT_CHAR_CAP: usize = 4000;
/// Chars kept from the START of an over-cap prompt. The real request almost
/// always lives at the END (users paste long context, then ask), so the tail
/// gets the larger share (~2/3) and the head a smaller ~1/3 — enough to keep
/// the leading framing without swallowing the actual ask. Head-only truncation
/// (the old behavior) cut the real intent out entirely, which made Stage-1
/// guess from the pasted context instead of the actual request.
const LLM_PROMPT_HEAD_CHARS: usize = LLM_PROMPT_CHAR_CAP / 3;
/// Inserted between the kept head and tail so the model (and a human reading
/// telemetry) can see the middle was elided rather than reading a false splice.
const LLM_PROMPT_TRUNCATION_MARKER: &str = "\n…[中段已截断]…\n";

/// Truncate an over-cap `prompt` for LLM input by keeping a head window AND a
/// tail window with a visible middle-elision marker between them, so the real
/// request (which users almost always put LAST, after pasted context) survives.
/// The kept text totals about `LLM_PROMPT_CHAR_CAP` chars — `LLM_PROMPT_HEAD_CHARS`
/// from the front, the rest from the back — plus the short marker. Char-boundary
/// safe (iterates over `chars`, never byte indices). Used ONLY by the Stage-1
/// intent user message — Stage-2 no longer embeds the raw prompt (it routes off
/// the condensed intent summary), so this is the single consumer of the cap.
pub(super) fn truncate_prompt_for_llm(prompt: &str) -> String {
    let total = prompt.chars().count();
    if total <= LLM_PROMPT_CHAR_CAP {
        return prompt.to_string();
    }
    let head_len = LLM_PROMPT_HEAD_CHARS.min(LLM_PROMPT_CHAR_CAP);
    let tail_len = LLM_PROMPT_CHAR_CAP - head_len;
    let head: String = prompt.chars().take(head_len).collect();
    let tail: String = prompt.chars().skip(total - tail_len).collect();
    format!("{head}{LLM_PROMPT_TRUNCATION_MARKER}{tail}")
}

/// Max transcript turns threaded into the Stage-2 history block. Kept small on
/// purpose — the block only needs to reveal whether the current prompt is a
/// reply to the previous recommendation, not replay the whole conversation.
const HISTORY_TURN_LIMIT: usize = 4;
const CWD_CHAR_CAP: usize = 256;
const CLIENT_KIND_CHAR_CAP: usize = 64;
const CONVERSATION_TURN_CHAR_CAP: usize = 1200;
const CONVERSATION_TOTAL_CHAR_CAP: usize = 6000;

/// Mode tag returned by the router on the first line of its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RouterMode {
    /// Skills in this set can be loaded together (e.g. github + writing-skills).
    Compatible,
    /// Skills are mutually exclusive — user must pick one (e.g. multiple image gen providers).
    #[default]
    Exclusive,
}

impl RouterMode {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            RouterMode::Compatible => "compatible",
            RouterMode::Exclusive => "exclusive",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CandidateCatalogEntry {
    pub(super) id: String,
    pub(super) name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EmptyReason {
    RetrievalZero,
    GateZero,
    ModelEmpty,
    ParseEmpty,
    FilteredEmpty,
    TransportError,
    None,
}

impl EmptyReason {
    fn as_str(self) -> &'static str {
        match self {
            EmptyReason::RetrievalZero => "retrieval_zero",
            EmptyReason::GateZero => "gate_zero",
            EmptyReason::ModelEmpty => "model_empty",
            EmptyReason::ParseEmpty => "parse_empty",
            EmptyReason::FilteredEmpty => "filtered_empty",
            EmptyReason::TransportError => "transport_error",
            EmptyReason::None => "none",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ParsedRouterResponse {
    pub(super) mode: RouterMode,
    pub(super) reasoning: String,
    pub(super) parsed_values: Vec<String>,
    pub(super) filtered_names: Vec<String>,
    pub(super) recovery: bool,
    pub(super) empty_reason: EmptyReason,
}

/// A single prior router round-trip — the exact `user_msg` we sent and the
/// exact `assistant` text the LLM produced. Used to rebuild a chat-history
/// messages array in `SessionMode::Conversation`.
#[derive(Debug, Clone)]
pub struct RouterTurn {
    pub user: String,
    pub assistant: String,
}

/// A single recommended skill. The router never sends the SKILL.md body,
/// server URL, API key, or filesystem path to the main agent — activation
/// flows through `runai-client activate <name>`, so all we ship is the
/// human-readable name and a short description for the candidate list.
#[derive(Debug, Clone)]
pub struct RecommendedSkill {
    pub name: String,
    pub description: String,
}

/// Full router output: the mode tag, a short reasoning sentence the router
/// LLM produced ("why this set"), and the ranked skill list. `reasoning`
/// can be empty when the LLM omitted the line — the renderer just hides
/// the block in that case.
#[derive(Debug, Clone, Default)]
pub struct RouterDecision {
    pub mode: RouterMode,
    pub reasoning: String,
    pub skills: Vec<RecommendedSkill>,
}

/// Harness system messages are host-injected envelopes (task queue pings,
/// inter-agent chatter, local slash-command echoes), never a human asking for
/// a skill. Routing them wastes two LLM waves that almost always end in an
/// empty push, and — worse — writing a `router_events` row for them pollutes
/// the adoption / precision funnel that feeds candidate ranking. Gate them out
/// before any LLM call or DB write.
///
/// Match is prefix-only on the trimmed prompt: the four envelopes always lead
/// the message. A prefix appearing mid-body is a real prompt quoting one, and
/// must still route.
pub(super) fn is_harness_message(prompt: &str) -> bool {
    const HARNESS_PREFIXES: [&str; 5] = [
        "<task-notification>",
        "<agent-message",
        "<teammate-message",
        "<local-command-",
        // The harness precedes teammate mail with a plain-text preamble, so
        // the raw tag prefix never matches that traffic shape (observed live:
        // all-day inter-agent chatter burned two LLM calls per message and
        // rate-limited real prompts into the hook's 30s timeout).
        "Another Claude session sent a message",
    ];
    let trimmed = prompt.trim();
    HARNESS_PREFIXES.iter().any(|p| trimmed.starts_with(p))
}

/// Hybrid candidate score used to pick which BM25 survivors reach the Stage-2
/// router LLM. Default blends three signals; `feedback_disabled` reverts to the
/// original two-signal weights (`RUNAI_FEEDBACK_DISABLED=1` escape hatch).
///
/// - default:  `bm25 * 0.35 + llm/10 * 0.45 + feedback_factor * 0.20`
/// - disabled: `bm25 * 0.40 + llm/10 * 0.60`
///
/// `bm25_norm` and `llm_val` are already normalised to `0..1`;
/// `feedback_factor` is the `0..1` scalar from `skill_metrics::feedback_factor`
/// (neutral 0.5 when a skill has no adoption / rating history).
pub(super) fn hybrid_score(
    bm25_norm: f64,
    llm_val: f64,
    feedback_factor: f64,
    feedback_disabled: bool,
) -> f64 {
    if feedback_disabled {
        bm25_norm * 0.4 + llm_val * 0.6
    } else {
        bm25_norm * 0.35 + llm_val * 0.45 + feedback_factor * 0.20
    }
}

/// The ` [adopt:NN%] [fb:+P/-N]` marker suffix appended to a candidate line so
/// the Stage-2 router sees real behavioural signal, not just the enrich-pass
/// `[llm:N]` guess. `[adopt:]` only renders once a skill has been chosen in at
/// least 3 distinct sessions (below that the ratio is too noisy to trust);
/// `[fb:]` renders whenever any explicit thumbs up/down exists. Returns an
/// empty string when neither threshold is met.
pub(super) fn feedback_markers(
    chosen_sessions: i64,
    adopted_sessions: i64,
    pos: i64,
    neg: i64,
) -> String {
    let mut out = String::new();
    if chosen_sessions >= 3 {
        let pct =
            ((adopted_sessions.max(0) as f64 / chosen_sessions as f64) * 100.0).round() as i64;
        out.push_str(&format!(" [adopt:{pct}%]"));
    }
    if pos + neg > 0 {
        out.push_str(&format!(" [fb:+{pos}/-{neg}]"));
    }
    out
}

pub(super) struct CandidateRelevanceInput<'a> {
    pub(super) name: &'a str,
    pub(super) search_doc: &'a str,
    pub(super) router_card: &'a str,
    pub(super) description: &'a str,
    pub(super) groups: &'a [&'a str],
}

fn lower_compact(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn has_any(text: &str, needles: &[&str]) -> bool {
    let text = lower_compact(text);
    needles.iter().any(|needle| text.contains(needle))
}

fn positive_retrieval_text(text: &str) -> String {
    let lower = text.to_lowercase();
    if let Some(idx) = lower.find("not-for") {
        return text[..idx].trim().to_string();
    }
    if let Some(idx) = lower.find("not for") {
        return text[..idx].trim().to_string();
    }
    text.to_string()
}

fn summary_field(text: &str, label: &str) -> String {
    text.lines()
        .find_map(|line| {
            let (key, value) = line.split_once([':', '：'])?;
            (key.trim().eq_ignore_ascii_case(label)).then(|| value.trim().to_string())
        })
        .unwrap_or_default()
}

fn compact_field(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

pub(super) fn structured_candidate_card(summary: &str, fallback: &str) -> String {
    let source = if summary.trim().is_empty() {
        fallback
    } else {
        summary
    };
    let task = compact_field(&summary_field(source, "task"), 120);
    let triggers = compact_field(&summary_field(source, "triggers"), 140);
    let inputs = compact_field(&summary_field(source, "inputs"), 100);
    let outputs = compact_field(&summary_field(source, "outputs"), 100);
    let not_for = compact_field(&summary_field(source, "not-for"), 140);
    if [
        task.as_str(),
        triggers.as_str(),
        inputs.as_str(),
        outputs.as_str(),
        not_for.as_str(),
    ]
    .iter()
    .all(|field| field.is_empty())
    {
        return format!(
            "task: {} | triggers: | inputs: | outputs: | not-for:",
            compact_field(source, 220)
        );
    }
    format!(
        "task: {task} | triggers: {triggers} | inputs: {inputs} | outputs: {outputs} | not-for: {not_for}"
    )
}

pub(super) fn candidate_allowed_by_intent(
    intent: &RecognizedIntent,
    candidate: &CandidateRelevanceInput<'_>,
) -> bool {
    let groups = candidate.groups.join(" ");
    let positive_without_groups = format!(
        "{} {} {} {}",
        candidate.name,
        positive_retrieval_text(candidate.search_doc),
        positive_retrieval_text(candidate.router_card),
        candidate.description,
    );
    let positive = format!("{} {}", positive_without_groups, groups);

    if intent.has(ScenarioConstraint::AndroidEmulatorDebug) {
        const VEHICLE_STRONG: &[&str] = &[
            "ktv",
            "ktvlite",
            "webview",
            "h5",
            "真车",
            "car-device",
            "car-debug",
            "调试面板",
            "车机 webview",
            "车机 h5",
            "车机/模拟器完整调试",
        ];
        const ANDROID_BASE: &[&str] = &[
            "android",
            "安卓",
            "adb",
            "logcat",
            "emulator",
            "avd",
            "模拟器",
        ];
        if has_any(&positive_without_groups, VEHICLE_STRONG) {
            return false;
        }
        return has_any(&positive, ANDROID_BASE);
    }

    if intent.has(ScenarioConstraint::ImageReferenceRegeneration) {
        const IMAGE_GENERATION_TERMS: &[&str] = &[
            "image",
            "图片",
            "图像",
            "生图",
            "画图",
            "绘图",
            "插画",
            "改图",
            "编辑图片",
            "生成图片",
            "参考图",
            "reference image",
            "img2img",
            "image-to-image",
            "图生图",
            "generate image",
            "edit image",
            "illustration",
        ];
        return has_any(&positive, IMAGE_GENERATION_TERMS);
    }

    if intent.has(ScenarioConstraint::PromptRouterAudit) {
        const ROUTER_TERMS: &[&str] = &[
            "router",
            "recommend",
            "推荐模型",
            "bm25",
            "not-for",
            "prompt",
            "提示词",
            "候选",
            "误召回",
        ];
        const ARCH_ONLY: &[&str] = &["仓库结构", "架构审计", "agents.md", "目录结构", "模块边界"];
        if has_any(&positive, ARCH_ONLY) && !has_any(&positive, ROUTER_TERMS) {
            return false;
        }
    }

    true
}

pub(super) struct RouterUserMessageParts<'a> {
    pub(super) task_anchor: &'a str,
    pub(super) cwd_block: &'a str,
    pub(super) project_context_block: &'a str,
    pub(super) history_block: &'a str,
    pub(super) intent_summary: &'a str,
    pub(super) candidate_listing: &'a str,
    pub(super) bm25_candidate_limit: usize,
}

pub(super) fn build_router_user_message(parts: RouterUserMessageParts<'_>) -> String {
    // The router sees the bounded original task anchor in parallel with the
    // deterministic/Precise expansion. This preserves source evidence without
    // resending the unbounded raw payload. Optional history/project blocks are
    // Precise-only and bounded before this builder.
    crate::core::prompts::template_body(USER_MSG_TEMPLATE)
        .replace("{TASK_ANCHOR}", parts.task_anchor)
        .replace("{HISTORY_BLOCK}", parts.history_block)
        .replace("{CWD_BLOCK}", parts.cwd_block)
        .replace("{PROJECT_CONTEXT_BLOCK}", parts.project_context_block)
        .replace("{INTENT_SUMMARY}", parts.intent_summary)
        .replace("{CANDIDATE_LISTING}", parts.candidate_listing)
        .replace(
            "{BM25_CANDIDATE_LIMIT}",
            &parts.bm25_candidate_limit.to_string(),
        )
}

/// Stage-1 intent user message. Carries ONLY dynamic content, each field
/// exactly once: cwd (one line), agent_cli (one line), session_memory (one
/// block), then the current prompt (truncated). All static instructions live
/// in the fixed `recommend_intent.md` system prompt — nothing here duplicates
/// them, so the previous double injection (whole template as system AND as a
/// filled user message, plus the deterministic fallback re-embedding
/// memory/cwd/prompt a second time) is gone. The deterministic fallback is NOT
/// sent to the model; it stays a code-side safety net used only when the
/// Stage-1 call fails.
fn truncate_context_field(value: &str, cap: usize) -> String {
    let trimmed = value.trim();
    let total = trimmed.chars().count();
    if total <= cap {
        return trimmed.to_string();
    }
    let marker = "…[截断]…";
    let marker_len = marker.chars().count();
    let head_len = (cap.saturating_sub(marker_len)) / 2;
    let tail_len = cap.saturating_sub(marker_len + head_len);
    let head: String = trimmed.chars().take(head_len).collect();
    let tail: String = trimmed.chars().skip(total - tail_len).collect();
    format!("{head}{marker}{tail}")
}

fn bound_conversation_history(turns: Vec<(String, String)>) -> Vec<RouterTurn> {
    let mut kept_rev = Vec::new();
    let mut total = 0_usize;
    for (user, assistant) in turns.into_iter().rev().take(HISTORY_TURN_LIMIT) {
        let user = truncate_context_field(&user, CONVERSATION_TURN_CHAR_CAP);
        let assistant = truncate_context_field(&assistant, CONVERSATION_TURN_CHAR_CAP);
        let cost = user.chars().count() + assistant.chars().count();
        if total + cost > CONVERSATION_TOTAL_CHAR_CAP {
            continue;
        }
        total += cost;
        kept_rev.push(RouterTurn { user, assistant });
    }
    kept_rev.reverse();
    kept_rev
}

fn build_intent_user_message(
    user_prompt: &str,
    cwd: Option<&str>,
    client_kind: &str,
    memory: &[String],
) -> String {
    let mut out = String::new();
    if let Some(c) = cwd.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str(&format!("cwd: {c}\n"));
    }
    let client = client_kind.trim();
    if !client.is_empty() {
        out.push_str(&format!("agent_cli: {client}\n"));
    }
    let memory_items: Vec<&String> = memory.iter().filter(|m| !m.trim().is_empty()).collect();
    if !memory_items.is_empty() {
        out.push_str("session_memory:\n");
        for (idx, m) in memory_items.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", idx + 1, m.trim()));
        }
    }
    out.push_str("\n当前用户输入：\n");
    out.push_str(&truncate_prompt_for_llm(user_prompt));
    out
}

fn clean_intent_model_output(raw: &str, fallback: &str) -> String {
    let mut text = raw.trim().to_string();
    if text.starts_with("```") {
        let lines = text.lines().collect::<Vec<_>>();
        if lines.len() >= 2 {
            let body = lines[1..]
                .iter()
                .copied()
                .take_while(|l| !l.trim_start().starts_with("```"))
                .collect::<Vec<_>>()
                .join("\n");
            text = body.trim().to_string();
        }
    }
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let upper = line.to_ascii_uppercase();
            upper != "EXCLUSIVE" && upper != "COMPATIBLE"
        })
        .take(8)
        .collect::<Vec<_>>();
    let mut out = lines.join("\n");
    if !out.contains("intent:") && !out.contains("intent：") {
        out = format!("intent: {}", out.trim());
    }
    if out.trim() == "intent:" || out.trim().is_empty() {
        out = fallback.trim().to_string();
    }
    out.chars().take(2000).collect::<String>()
}

fn provider_label(cfg: &RecommendConfig) -> String {
    match cfg.provider {
        Provider::OpenaiCompat => "openai-compat".into(),
        Provider::Anthropic => "anthropic".into(),
        Provider::ClaudeCli => "claude-cli".into(),
    }
}

fn add_stats(a: &RouterCallStats, b: &RouterCallStats) -> RouterCallStats {
    RouterCallStats {
        prompt_tokens: a.prompt_tokens + b.prompt_tokens,
        completion_tokens: a.completion_tokens + b.completion_tokens,
        reasoning_tokens: a.reasoning_tokens + b.reasoning_tokens,
        total_tokens: a.total_tokens + b.total_tokens,
        cache_hit_tokens: a.cache_hit_tokens + b.cache_hit_tokens,
        cache_miss_tokens: a.cache_miss_tokens + b.cache_miss_tokens,
    }
}

fn call_intent_recognition(
    cfg: &RecommendConfig,
    api_key: &str,
    user_msg: &str,
) -> Result<(String, RouterCallStats)> {
    let history: &[RouterTurn] = &[];
    let system = intent_prompt_template();
    let max_tokens = Some(INTENT_MAX_TOKENS);
    match cfg.provider {
        Provider::OpenaiCompat => {
            call_openai_compat_with_system(cfg, api_key, system, user_msg, history, max_tokens)
        }
        Provider::Anthropic => {
            call_anthropic_with_system(cfg, api_key, system, user_msg, history, max_tokens)
        }
        Provider::ClaudeCli => call_claude_cli_with_system(cfg, system, user_msg),
    }
}

/// Top-level entry: run the router and return the list of recommended skills.
/// Returns `Ok(Vec::new())` when nothing matches, when disabled, or when prompt
/// is too short.
///
/// `transcript_path`, when supplied, is consulted only in Precise mode and is
/// rendered through bounded recent-message helpers. Fast never reads it.
pub fn recommend(
    mgr: &SkillManager,
    user_prompt: &str,
    transcript_path: Option<&Path>,
    session_id: Option<&str>,
    cwd: Option<&str>,
) -> Result<RouterDecision> {
    recommend_for_user(mgr, user_prompt, transcript_path, session_id, cwd, None)
}

/// Multi-user variant: filters the candidate set against the user's
/// per-user library when `user_id` is `Some`. When `None`, behaves like
/// the legacy single-user path (no filtering).
///
/// Filter rules (when user_id is set):
/// - allow_public_recommend = false (default): candidate set =
///   resources owned by this user (owner_user_id = uid) ∪ public skills
///   in user_skill_library
/// - allow_public_recommend = true: candidate set = all public skills
///   ∪ user-owned skills (= every skill the user could possibly see)
pub fn recommend_for_user(
    mgr: &SkillManager,
    user_prompt: &str,
    transcript_path: Option<&Path>,
    session_id: Option<&str>,
    cwd: Option<&str>,
    user_id: Option<&str>,
) -> Result<RouterDecision> {
    recommend_for_user_with_client(
        mgr,
        user_prompt,
        transcript_path,
        session_id,
        cwd,
        user_id,
        Some("claude"),
    )
}

pub(crate) fn resolve_user_prefs(
    db: &Database,
    user_id: Option<&str>,
) -> Result<crate::core::prefs::UserPrefs> {
    let Some(uid) = user_id else {
        return Ok(crate::core::prefs::UserPrefs::default());
    };
    if uid == "owner" {
        let stored = db.app_setting("owner_prefs")?.unwrap_or_default();
        return Ok(crate::core::prefs::UserPrefs::from_json_str(&stored));
    }
    Ok(db
        .find_user_by_id(uid)?
        .map(|user| crate::core::prefs::UserPrefs::from_json_str(&user.prefs_json))
        .unwrap_or_default())
}

pub fn recommend_for_user_with_client(
    mgr: &SkillManager,
    user_prompt: &str,
    transcript_path: Option<&Path>,
    session_id: Option<&str>,
    cwd: Option<&str>,
    user_id: Option<&str>,
    client_kind: Option<&str>,
) -> Result<RouterDecision> {
    let session_id_owned = session_id.and_then(|sid| runai_session_id_from_native(user_id, sid));
    let session_id = session_id_owned.as_deref();
    let mut cfg = RecommendConfig::load(mgr.paths())?;
    // v15 multi-user + PLANNING §1.3 prompt injection toggles. When an
    // authenticated user is on the request, their per-user UserPrefs
    // override the matching fields on the global cfg AND drive the
    // per-request prompt-injection gating below. When `user_id` is None
    // (unauthenticated / legacy CLI hook) the prefs are the default value
    // = every prompt enabled, ensuring the unauthenticated path NEVER
    // reads another account's prefs.
    let user_prefs = resolve_user_prefs(mgr.db(), user_id)?;
    if user_id.is_some() {
        cfg.enabled = cfg.enabled && user_prefs.recommend_enabled;
        cfg.read_claude_md = user_prefs.read_claude_md;
        cfg.skip_reminder_enabled = user_prefs.skip_reminder_enabled;
        if !user_prefs.skip_reminder_template.is_empty() {
            cfg.skip_reminder_template = user_prefs.skip_reminder_template.clone();
        }
    }
    if !cfg.enabled {
        return Ok(RouterDecision {
            mode: RouterMode::Exclusive,
            reasoning: String::new(),
            skills: Vec::new(),
        });
    }
    if user_prompt.trim().chars().count() < cfg.min_prompt_len {
        return Ok(RouterDecision {
            mode: RouterMode::Exclusive,
            reasoning: String::new(),
            skills: Vec::new(),
        });
    }

    // Harness system messages (task-notification / agent-message /
    // teammate-message / local-command envelopes) are never a human asking
    // for a skill. Short-circuit BEFORE the intent/router LLM calls and
    // BEFORE any router_events write — silent like the disabled path, so we
    // neither burn two LLM waves nor pollute the adoption funnel with rows
    // that could never be adopted.
    if is_harness_message(user_prompt) {
        return Ok(RouterDecision {
            mode: RouterMode::Exclusive,
            reasoning: String::new(),
            skills: Vec::new(),
        });
    }

    let bounded_cwd = cwd
        .map(|value| truncate_context_field(value, CWD_CHAR_CAP))
        .filter(|value| !value.is_empty());
    let bounded_client_kind = truncate_context_field(
        client_kind
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("claude"),
        CLIENT_KIND_CHAR_CAP,
    );
    let client_kind = bounded_client_kind.as_str();

    // PLANNING §1.3: per-user injection flags strip optional blocks before
    // prompt construction. The intent layer shares the same cwd switch so a
    // disabled cwd block cannot leak through `{INTENT_SUMMARY}`.
    let inject_history = user_prefs.prompt_injection_enabled("recommend_history_prefix");
    let inject_cwd = user_prefs.prompt_injection_enabled("recommend_cwd_prefix");
    let inject_project_context = user_prefs.prompt_injection_enabled("recommend_project_context");
    let cwd_for_intent = if inject_cwd {
        bounded_cwd.as_deref()
    } else {
        None
    };

    // ClaudeCli reuses the user's Claude Code session — no API key needed.
    let api_key = if cfg.provider == Provider::ClaudeCli {
        String::new()
    } else {
        cfg.effective_api_key()
            .context("recommend api_key not configured: run `runai recommend setup` or set RUNAI_RECOMMEND_API_KEY")?
    };

    let precise = user_prefs.routing_mode == crate::core::prefs::RoutingMode::Precise;
    let intent_memory_limit = if precise && user_prefs.intent_memory_enabled {
        user_prefs.intent_memory_limit
    } else {
        0
    };
    let mut intent_memory = Vec::new();
    if let Some(sid) = session_id
        && !sid.is_empty()
        && intent_memory_limit > 0
    {
        intent_memory = mgr
            .db()
            .router_intent_memory(sid, user_id, client_kind, intent_memory_limit)
            .unwrap_or_default()
            .into_iter()
            .map(|i| i.memory)
            .collect();
    }

    let task_anchor = compact_true_intent(user_prompt);
    let deterministic_intent =
        build_intent_summary(user_prompt, cwd_for_intent, client_kind, &intent_memory);
    let (
        intent_summary,
        intent_llm_input,
        intent_llm_raw_output,
        intent_status,
        intent_error_msg,
        intent_stats,
        intent_call_attempts,
    ) = match user_prefs.routing_mode {
        crate::core::prefs::RoutingMode::Fast => (
            deterministic_intent
                .lines()
                .filter(|line| !line.trim_start().starts_with("intent:"))
                .collect::<Vec<_>>()
                .join("\n"),
            String::new(),
            String::new(),
            "skipped-fast".to_string(),
            None,
            RouterCallStats::default(),
            0_i64,
        ),
        crate::core::prefs::RoutingMode::Precise => {
            let input =
                build_intent_user_message(user_prompt, cwd_for_intent, client_kind, &intent_memory);
            match call_intent_recognition(&cfg, &api_key, &input) {
                Ok((raw, stats)) => (
                    clean_intent_model_output(&raw, &deterministic_intent),
                    input,
                    raw,
                    "ok".to_string(),
                    None,
                    stats,
                    1_i64,
                ),
                Err(e) => (
                    deterministic_intent.clone(),
                    input,
                    String::new(),
                    "fallback".to_string(),
                    Some(e.to_string()),
                    RouterCallStats::default(),
                    1_i64,
                ),
            }
        }
    };
    let positive_expansion = intent_summary
        .lines()
        .filter(|line| {
            !line
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("exclude_terms")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let bm25_input_query = format!("{task_anchor}\n{positive_expansion}");
    let recognized_intent = recognize_intent(
        user_prompt,
        &intent_memory,
        cwd_for_intent,
        Some(client_kind),
    );
    let current_intent_memory = build_intent_memory_from_prompt(&intent_summary);
    if let Some(sid) = session_id
        && !sid.is_empty()
        && intent_memory_limit > 0
    {
        let _ = mgr.db().append_router_intent_memory(
            sid,
            user_id,
            client_kind,
            &current_intent_memory,
            intent_memory_limit,
        );
    }

    // Session no-repeat suppression was removed (每轮独立全质量推荐): the
    // router no longer fetches this session's prior recommendations to inject
    // an ALREADY_ROUTED block or a hook "已推参考池" reminder. Same skill can
    // be re-recommended on consecutive turns. `router_session_recommended_skills`
    // stays available as pure telemetry but never enters a prompt.
    let db = mgr.db();
    let mut all_candidates: Vec<_> = match user_id {
        Some(uid) => {
            if user_prefs.allow_public_recommend || uid == "owner" {
                db.list_resources_for_user(Some(ResourceKind::Skill), Some(uid))?
            } else {
                let lib: std::collections::BTreeSet<String> = db
                    .library_list(uid)
                    .unwrap_or_default()
                    .into_iter()
                    .collect();
                db.list_resources_for_user(Some(ResourceKind::Skill), Some(uid))?
                    .into_iter()
                    .filter(|r| r.owner_user_id.as_deref() == Some(uid) || lib.contains(&r.name))
                    .collect()
            }
        }
        None => mgr
            .list_resources(Some(ResourceKind::Skill), None)?
            .into_iter()
            .filter(|r| r.owner_user_id.is_none())
            .collect(),
    };
    all_candidates.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| b.owner_user_id.is_some().cmp(&a.owner_user_id.is_some()))
            .then_with(|| b.installed_at.cmp(&a.installed_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut seen_candidate_names = std::collections::HashSet::new();
    all_candidates.retain(|r| seen_candidate_names.insert(r.name.clone()));

    if all_candidates.is_empty() {
        return Ok(RouterDecision {
            mode: RouterMode::Exclusive,
            reasoning: String::new(),
            skills: Vec::new(),
        });
    }
    let all_candidates_count = all_candidates.len();

    // BM25 prefilter. Without it the LLM sees all ~343 candidates and gets
    // noise-flooded — empirically this is what tanks chosen-rate to ~46%
    // even when a relevant skill exists. After prefilter the LLM sees a
    // focused candidate set with strong term-overlap with the current
    // intent summary. `bm25_candidate_limit` controls how many candidates the
    // router LLM sees; the final recommendation quantity is decided by the
    // router LLM under the system prompt's 最小充分集合 rule (no numeric cap in
    // the prompt — `cfg.top_k` stays a config-side ceiling only).
    let bm25_candidate_limit: usize = std::env::var("RUNAI_BM25_TOP_K")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(user_prefs.bm25_candidate_limit)
        .clamp(1, BM25_TOP_K_MAX);

    let bm25_disabled = std::env::var("RUNAI_BM25_DISABLED").is_ok();
    // Default: hybrid scoring, then configured top-K → LLM. Empirically
    // beats pure BM25 prefilter on prompts where descriptions are weak /
    // cross-lingual.
    //
    // Escape hatches:
    //   RUNAI_BM25_PURE=1     → pure BM25 score ranking (no LLM/user weight)
    //   RUNAI_BM25_AS_SIGNAL=1 → full candidate set, BM25 score as a tag
    //   RUNAI_BM25_DISABLED=1 → skip prefilter entirely (full set, no tag)
    let bm25_pure = std::env::var("RUNAI_BM25_PURE").is_ok();
    let bm25_as_signal = std::env::var("RUNAI_BM25_AS_SIGNAL").is_ok();
    let bm25_hybrid = !bm25_pure && !bm25_as_signal && !bm25_disabled;
    // BM25 ranks candidates. Positive retrieval query always keeps the
    // original task anchor in parallel with deterministic/Stage-1 expansion.
    // Exclusion fields stay out of positive BM25 text and remain available to
    // deterministic gates/router cards. Quality/feedback may re-rank positive
    // retrieval evidence, but never resurrect a zero-evidence candidate.

    let q_terms = bm25::tokenize(&bm25_input_query);
    let bm25_fallback_reason: &'static str;

    let indices = mgr
        .db()
        .skill_ai_index_all_by_resource_key()
        .unwrap_or_default();
    let groups_by_resource = mgr.db().groups_for_all_resources().unwrap_or_default();
    let groups_of = |resource_id: &str| -> Vec<String> {
        groups_by_resource
            .get(resource_id)
            .cloned()
            .unwrap_or_default()
    };

    // Aggregated behavioural feedback, fetched ONCE for the whole candidate
    // set (never inside a per-candidate loop). Two signals:
    //   - `router_stats`: per-skill funnel counts (candidate/chosen events,
    //     chosen/adopted sessions) over the last 90 days.
    //   - `feedback_counts`: explicit thumbs up/down per skill.
    // They drive both the hybrid re-rank weight (deliverable 1) and the
    // `[adopt:] [fb:]` candidate-line markers (deliverable 2).
    //
    // `RUNAI_FEEDBACK_DISABLED=1` reverts to the legacy 0.4/0.6 weights and
    // suppresses the markers entirely — a full "old behaviour" escape hatch,
    // named to match the existing `RUNAI_BM25_*` family.
    //
    // A DB read failure must never break recommend: `unwrap_or_default()`
    // yields empty maps, so every skill degrades to `feedback_factor(0,0,0,0)`
    // = neutral 0.5 and no markers, and routing proceeds normally.
    let feedback_disabled = std::env::var("RUNAI_FEEDBACK_DISABLED").is_ok();
    let (router_stats, feedback_counts) = if feedback_disabled {
        (
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        )
    } else {
        let since_ts = chrono::Utc::now().timestamp() - 90 * 86_400;
        (
            mgr.db().skill_router_stats(since_ts).unwrap_or_default(),
            mgr.db().skill_feedback_counts_all().unwrap_or_default(),
        )
    };
    let feedback_factor_of = |name: &str| -> f64 {
        let stats = router_stats.get(name).copied().unwrap_or_default();
        let (pos, neg) = feedback_counts.get(name).copied().unwrap_or((0, 0));
        crate::core::skill_metrics::feedback_factor(
            stats.adopted_sessions,
            stats.chosen_sessions,
            pos,
            neg,
        )
    };

    all_candidates.retain(|r| {
        let index_key = Database::skill_ai_index_key_for_resource(r);
        let search_doc = indices
            .get(&index_key)
            .map(|row| row.search_doc.as_str())
            .unwrap_or("");
        let router_card = indices
            .get(&index_key)
            .map(|row| row.router_card.as_str())
            .unwrap_or("");
        let groups_vec = groups_of(&r.id);
        let group_refs: Vec<&str> = groups_vec.iter().map(String::as_str).collect();
        candidate_allowed_by_intent(
            &recognized_intent,
            &CandidateRelevanceInput {
                name: &r.name,
                search_doc,
                router_card,
                description: &r.description,
                groups: &group_refs,
            },
        )
    });

    if all_candidates.is_empty() {
        let decision = RouterDecision {
            mode: RouterMode::Exclusive,
            reasoning: String::new(),
            skills: Vec::new(),
        };
        let ev = RouterEvent {
            ts: chrono::Utc::now().timestamp(),
            provider: provider_label(&cfg),
            model: cfg.model.clone(),
            prompt_tokens: intent_stats.prompt_tokens,
            completion_tokens: intent_stats.completion_tokens,
            reasoning_tokens: intent_stats.reasoning_tokens,
            total_tokens: intent_stats.total_tokens,
            cache_hit_tokens: intent_stats.cache_hit_tokens,
            cache_miss_tokens: intent_stats.cache_miss_tokens,
            chosen_skills_json: "[]".into(),
            candidate_count: all_candidates_count as i64,
            session_id: session_id.unwrap_or("").into(),
            mode: RouterMode::Exclusive.as_str().into(),
            user_prompt: user_prompt.into(),
            cwd: cwd.unwrap_or("").into(),
            intent_llm_input,
            intent_llm_raw_output,
            intent_llm_output: intent_summary,
            intent_status,
            intent_error_msg,
            user_id: user_id.map(str::to_string),
            routing_mode: user_prefs.routing_mode.as_str().into(),
            empty_reason: EmptyReason::GateZero.as_str().into(),
            retrieval_query: bm25_input_query,
            llm_call_count: intent_call_attempts,
            ..RouterEvent::default()
        };
        let _ = mgr.db().insert_router_event(&ev);
        return Ok(decision);
    }

    // skill name → normalised BM25 score (0..1) for the [bm25:0.XX] tag.
    let mut bm25_scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();

    let candidates: Vec<_> = if bm25_disabled {
        bm25_fallback_reason = "disabled-by-env";
        all_candidates
    } else if q_terms.len() < BM25_MIN_QUERY_TERMS {
        bm25_fallback_reason = "query-too-short";
        all_candidates
    } else {
        // BM25 doc text: prefer the structured search_doc over the raw
        // description. search_doc packs name + summary + trigger/not-for
        // tokens and is built specifically for retrieval.
        let docs: Vec<String> = all_candidates
            .iter()
            .map(|r| {
                let index_key = Database::skill_ai_index_key_for_resource(r);
                let body = if let Some(index) = indices.get(&index_key) {
                    if index.search_doc.is_empty() {
                        r.description.clone()
                    } else {
                        positive_retrieval_text(&index.search_doc)
                    }
                } else {
                    r.description.clone()
                };
                let groups = groups_of(&r.id).join(" ");
                if groups.is_empty() {
                    format!("{} {}", r.name, body)
                } else {
                    format!("{} {} {}", r.name, body, groups)
                }
            })
            .collect();
        let ranked = bm25::rank(&bm25_input_query, &docs);
        // Build normalised score map for the [bm25:0.XX] tag.
        let max_score = ranked.iter().map(|(_, s)| *s).fold(0.0_f64, f64::max);
        if max_score > 0.0 {
            for (i, s) in &ranked {
                if *s > 0.0
                    && let Some(c) = all_candidates.get(*i)
                {
                    bm25_scores.insert(c.name.clone(), s / max_score);
                }
            }
        }

        if bm25_as_signal {
            bm25_fallback_reason = "bm25-as-signal";
            all_candidates
        } else if bm25_hybrid {
            // Quality/feedback can only re-rank candidates with positive
            // retrieval evidence. This keeps Fast domain-independent: its
            // deterministic anchor + expansion and each skill's structured
            // task/triggers text are the evidence. When every score is zero we
            // return an empty pool instead of reviving unrelated high-prior
            // skills. Precise users may improve recall through Stage-1's one
            // bounded semantic expansion call, without adding a Fast LLM call.
            let mut scored: Vec<(usize, f64)> = all_candidates
                .iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let bm = bm25_scores.get(&r.name).copied()?;
                    let index_key = Database::skill_ai_index_key_for_resource(r);
                    let llm = indices.get(&index_key).map(|i| i.llm_score).unwrap_or(5);
                    let llm_val = (llm as f64) / 10.0;
                    let ff = feedback_factor_of(&r.name);
                    Some((i, hybrid_score(bm, llm_val, ff, feedback_disabled)))
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            if scored.is_empty() && all_candidates.len() <= bm25_candidate_limit {
                bm25_fallback_reason = "bounded-full-pool";
                all_candidates
            } else {
                bm25_fallback_reason = if scored.is_empty() {
                    "retrieval-zero"
                } else {
                    "bm25-hybrid"
                };
                scored
                    .into_iter()
                    .take(bm25_candidate_limit)
                    .map(|(i, _)| all_candidates[i].clone())
                    .collect()
            }
        } else {
            let positive: Vec<(usize, f64)> = ranked
                .into_iter()
                .filter(|(_, s)| *s > 0.0)
                .take(bm25_candidate_limit)
                .collect();
            if positive.is_empty() && all_candidates.len() <= bm25_candidate_limit {
                bm25_fallback_reason = "bounded-full-pool";
                all_candidates
            } else {
                bm25_fallback_reason = if positive.is_empty() {
                    "retrieval-zero"
                } else if positive.len() < BM25_MIN_POSITIVE_HITS {
                    "few-bm25-hits"
                } else {
                    "bm25-pure"
                };
                positive
                    .into_iter()
                    .map(|(i, _)| all_candidates[i].clone())
                    .collect()
            }
        }
    };
    if std::env::var("RUNAI_RECOMMEND_DEBUG").is_ok() {
        eprintln!(
            "[recommend debug] bm25 prefilter: total={}, kept={}, fallback={}",
            all_candidates_count,
            candidates.len(),
            if bm25_fallback_reason.is_empty() {
                "no"
            } else {
                bm25_fallback_reason
            },
        );
    }

    // Relevance cutoff produced zero candidates: no installed skill shares any
    // query-term overlap with the current intent. Skip the Stage-2 router LLM
    // entirely — no call, no tokens — and record a normal telemetry row so the
    // dashboard shows the skipped round. Stage-1 already ran and its compact
    // intent was appended to memory above, so intent recall is unaffected.
    if candidates.is_empty() {
        let decision = RouterDecision {
            mode: RouterMode::Exclusive,
            reasoning: NO_RELEVANT_CANDIDATE_REASONING.to_string(),
            skills: Vec::new(),
        };
        let ev = RouterEvent {
            id: None,
            ts: chrono::Utc::now().timestamp(),
            provider: provider_label(&cfg),
            model: cfg.model.clone(),
            // Stage-1 tokens only; Stage-2 never ran.
            prompt_tokens: intent_stats.prompt_tokens,
            completion_tokens: intent_stats.completion_tokens,
            reasoning_tokens: intent_stats.reasoning_tokens,
            total_tokens: intent_stats.total_tokens,
            cache_hit_tokens: intent_stats.cache_hit_tokens,
            cache_miss_tokens: intent_stats.cache_miss_tokens,
            latency_ms: 0,
            chosen_skills_json: "[]".to_string(),
            candidate_count: all_candidates_count as i64,
            status: "ok".to_string(),
            error_msg: None,
            session_id: session_id.unwrap_or("").to_string(),
            mode: RouterMode::Exclusive.as_str().to_string(),
            user_prompt: user_prompt.to_string(),
            cwd: cwd.unwrap_or("").to_string(),
            bm25_kept: 0,
            llm_raw_response: String::new(),
            hook_output: String::new(),
            llm_input: STAGE2_SKIPPED_LLM_INPUT.to_string(),
            intent_llm_input,
            intent_llm_raw_output,
            intent_llm_output: intent_summary.clone(),
            intent_status,
            intent_error_msg,
            bm25_candidates_json: "[]".to_string(),
            user_id: user_id.map(|s| s.to_string()),
            routing_mode: user_prefs.routing_mode.as_str().to_string(),
            empty_reason: EmptyReason::RetrievalZero.as_str().to_string(),
            retrieval_query: bm25_input_query.clone(),
            parsed_candidates_json: "[]".into(),
            filtered_candidates_json: "[]".into(),
            parser_recovery: false,
            llm_call_count: intent_call_attempts,
        };
        let _ = mgr.db().insert_router_event(&ev);
        write_last_recommend(mgr.paths(), &decision);
        return Ok(decision);
    }

    let bm25_candidate_names: Vec<String> = candidates.iter().map(|r| r.name.clone()).collect();
    let bm25_candidates_json =
        serde_json::to_string(&bm25_candidate_names).unwrap_or_else(|_| "[]".to_string());

    // Per-skill quality score 0-10. Owned entirely by the LLM enrich pass.
    // bm25 tags are only emitted in signal mode; in prefilter mode the
    // score already determined which candidates landed here.
    let emit_bm25_tag = bm25_as_signal;
    let candidate_listing: String = candidates
        .iter()
        .enumerate()
        .map(|(candidate_idx, r)| {
            let mut tags = String::new();
            if r.usage_count > 0 {
                tags.push_str(&format!(" [used:{}]", r.usage_count));
            }
            // `llm` tag = LLM-side enrich pass quality score (0-10). User
            // ratings are no longer part of the pipeline; the tag is named
            // explicitly `llm:N` rather than generic `score:N` to make this
            // obvious to the router LLM (and to humans inspecting the prompt).
            let index_key = Database::skill_ai_index_key_for_resource(r);
            if let Some(s) = indices.get(&index_key).map(|row| row.llm_score) {
                tags.push_str(&format!(" [llm:{}]", s));
            }
            // Real behavioural signal after [llm:]: [adopt:NN%] = of sessions
            // where the router chose this skill, how many the main agent
            // actually adopted; [fb:+P/-N] = explicit thumbs. Suppressed
            // entirely under RUNAI_FEEDBACK_DISABLED (maps are empty there).
            if !feedback_disabled {
                let stats = router_stats.get(&r.name).copied().unwrap_or_default();
                let (pos, neg) = feedback_counts.get(&r.name).copied().unwrap_or((0, 0));
                tags.push_str(&feedback_markers(
                    stats.chosen_sessions,
                    stats.adopted_sessions,
                    pos,
                    neg,
                ));
            }
            if emit_bm25_tag {
                let b = bm25_scores.get(&r.name).copied().unwrap_or(0.0);
                tags.push_str(&format!(" [bm25:{:.2}]", b));
            }
            let gs = groups_of(&r.id);
            if !gs.is_empty() {
                // Cap at 3 groups per line to keep candidate listing tight.
                let shown: Vec<&str> = gs.iter().take(3).map(String::as_str).collect();
                tags.push_str(&format!(" [group:{}]", shown.join(",")));
            }
            // Show the short router card when available — it is the compact
            // LLM-facing digest built specifically for this prompt. Falls
            // back to the raw description when a skill has not been enriched.
            let compact_card = match indices.get(&index_key) {
                Some(index) => structured_candidate_card(&index.summary, &index.router_card),
                None => structured_candidate_card("", &r.description),
            };
            format!(
                "C{:02} | {}{tags} | {}",
                candidate_idx + 1,
                r.name,
                compact_card
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let history = if precise && inject_history {
        transcript_path
            .map(|p| recent_transcript_messages(p, HISTORY_TURN_LIMIT))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let history_block = if history.is_empty() {
        String::new()
    } else {
        crate::core::prompts::template_body(HISTORY_PREFIX_TEMPLATE).replace("{HISTORY}", &history)
    };

    let cwd_block = match bounded_cwd.as_deref() {
        Some(c) if !c.is_empty() && inject_cwd => {
            crate::core::prompts::template_body(CWD_PREFIX_TEMPLATE).replace("{CWD}", c)
        }
        _ => String::new(),
    };
    let project_context_block = match bounded_cwd.as_deref() {
        Some(c) if precise && !c.is_empty() && cfg.read_claude_md && inject_project_context => {
            read_project_context(Path::new(c))
        }
        _ => String::new(),
    };

    let user_msg = build_router_user_message(RouterUserMessageParts {
        cwd_block: &cwd_block,
        project_context_block: &project_context_block,
        history_block: &history_block,
        task_anchor: &task_anchor,
        intent_summary: &intent_summary,
        candidate_listing: &candidate_listing,
        bm25_candidate_limit,
    });

    // Build conversation history when this session has prior turns AND
    // Conversation mode is on. Oneshot keeps history empty regardless.
    let history_turns: Vec<RouterTurn> = match (precise, cfg.session_mode, session_id) {
        (true, SessionMode::Conversation, Some(sid))
            if !sid.is_empty() && cfg.session_history_limit > 0 =>
        {
            let limit = cfg
                .session_history_limit
                .min(crate::core::recommend::SESSION_HISTORY_LIMIT_MAX)
                .min(HISTORY_TURN_LIMIT);
            bound_conversation_history(
                mgr.db()
                    .router_session_turn_history(sid, limit)
                    .unwrap_or_default(),
            )
        }
        _ => Vec::new(),
    };

    let started = Instant::now();
    let catalog: Vec<CandidateCatalogEntry> = candidates
        .iter()
        .enumerate()
        .map(|(idx, resource)| CandidateCatalogEntry {
            id: format!("C{:02}", idx + 1),
            name: resource.name.clone(),
        })
        .collect();
    let call_result = call_router(&cfg, &api_key, &user_msg, &history_turns, &catalog);
    let llm_call_count = intent_call_attempts + 1;
    let latency_ms = started.elapsed().as_millis() as i64;

    let (parsed, router_stats, status, error_msg, llm_raw) = match call_result {
        Ok((parsed, stats, raw)) => (parsed, stats, "ok".to_string(), None, raw),
        Err(e) => (
            ParsedRouterResponse {
                mode: RouterMode::Exclusive,
                reasoning: String::new(),
                parsed_values: Vec::new(),
                filtered_names: Vec::new(),
                recovery: false,
                empty_reason: EmptyReason::TransportError,
            },
            RouterCallStats::default(),
            "error".to_string(),
            Some(e.to_string()),
            String::new(),
        ),
    };
    let mode = parsed.mode;
    let reasoning = parsed.reasoning.clone();
    let mut chosen_names = parsed.filtered_names.clone();
    let stats = add_stats(&intent_stats, &router_stats);
    // `parse_router_response` already maps every protocol variant through the
    // request-local short-ID/name whitelist and enforces the output ceiling.
    if mode == RouterMode::Exclusive
        && recognized_intent.has(ScenarioConstraint::ImageReferenceRegeneration)
        && chosen_names.len() > 1
    {
        chosen_names.truncate(1);
    }
    if std::env::var("RUNAI_RECOMMEND_DEBUG").is_ok() {
        eprintln!(
            "[recommend debug] candidates={}, chosen={:?}, latency_ms={}, tokens={}",
            candidates.len(),
            chosen_names,
            latency_ms,
            stats.total_tokens
        );
    }

    // Build the decision NOW (resolve SKILL.md) so we can also capture
    // format_for_hook output and persist it to telemetry. Telemetry must
    // include both the LLM raw response (what the model said) and the hook
    // output (what we actually injected into Claude Code) so the dashboard
    // can show the full round-trip.
    let by_name: std::collections::HashMap<String, _> =
        candidates.iter().map(|r| (r.name.clone(), r)).collect();

    let mut out = Vec::new();
    for name in chosen_names.iter() {
        if let Some(r) = by_name.get(name) {
            // Prefer the compact router card when present; otherwise use
            // the raw description.
            let index_key = Database::skill_ai_index_key_for_resource(r);
            let desc_for_agent = match indices.get(&index_key) {
                Some(index) if !index.router_card.is_empty() => index.router_card.clone(),
                Some(index) if !index.summary.is_empty() => index.summary.clone(),
                _ => r.description.clone(),
            };
            out.push(RecommendedSkill {
                name: r.name.clone(),
                description: desc_for_agent,
            });
        }
    }
    let decision = RouterDecision {
        mode,
        reasoning: reasoning.clone(),
        skills: out,
    };
    let hook_output = if status == "ok" {
        // No session-recall block any more (session no-repeat removed): the
        // hook output never lists "已推参考池" skills, so this path no longer
        // reads `router_session_recommended_skills` — one fewer DB round-trip
        // on every recommend. `session_history` is passed empty; `skip_reminder`
        // is passed empty (the renderer ignores both now).
        //
        // CLI / library callers default to the local machine's LAN
        // IPv4-style URL. The server endpoint path (server::handle_recommend)
        // overrides via its own call with the request-derived server_url.
        let local_server_url = default_local_server_url();
        format_for_hook_full(
            &decision,
            session_id.unwrap_or(""),
            &[],
            &local_server_url,
            "",
            "",
        )
    } else {
        String::new()
    };

    // Persist the telemetry row regardless of success/failure so users can
    // audit cost & error rate. Best-effort: DB write failure does not block
    // the hook.
    let chosen_json = serde_json::to_string(&chosen_names).unwrap_or_else(|_| "[]".to_string());
    let ev = RouterEvent {
        id: None,
        ts: chrono::Utc::now().timestamp(),
        provider: provider_label(&cfg),
        model: cfg.model.clone(),
        prompt_tokens: stats.prompt_tokens,
        completion_tokens: stats.completion_tokens,
        reasoning_tokens: stats.reasoning_tokens,
        total_tokens: stats.total_tokens,
        cache_hit_tokens: stats.cache_hit_tokens,
        cache_miss_tokens: stats.cache_miss_tokens,
        latency_ms,
        chosen_skills_json: chosen_json,
        candidate_count: all_candidates_count as i64,
        status,
        error_msg: error_msg.clone(),
        session_id: session_id.unwrap_or("").to_string(),
        mode: mode.as_str().to_string(),
        user_prompt: user_prompt.to_string(),
        cwd: cwd.unwrap_or("").to_string(),
        bm25_kept: candidates.len() as i64,
        llm_raw_response: llm_raw,
        hook_output: hook_output.clone(),
        llm_input: user_msg.clone(),
        intent_llm_input,
        intent_llm_raw_output,
        intent_llm_output: intent_summary.clone(),
        intent_status,
        intent_error_msg,
        bm25_candidates_json,
        user_id: user_id.map(|s| s.to_string()),
        routing_mode: user_prefs.routing_mode.as_str().to_string(),
        empty_reason: parsed.empty_reason.as_str().to_string(),
        retrieval_query: bm25_input_query.clone(),
        parsed_candidates_json: serde_json::to_string(&parsed.parsed_values)
            .unwrap_or_else(|_| "[]".into()),
        filtered_candidates_json: serde_json::to_string(&chosen_names)
            .unwrap_or_else(|_| "[]".into()),
        parser_recovery: parsed.recovery,
        llm_call_count,
    };
    let _ = mgr.db().insert_router_event(&ev);

    // usage_count and session-adoption are bumped exclusively by the
    // activation command (`runai-client activate <skill>`) that the main
    // agent runs after accepting a recommendation. Recommending ≠ adopting
    // — the router never bumps counts on its own, no matter the mode.

    if let Some(err) = error_msg {
        bail!(err);
    }

    write_last_recommend(mgr.paths(), &decision);
    Ok(decision)
}

/// Write the most-recent router decision to `<data_dir>/last-recommend.json`.
/// Statusline tools (omc-hud, claude-hud, custom shell scripts) can read this
/// to surface the active skill in Claude Code's bottom bar. Best-effort: any
/// write error is silently swallowed so it never blocks the hook.
fn write_last_recommend(paths: &AppPaths, decision: &RouterDecision) {
    let skills = &decision.skills;
    let primary = skills.first().map(|s| s.name.as_str());
    let alternates: Vec<&str> = skills.iter().skip(1).map(|s| s.name.as_str()).collect();
    let entry = serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "mode": decision.mode.as_str(),
        "primary": primary,
        "alternates": alternates,
        "count": skills.len(),
    });
    let path = paths.data_dir().join("last-recommend.json");
    if let Ok(text) = serde_json::to_string_pretty(&entry) {
        let _ = std::fs::write(&path, text);
    }
}

fn call_router(
    cfg: &RecommendConfig,
    api_key: &str,
    user_msg: &str,
    history: &[RouterTurn],
    catalog: &[CandidateCatalogEntry],
) -> Result<(ParsedRouterResponse, RouterCallStats, String)> {
    let max_tokens = Some(ROUTER_MAX_TOKENS);
    let (raw, stats) = match cfg.provider {
        Provider::OpenaiCompat => call_openai_compat(cfg, api_key, user_msg, history, max_tokens)?,
        Provider::Anthropic => call_anthropic(cfg, api_key, user_msg, history, max_tokens)?,
        Provider::ClaudeCli => call_claude_cli(cfg, user_msg)?,
    };
    let parsed = parse_router_response(&raw, catalog, cfg.top_k);
    Ok((parsed, stats, raw))
}

#[derive(serde::Deserialize)]
struct RouterJsonOutput {
    mode: Option<String>,
    selected: Option<Vec<String>>,
    reasoning: Option<String>,
}

fn strip_fence(raw: &str) -> String {
    let trimmed = raw.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }
    let mut lines = trimmed.lines();
    let _ = lines.next();
    lines
        .take_while(|line| !line.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn normalized_selection_token(line: &str) -> String {
    let mut text = line.trim().trim_start_matches(['-', '*', '•']).trim();
    let digits = text.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &text[digits..];
        if rest.starts_with('.') || rest.starts_with(')') || rest.starts_with('、') {
            text = rest[1..].trim();
        }
    }
    for prefix in ["skill:", "skill：", "name:", "name："] {
        if text.to_ascii_lowercase().starts_with(prefix) {
            text = text[prefix.len()..].trim();
            break;
        }
    }
    text.trim_matches('`')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_end_matches([',', ';', '，', '；'])
        .to_string()
}

fn exact_mention(text: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let lower = text.to_lowercase();
    let needle = needle.to_lowercase();
    lower.match_indices(&needle).any(|(idx, _)| {
        let before = lower[..idx].chars().next_back();
        let after = lower[idx + needle.len()..].chars().next();
        let boundary = |c: Option<char>| {
            c.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '-' && ch != '_')
        };
        boundary(before) && boundary(after)
    })
}

pub(super) fn parse_router_response(
    raw: &str,
    catalog: &[CandidateCatalogEntry],
    limit: usize,
) -> ParsedRouterResponse {
    let trimmed = strip_fence(raw);
    let (mode, reasoning, parsed_values, explicit_empty) =
        if let Ok(json) = serde_json::from_str::<RouterJsonOutput>(&trimmed) {
            let mode = match json.mode.as_deref().map(str::to_ascii_lowercase).as_deref() {
                Some("compatible") => RouterMode::Compatible,
                _ => RouterMode::Exclusive,
            };
            let explicit_empty =
                matches!(json.selected.as_ref(), Some(selected) if selected.is_empty());
            let selected = json.selected.unwrap_or_default();
            (
                mode,
                json.reasoning.unwrap_or_default(),
                selected,
                explicit_empty,
            )
        } else {
            let lines = parse_lines(&trimmed);
            let (mode, reasoning, values) = split_mode_and_names(lines);
            let values: Vec<String> = values
                .into_iter()
                .map(|line| normalized_selection_token(&line))
                .filter(|line| !line.is_empty())
                .collect();
            let explicit_empty = trimmed.eq_ignore_ascii_case("NONE")
                || (values.is_empty()
                    && trimmed
                        .lines()
                        .any(|line| line.trim().eq_ignore_ascii_case("NONE")));
            (mode, reasoning, values, explicit_empty)
        };

    let by_id: std::collections::HashMap<String, String> = catalog
        .iter()
        .map(|entry| (entry.id.to_ascii_uppercase(), entry.name.clone()))
        .collect();
    let by_name: std::collections::HashMap<String, String> = catalog
        .iter()
        .map(|entry| (entry.name.to_lowercase(), entry.name.clone()))
        .collect();
    let mut filtered_names = Vec::new();
    for value in &parsed_values {
        let normalized = normalized_selection_token(value);
        let resolved = by_id
            .get(&normalized.to_ascii_uppercase())
            .or_else(|| by_name.get(&normalized.to_lowercase()));
        if let Some(name) = resolved
            && !filtered_names.contains(name)
        {
            filtered_names.push(name.clone());
        }
    }
    filtered_names.truncate(limit);

    let mut recovery = false;
    if filtered_names.is_empty() && !explicit_empty && !reasoning.trim().is_empty() {
        let mentions: Vec<String> = catalog
            .iter()
            .filter(|entry| reasoning_mentions_candidate(&reasoning, entry))
            .map(|entry| entry.name.clone())
            .collect();
        if mentions.len() == 1 {
            filtered_names = mentions;
            recovery = true;
        }
    }

    let empty_reason = if !filtered_names.is_empty() {
        EmptyReason::None
    } else if explicit_empty {
        EmptyReason::ModelEmpty
    } else if raw.trim().is_empty() || parsed_values.is_empty() {
        EmptyReason::ParseEmpty
    } else {
        EmptyReason::FilteredEmpty
    };

    ParsedRouterResponse {
        mode,
        reasoning,
        parsed_values,
        filtered_names,
        recovery,
        empty_reason,
    }
}

fn reasoning_mentions_candidate(reasoning: &str, entry: &CandidateCatalogEntry) -> bool {
    let lower = reasoning.to_lowercase();
    let positive_terms = [
        "选择",
        "选用",
        "使用",
        "采用",
        "推荐",
        "命中",
        "匹配",
        "合适",
        "适合",
        "最佳",
        "choose",
        "select",
        "use",
        "recommend",
        "match",
        "suitable",
        "best",
        "relevant",
    ];
    let negative_terms = [
        "不合适",
        "不适合",
        "不要",
        "禁止",
        "不能",
        "不可",
        "并非",
        "拒绝",
        "不应",
        "不选",
        "不使用",
        "不推荐",
        "无需",
        "排除",
        "剔除",
        "否定",
        "别用",
        "not suitable",
        "cannot",
        "can't",
        "must not",
        "forbidden",
        "not allowed",
        "not a match",
        "not relevant",
        "refuse",
        "refused",
        "do not",
        "don't",
        "exclude",
        "avoid",
        "reject",
        "irrelevant",
    ];
    for needle in [&entry.id, &entry.name] {
        if !exact_mention(reasoning, needle) {
            continue;
        }
        let needle_lower = needle.to_lowercase();
        let Some(idx) = lower.find(&needle_lower) else {
            continue;
        };
        let before: String = lower[..idx].chars().rev().take(32).collect();
        let after: String = lower[idx + needle_lower.len()..].chars().take(40).collect();
        let window = format!("{}{}", before.chars().rev().collect::<String>(), after);
        if negative_terms.iter().any(|term| window.contains(term)) {
            continue;
        }
        if positive_terms.iter().any(|term| window.contains(term)) {
            return true;
        }
    }
    false
}

/// Parse router output into `(mode, reasoning, skill_names)`.
///
/// Expected shape:
/// ```text
/// COMPATIBLE                  ← line 1: mode tag
/// reasoning: 用户在做 X，建议 A+B  ← line 2 (optional): `reasoning:` prefix
/// skill-a                     ← line 3+: one skill name each
/// skill-b
/// ```
///
/// Missing / unknown mode → defaults to `Exclusive` (safer — main agent
/// will ask the user to pick). Missing `reasoning:` line → empty string;
/// the renderer hides the block.
pub(super) fn split_mode_and_names(content: Vec<String>) -> (RouterMode, String, Vec<String>) {
    let mut iter = content.into_iter().filter(|l| !l.is_empty());
    let first = match iter.next() {
        Some(s) => s,
        None => return (RouterMode::Exclusive, String::new(), Vec::new()),
    };
    let upper = first.to_ascii_uppercase();
    let mode = if upper == "COMPATIBLE" {
        RouterMode::Compatible
    } else if upper == "EXCLUSIVE" {
        RouterMode::Exclusive
    } else {
        // First line wasn't a tag — treat it as a skill name and default
        // to Exclusive. Defensive against LLMs that forget the tag.
        let mut names = vec![first];
        names.extend(iter);
        return (RouterMode::Exclusive, String::new(), names);
    };

    let mut reasoning = String::new();
    let mut names: Vec<String> = Vec::new();
    for line in iter {
        let stripped = line.trim();
        let lower = stripped.to_ascii_lowercase();
        if reasoning.is_empty()
            && (lower.starts_with("reasoning:") || lower.starts_with("reasoning："))
        {
            // accept both ASCII and fullwidth colon
            let body = stripped
                .split_once([':', '：'])
                .map(|(_, rest)| rest)
                .unwrap_or("")
                .trim();
            reasoning = body.to_string();
            continue;
        }
        names.push(line);
    }
    (mode, reasoning, names)
}

/// Strip bullets / quotes / whitespace from each line of LLM output. Empty
/// lines are dropped. Caller (split_mode_and_names) interprets the first
/// non-empty line as either a COMPATIBLE/EXCLUSIVE tag or a skill name.
pub(super) fn parse_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|l| l.trim().trim_start_matches('-').trim().trim_matches('`'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

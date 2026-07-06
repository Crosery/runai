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
use super::enrich::rewrite_query_for_bm25;
use super::hook_output::format_for_hook_full;
use super::intent::{build_intent_memory_from_prompt, build_intent_summary};
use super::llm_call::{RouterCallStats, call_anthropic, call_claude_cli, call_openai_compat};
use super::project_context::read_project_context;
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

// All router prompts and hook output templates live in src/core/prompts/ and
// are exposed as `PROMPT_<NAME>` consts via the centralised registry
// (`crate::core::prompts`). Edit the .md files to retune wording.
const USER_MSG_TEMPLATE: &str = crate::core::prompts::PROMPT_RECOMMEND_USER;
const HISTORY_PREFIX_TEMPLATE: &str = crate::core::prompts::PROMPT_RECOMMEND_HISTORY_PREFIX;
const ALREADY_ROUTED_TEMPLATE: &str = crate::core::prompts::PROMPT_RECOMMEND_ALREADY_ROUTED;
const CWD_PREFIX_TEMPLATE: &str = crate::core::prompts::PROMPT_RECOMMEND_CWD_PREFIX;

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

pub(super) struct RouterUserMessageParts<'a> {
    pub(super) user_prompt: &'a str,
    pub(super) cwd_block: &'a str,
    pub(super) project_context_block: &'a str,
    pub(super) history_block: &'a str,
    pub(super) already_routed_block: &'a str,
    pub(super) intent_summary: &'a str,
    pub(super) candidate_listing: &'a str,
    pub(super) top_k: usize,
    pub(super) bm25_candidate_limit: usize,
}

pub(super) fn build_router_user_message(parts: RouterUserMessageParts<'_>) -> String {
    crate::core::prompts::template_body(USER_MSG_TEMPLATE)
        .replace("{HISTORY_BLOCK}", parts.history_block)
        .replace("{ALREADY_ROUTED_BLOCK}", parts.already_routed_block)
        .replace("{CWD_BLOCK}", parts.cwd_block)
        .replace("{PROJECT_CONTEXT_BLOCK}", parts.project_context_block)
        .replace("{INTENT_SUMMARY}", parts.intent_summary)
        .replace("{CANDIDATE_LISTING}", parts.candidate_listing)
        .replace("{USER_PROMPT}", parts.user_prompt)
        .replace("{TOP_K}", &parts.top_k.to_string())
        .replace(
            "{BM25_CANDIDATE_LIMIT}",
            &parts.bm25_candidate_limit.to_string(),
        )
}

/// Top-level entry: run the router and return the list of recommended skills.
/// Returns `Ok(Vec::new())` when nothing matches, when disabled, or when prompt
/// is too short.
///
/// `transcript_path`, when supplied, points at the Claude Code session jsonl.
/// The last few user+assistant text messages are appended to the LLM input so
/// the router can recognize replies like "use figma-component-mapping" and pick
/// the right skill on the next round.
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
    let user_prefs: crate::core::prefs::UserPrefs = match user_id {
        Some(uid) => mgr
            .db()
            .find_user_by_id(uid)
            .ok()
            .flatten()
            .map(|u| crate::core::prefs::UserPrefs::from_json_str(&u.prefs_json))
            .unwrap_or_default(),
        None => crate::core::prefs::UserPrefs::default(),
    };
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
    // ClaudeCli reuses the user's Claude Code session — no API key needed.
    let api_key = if cfg.provider == Provider::ClaudeCli {
        String::new()
    } else {
        cfg.effective_api_key()
            .context("recommend api_key not configured: run `runai recommend setup` or set RUNAI_RECOMMEND_API_KEY")?
    };

    let client_kind = client_kind
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("claude");
    let intent_memory_limit = if user_prefs.intent_memory_enabled {
        user_prefs.intent_memory_limit as usize
    } else {
        0
    };
    let mut intent_memory = Vec::new();
    if let Some(sid) = session_id
        && !sid.is_empty()
        && intent_memory_limit > 0
    {
        let memory = build_intent_memory_from_prompt(user_prompt);
        let _ = mgr.db().append_router_intent_memory(
            sid,
            user_id,
            client_kind,
            &memory,
            intent_memory_limit,
        );
        intent_memory = mgr
            .db()
            .router_intent_memory(sid, user_id, client_kind, intent_memory_limit)
            .unwrap_or_default()
            .into_iter()
            .map(|i| i.memory)
            .collect();
    }
    let intent_summary = build_intent_summary(user_prompt, cwd, client_kind, &intent_memory);

    // `already_routed` is the dedup signal handed to the router LLM. It is
    // the **full** recommendation history this session (every skill the
    // router has proposed), not just adoptions. Rationale: even if the
    // main agent declined to Read a skill, it has already seen the name in
    // a previous hook output, and re-recommending unrelated-but-same-name
    // skills (e.g. ppt-anything → guizang-ppt-skill → pptx three turns in
    // a row) is the most obvious "the router doesn't remember" failure
    // mode users notice. The recommend_system prompt tells the LLM to skip
    // these unless the user explicitly asks to revisit one ("再用一次 X").
    let already_routed = match session_id {
        Some(sid) if !sid.is_empty() => mgr
            .db()
            .router_session_recommended_skills(sid)
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    let db = mgr.db();
    let mut all_candidates: Vec<_> = match user_id {
        Some(uid) => {
            let prefs = db
                .find_user_by_id(uid)
                .ok()
                .flatten()
                .map(|u| crate::core::prefs::UserPrefs::from_json_str(&u.prefs_json))
                .unwrap_or_default();
            if prefs.allow_public_recommend {
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
    // intent summary. `output_top_k` controls final recommendations;
    // `bm25_candidate_limit` controls how many candidates the router LLM sees.
    let output_top_k: usize = cfg.top_k.clamp(1, BM25_TOP_K_MAX);
    let bm25_candidate_limit: usize = std::env::var("RUNAI_BM25_TOP_K")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(user_prefs.bm25_candidate_limit as usize)
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

    // Query expansion (opt-in): rewrite short prompts via the LLM into a
    // BM25-friendly keyword list before prefilter. Off by default —
    // empirically in hybrid mode (`bm25 * 0.4 + llm_score/10 * 0.6`) the
    // LLM-score weight dominates and reshuffling BM25 rarely changes the
    // bounded top-K; the rewrite call adds latency with no chosen-set change.
    // Worth enabling only with `RUNAI_QUERY_REWRITE_ENABLE=1`, typically
    // paired with `RUNAI_BM25_PURE=1` to give BM25 score more weight.
    // Failure falls back to the original prompt.
    let rewrite_enabled = std::env::var("RUNAI_QUERY_REWRITE_ENABLE").is_ok();
    let expanded_query = if !rewrite_enabled || intent_summary.chars().count() > 800 {
        None
    } else {
        let api_key_for_rewrite = if cfg.provider == Provider::ClaudeCli {
            String::new()
        } else {
            cfg.effective_api_key().unwrap_or_default()
        };
        if cfg.provider == Provider::ClaudeCli || !api_key_for_rewrite.is_empty() {
            rewrite_query_for_bm25(&cfg, &api_key_for_rewrite, &intent_summary)
        } else {
            None
        }
    };

    let bm25_input_query: String = match &expanded_query {
        Some(expanded) => format!("{intent_summary}\n{expanded}"),
        None => intent_summary.clone(),
    };

    let q_terms = bm25::tokenize(&bm25_input_query);
    let mut bm25_fallback_reason: &'static str = "";

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
                        r.description.as_str()
                    } else {
                        index.search_doc.as_str()
                    }
                } else {
                    r.description.as_str()
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
            // Hybrid score = BM25 * 0.4 + LLM/10 * 0.6
            // User-side ratings are intentionally NOT used — the LLM enrich
            // pass owns quality scoring end-to-end (incorporating implicit
            // user feedback when re-enriching). Keeps the system one-axis
            // simpler and avoids the noise of sparse manual ratings.
            let mut scored: Vec<(usize, f64)> = all_candidates
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    let bm = bm25_scores.get(&r.name).copied().unwrap_or(0.0);
                    let index_key = Database::skill_ai_index_key_for_resource(r);
                    let llm = indices.get(&index_key).map(|i| i.llm_score).unwrap_or(5);
                    let llm_val = (llm as f64) / 10.0;
                    let hybrid = bm * 0.4 + llm_val * 0.6;
                    (i, hybrid)
                })
                .collect();
            scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            // Drop trailing entries with zero BM25 + LLM default — they
            // contribute no signal at all.
            bm25_fallback_reason = "bm25-hybrid";
            scored
                .into_iter()
                .take(bm25_candidate_limit)
                .map(|(i, _)| all_candidates[i].clone())
                .collect()
        } else {
            let positive: Vec<(usize, f64)> = ranked
                .into_iter()
                .filter(|(_, s)| *s > 0.0)
                .take(bm25_candidate_limit)
                .collect();
            if positive.len() < BM25_MIN_POSITIVE_HITS {
                bm25_fallback_reason = if positive.is_empty() {
                    "no-bm25-hits"
                } else {
                    "few-bm25-hits"
                };
                all_candidates
            } else {
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

    // Per-skill quality score 0-10. Owned entirely by the LLM enrich pass.
    // bm25 tags are only emitted in signal mode; in prefilter mode the
    // score already determined which candidates landed here.
    let emit_bm25_tag = bm25_as_signal;
    let candidate_listing: String = candidates
        .iter()
        .map(|r| {
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
            let body_for_llm = match indices.get(&index_key) {
                Some(index) if !index.router_card.is_empty() => index.router_card.as_str(),
                _ => r.description.as_str(),
            };
            format!("- {}{tags}: {}", r.name, body_for_llm)
        })
        .collect::<Vec<_>>()
        .join("\n");

    // PLANNING §1.3: per-user injection flags strip optional blocks BEFORE
    // they're substituted into USER_MSG_TEMPLATE. Each block is dropped to
    // the empty string when its toggle is off; the USER_MSG_TEMPLATE then
    // renders without that section just as if there had been no signal
    // (history empty / no cwd / no already_routed). Defaults to true when
    // the key is missing, so legacy / unauthenticated callers behave as
    // they did before §1.3 landed.
    let inject_history = user_prefs.prompt_injection_enabled("recommend_history_prefix");
    let inject_already_routed = user_prefs.prompt_injection_enabled("recommend_already_routed");
    let inject_cwd = user_prefs.prompt_injection_enabled("recommend_cwd_prefix");
    let inject_project_context = user_prefs.prompt_injection_enabled("recommend_project_context");

    let history = if inject_history {
        transcript_path
            .map(|p| recent_transcript_messages(p, 6))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let history_block = if history.is_empty() {
        String::new()
    } else {
        crate::core::prompts::template_body(HISTORY_PREFIX_TEMPLATE).replace("{HISTORY}", &history)
    };

    let already_routed_block = if !inject_already_routed || already_routed.is_empty() {
        String::new()
    } else {
        crate::core::prompts::template_body(ALREADY_ROUTED_TEMPLATE)
            .replace("{ALREADY_ROUTED}", &already_routed.join(", "))
    };

    let cwd_block = match cwd {
        Some(c) if !c.is_empty() && inject_cwd => {
            crate::core::prompts::template_body(CWD_PREFIX_TEMPLATE).replace("{CWD}", c)
        }
        _ => String::new(),
    };
    let project_context_block = match cwd {
        Some(c) if !c.is_empty() && cfg.read_claude_md && inject_project_context => {
            read_project_context(Path::new(c))
        }
        _ => String::new(),
    };

    let user_msg = build_router_user_message(RouterUserMessageParts {
        user_prompt,
        cwd_block: &cwd_block,
        project_context_block: &project_context_block,
        history_block: &history_block,
        already_routed_block: &already_routed_block,
        intent_summary: &intent_summary,
        candidate_listing: &candidate_listing,
        top_k: output_top_k,
        bm25_candidate_limit,
    });

    // Build conversation history when this session has prior turns AND
    // Conversation mode is on. Oneshot keeps history empty regardless.
    let history_turns: Vec<RouterTurn> = match (cfg.session_mode, session_id) {
        (SessionMode::Conversation, Some(sid))
            if !sid.is_empty() && cfg.session_history_limit > 0 =>
        {
            mgr.db()
                .router_session_turn_history(sid, cfg.session_history_limit)
                .unwrap_or_default()
                .into_iter()
                .map(|(user, assistant)| RouterTurn { user, assistant })
                .collect()
        }
        _ => Vec::new(),
    };

    let started = Instant::now();
    let call_result = call_router(&cfg, &api_key, &user_msg, &history_turns);
    let latency_ms = started.elapsed().as_millis() as i64;

    let (mode, reasoning, chosen_names, stats, status, error_msg, llm_raw) = match call_result {
        Ok((mode, reasoning, names, stats, raw)) => {
            (mode, reasoning, names, stats, "ok".to_string(), None, raw)
        }
        Err(e) => (
            RouterMode::Exclusive,
            String::new(),
            Vec::new(),
            RouterCallStats::default(),
            "error".to_string(),
            Some(e.to_string()),
            String::new(),
        ),
    };
    // Drop names that the LLM hallucinated against the candidate set (they
    // can't be loaded). Already-routed names stay eligible here: the prompt
    // warns the router about them, but follow-up requests can still re-surface
    // the same skill if it is the right answer again.
    let candidate_set: std::collections::HashSet<String> =
        candidates.iter().map(|r| r.name.clone()).collect();
    let chosen_names: Vec<String> = chosen_names
        .into_iter()
        .filter(|n| candidate_set.contains(n))
        .collect();
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
        // Pull this session's previous recommendations so the hook output
        // can remind the main agent which skills it already saw — cuts
        // down on repeat recommendations of skills already in context.
        let history = match session_id {
            Some(sid) if !sid.is_empty() => mgr
                .db()
                .router_session_recommended_skills(sid)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        // CLI / library callers default to the local machine's LAN
        // IPv4-style URL (so any process / agent on the LAN can curl it,
        // not just loopback). The server endpoint path
        // (server::handle_recommend) overrides via its own call with the
        // request-derived server_url + user_header.
        let local_server_url = default_local_server_url();
        let skip_reminder = if cfg.skip_reminder_enabled {
            cfg.skip_reminder_template.as_str()
        } else {
            ""
        };
        format_for_hook_full(
            &decision,
            session_id.unwrap_or(""),
            &history,
            &local_server_url,
            "",
            skip_reminder,
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
        provider: match cfg.provider {
            Provider::OpenaiCompat => "openai-compat".into(),
            Provider::Anthropic => "anthropic".into(),
            Provider::ClaudeCli => "claude-cli".into(),
        },
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
        user_id: user_id.map(|s| s.to_string()),
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
) -> Result<(RouterMode, String, Vec<String>, RouterCallStats, String)> {
    let (raw, stats) = match cfg.provider {
        Provider::OpenaiCompat => call_openai_compat(cfg, api_key, user_msg, history)?,
        Provider::Anthropic => call_anthropic(cfg, api_key, user_msg, history)?,
        // ClaudeCli always boots a fresh Claude Code session per call,
        // so conversation replay would have to ship the entire history
        // through stdin every time — defeats the cost story. Stay oneshot.
        Provider::ClaudeCli => call_claude_cli(cfg, user_msg)?,
    };
    let (mode, reasoning, names) = split_mode_and_names(parse_lines(&raw));
    Ok((mode, reasoning, names, stats, raw))
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

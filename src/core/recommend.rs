use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::core::bm25;
use crate::core::db::RouterEvent;
use crate::core::manager::SkillManager;
use crate::core::paths::AppPaths;
use crate::core::resource::ResourceKind;

/// Skill prefilter cap: how many candidates the BM25 ranker keeps before the
/// LLM precision-picks. Anthropic's Contextual Retrieval paper finds 20-50
/// gives the best signal-to-noise for downstream LLM rerank. 50 leaves room
/// for the LLM to ignore false positives without losing genuine candidates.
const BM25_TOP_K: usize = 50;
/// If the user prompt tokenizes to fewer than this many terms (e.g. "ok"),
/// skip BM25 and pass the full candidate set — BM25 on a single token would
/// pick a near-random top-K and exclude unrelated-but-relevant skills.
const BM25_MIN_QUERY_TERMS: usize = 2;

// All router prompts and hook output templates live in src/core/prompts/ so
// they are not scattered through the code. Edit those files to retune wording;
// the placeholders below are substituted with str::replace at runtime.
const SYSTEM_PROMPT_TEMPLATE: &str = include_str!("prompts/recommend_system.md");
const USER_MSG_TEMPLATE: &str = include_str!("prompts/recommend_user.md");
const HISTORY_PREFIX_TEMPLATE: &str = include_str!("prompts/recommend_history_prefix.md");
const ALREADY_ROUTED_TEMPLATE: &str = include_str!("prompts/recommend_already_routed.md");
const CWD_PREFIX_TEMPLATE: &str = include_str!("prompts/recommend_cwd_prefix.md");
const PROJECT_CONTEXT_TEMPLATE: &str = include_str!("prompts/recommend_project_context.md");
const HOOK_INLINE_TEMPLATE: &str = include_str!("prompts/hook_inline.md");
const HOOK_POINTER_TEMPLATE: &str = include_str!("prompts/hook_pointer.md");
const HOOK_MULTI_TEMPLATE: &str = include_str!("prompts/hook_multi.md");

/// Mode tag returned by the router on the first line of its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterMode {
    /// Skills in this set can be loaded together (e.g. github + writing-skills).
    Compatible,
    /// Skills are mutually exclusive — user must pick one (e.g. multiple image gen providers).
    Exclusive,
}

impl RouterMode {
    fn as_str(self) -> &'static str {
        match self {
            RouterMode::Compatible => "compatible",
            RouterMode::Exclusive => "exclusive",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecommendConfig {
    pub enabled: bool,
    pub provider: Provider,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub top_k: usize,
    pub min_prompt_len: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    OpenaiCompat,
    Anthropic,
    /// Spawn `claude -p --model <model>` (uses the user's Claude Code session,
    /// including Max plan quota — no API key needed). Slower than direct API
    /// because each call boots Claude Code's full system prompt (~5-10s per
    /// run even with cache hits), but free for Max subscribers.
    ClaudeCli,
}

impl Default for RecommendConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: Provider::OpenaiCompat,
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-v4-flash".into(),
            api_key: String::new(),
            top_k: 3,
            min_prompt_len: 0,
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawConfig {
    recommend: Option<RecommendConfig>,
}

#[derive(Debug, Serialize)]
struct WrappedConfig<'a> {
    recommend: &'a RecommendConfig,
}

impl RecommendConfig {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        let path = paths.config_path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let raw: RawConfig =
            toml::from_str(&text).with_context(|| format!("parse {}", path.display()))?;
        Ok(raw.recommend.unwrap_or_default())
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        let path = paths.config_path();
        let wrapped = WrappedConfig { recommend: self };
        let text = toml::to_string_pretty(&wrapped).context("serialize recommend config")?;
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        Self::set_owner_only(&path);
        Ok(())
    }

    #[cfg(unix)]
    fn set_owner_only(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(path) {
            let mut perms = metadata.permissions();
            perms.set_mode(0o600);
            let _ = fs::set_permissions(path, perms);
        }
    }

    #[cfg(not(unix))]
    fn set_owner_only(_path: &Path) {}

    pub fn effective_api_key(&self) -> Option<String> {
        if !self.api_key.is_empty() {
            return Some(self.api_key.clone());
        }
        std::env::var("RUNAI_RECOMMEND_API_KEY").ok()
    }
}

/// A single recommended skill. For the primary pick, `content` holds the full
/// SKILL.md text. For alternates, `content` is empty (only `name` + `description`
/// are surfaced so the main agent can ask the user which to load).
#[derive(Debug, Clone)]
pub struct RecommendedSkill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub content: String,
}

/// Full router output: the mode tag plus the ranked skill list.
#[derive(Debug, Clone)]
pub struct RouterDecision {
    pub mode: RouterMode,
    pub skills: Vec<RecommendedSkill>,
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
    let cfg = RecommendConfig::load(mgr.paths())?;
    if !cfg.enabled {
        return Ok(RouterDecision {
            mode: RouterMode::Exclusive,
            skills: Vec::new(),
        });
    }
    if user_prompt.trim().chars().count() < cfg.min_prompt_len {
        return Ok(RouterDecision {
            mode: RouterMode::Exclusive,
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

    let already_routed = match session_id {
        Some(sid) if !sid.is_empty() => mgr
            .db()
            .router_session_routed_skills(sid)
            .unwrap_or_default(),
        _ => Vec::new(),
    };

    let resources = mgr.list_resources(None, None)?;
    let all_candidates: Vec<_> = resources
        .into_iter()
        .filter(|r| r.kind == ResourceKind::Skill)
        .collect();
    if all_candidates.is_empty() {
        return Ok(RouterDecision {
            mode: RouterMode::Exclusive,
            skills: Vec::new(),
        });
    }
    let all_candidates_count = all_candidates.len();

    // BM25 prefilter. Without it the LLM sees all ~343 candidates and gets
    // noise-flooded — empirically this is what tanks chosen-rate to ~46%
    // even when a relevant skill exists. After prefilter the LLM sees a
    // focused top-K with strong term-overlap with the user prompt.
    //
    // Short / ambiguous prompts (< 2 query terms) skip the prefilter — BM25
    // on a single token degenerates to "any doc containing that token" and
    // hides legitimate matches whose desc happens to use a synonym.
    let bm25_disabled = std::env::var("RUNAI_BM25_DISABLED").is_ok();
    let q_terms = bm25::tokenize(user_prompt);
    let candidates: Vec<_> = if bm25_disabled || q_terms.len() < BM25_MIN_QUERY_TERMS {
        all_candidates
    } else {
        let docs: Vec<String> = all_candidates
            .iter()
            .map(|r| format!("{} {}", r.name, r.description))
            .collect();
        let ranked = bm25::rank(user_prompt, &docs);
        // Keep only docs with positive score; if none score (totally
        // unrelated prompt) fall back to passing all candidates so the LLM
        // can still make a semantic judgement.
        let positive: Vec<_> = ranked.iter().filter(|(_, s)| *s > 0.0).take(BM25_TOP_K).collect();
        if positive.is_empty() {
            all_candidates
        } else {
            positive
                .into_iter()
                .map(|(i, _)| all_candidates[*i].clone())
                .collect()
        }
    };
    if std::env::var("RUNAI_RECOMMEND_DEBUG").is_ok() {
        eprintln!(
            "[recommend debug] bm25 prefilter: total={}, kept={}",
            all_candidates_count,
            candidates.len()
        );
    }

    let candidate_listing: String = candidates
        .iter()
        .map(|r| {
            let usage_tag = if r.usage_count > 0 {
                format!(" [used:{}]", r.usage_count)
            } else {
                String::new()
            };
            // Description is already capped at 200 chars at adoption time
            // (see scanner / classifier). No further truncation here — the
            // router needs the full signal.
            format!("- {}{usage_tag}: {}", r.name, r.description)
        })
        .collect::<Vec<_>>()
        .join("\n");

    let history = transcript_path
        .map(|p| recent_transcript_messages(p, 6))
        .unwrap_or_default();
    let history_block = if history.is_empty() {
        String::new()
    } else {
        HISTORY_PREFIX_TEMPLATE.replace("{HISTORY}", &history)
    };

    let already_routed_block = if already_routed.is_empty() {
        String::new()
    } else {
        ALREADY_ROUTED_TEMPLATE.replace("{ALREADY_ROUTED}", &already_routed.join(", "))
    };

    let cwd_block = match cwd {
        Some(c) if !c.is_empty() => CWD_PREFIX_TEMPLATE.replace("{CWD}", c),
        _ => String::new(),
    };
    let project_context_block = match cwd {
        Some(c) if !c.is_empty() => read_project_context(Path::new(c)),
        _ => String::new(),
    };

    let user_msg = USER_MSG_TEMPLATE
        .replace("{HISTORY_BLOCK}", &history_block)
        .replace("{ALREADY_ROUTED_BLOCK}", &already_routed_block)
        .replace("{CWD_BLOCK}", &cwd_block)
        .replace("{PROJECT_CONTEXT_BLOCK}", &project_context_block)
        .replace("{CANDIDATE_LISTING}", &candidate_listing)
        .replace("{USER_PROMPT}", user_prompt)
        .replace("{TOP_K}", &cfg.top_k.to_string());

    let started = Instant::now();
    let call_result = call_router(&cfg, &api_key, &user_msg);
    let latency_ms = started.elapsed().as_millis() as i64;

    let (mode, chosen_names, stats, status, error_msg, llm_raw) = match call_result {
        Ok((mode, names, stats, raw)) => (mode, names, stats, "ok".to_string(), None, raw),
        Err(e) => (
            RouterMode::Exclusive,
            Vec::new(),
            RouterCallStats::default(),
            "error".to_string(),
            Some(e.to_string()),
            String::new(),
        ),
    };
    // Drop names that the LLM hallucinated against the candidate set (they
    // can't be loaded). Also drop anything in already_routed to enforce
    // session memory at the runai layer regardless of LLM compliance.
    let already_set: std::collections::HashSet<String> = already_routed.iter().cloned().collect();
    let candidate_set: std::collections::HashSet<String> =
        candidates.iter().map(|r| r.name.clone()).collect();
    let chosen_names: Vec<String> = chosen_names
        .into_iter()
        .filter(|n| candidate_set.contains(n) && !already_set.contains(n))
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
    for (idx, name) in chosen_names.iter().enumerate() {
        if let Some(r) = by_name.get(name) {
            let skill_md = mgr.paths().skills_dir().join(&r.name).join("SKILL.md");
            let content = if idx == 0 {
                match fs::read_to_string(&skill_md) {
                    Ok(c) => c,
                    Err(_) => continue,
                }
            } else {
                String::new()
            };
            out.push(RecommendedSkill {
                name: r.name.clone(),
                description: r.description.clone(),
                path: skill_md,
                content,
            });
        }
    }
    let decision = RouterDecision {
        mode,
        skills: out,
    };
    let hook_output = if status == "ok" {
        format_for_hook(&decision)
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
    };
    let _ = mgr.db().insert_router_event(&ev);

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
        let _ = fs::write(&path, text);
    }
}

/// Format a COMPATIBLE multi-skill set. Each skill becomes its own inline
/// block if its content fits the per-skill budget; otherwise it shows as a
/// pointer line telling the main agent to Read once. The total output is hard
/// capped at 9 KB to stay under Claude Code's 10 KB UserPromptSubmit cap.
fn format_compatible_set(skills: &[RecommendedSkill]) -> String {
    const HARD_BUDGET: usize = 9000;
    const PER_SKILL_INLINE_LIMIT: usize = 4000;

    let mut buf = String::new();
    buf.push_str("# Skill recommendations (runai recommend)\n\n");
    buf.push_str(
        "**Compatible skill set — these skills can be combined; load them all and use as needed.** Start your reply with one short line listing the activated skills, e.g. `激活 skills: a, b, c`. Then follow the inlined SKILL.md content directly. For any skill below shown as `(too large — Read once)`, Read its path exactly once.\n\n\
         **These skills are already loaded for this turn via runai's UserPromptSubmit hook.** Do NOT call `sm_enable` / `sm_install` / `runai enable` / `runai install` / any 'activate' or 'install' tool — the SKILL.md content / paths below ARE the activation. Even if `sm_list` shows any of them as 'disabled', that only affects future sessions; for this turn they are usable.\n\n",
    );
    buf.push_str(&format!(
        "激活 skills: {}\n\n",
        skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    for s in skills {
        let header = format!("---\n## {}\nSource path: `{}`\n", s.name, s.path.display());
        if buf.len() + header.len() > HARD_BUDGET {
            buf.push_str(&format!(
                "\n(Remaining skills omitted to stay under 10 KB hook cap. Read these one-by-one as needed: {})\n",
                skills
                    .iter()
                    .skip_while(|x| x.name != s.name)
                    .map(|x| x.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            break;
        }
        buf.push_str(&header);
        if !s.content.is_empty()
            && s.content.len() <= PER_SKILL_INLINE_LIMIT
            && buf.len() + s.content.len() < HARD_BUDGET
        {
            buf.push('\n');
            buf.push_str(&s.content);
            if !s.content.ends_with('\n') {
                buf.push('\n');
            }
        } else {
            buf.push_str(&format!(
                "(too large — Read once at the path above; what it does: {})\n",
                s.description.chars().take(160).collect::<String>()
            ));
        }
    }
    buf
}

/// Format recommended skills as the `UserPromptSubmit` hook stdout text.
/// Output is plain markdown; Claude Code injects hook stdout as additional
/// context before the user prompt.
pub fn format_for_hook(decision: &RouterDecision) -> String {
    let skills = &decision.skills;
    if skills.is_empty() {
        return String::new();
    }

    // Multi-skill + COMPATIBLE → inline all primaries' SKILL.md if they fit
    // under the 10 KB hook cap. Each compatible skill becomes its own inline
    // block. Falls back to pointer mode for the over-budget ones.
    if skills.len() > 1 && decision.mode == RouterMode::Compatible {
        return format_compatible_set(skills);
    }

    if skills.len() == 1 {
        // Single match. Claude Code caps UserPromptSubmit hook stdout (and
        // JSON additionalContext) at 10,000 chars — anything larger gets
        // persisted to a temp file with a 2 KB preview, forcing the main
        // agent to Read that temp file. So:
        //   - small SKILL.md (~≤ 8 KB after instruction overhead) → inline the
        //     full content, zero Read needed.
        //   - large SKILL.md → point at the path, main agent Reads it once.
        const INLINE_BUDGET: usize = 8000;
        let primary = &skills[0];

        if !primary.content.is_empty() && primary.content.len() <= INLINE_BUDGET {
            HOOK_INLINE_TEMPLATE
                .replace("{NAME}", &primary.name)
                .replace("{PATH}", &primary.path.display().to_string())
                .replace("{CONTENT}", &primary.content)
        } else {
            let desc_short: String = primary.description.chars().take(200).collect();
            HOOK_POINTER_TEMPLATE
                .replace("{NAME}", &primary.name)
                .replace("{PATH}", &primary.path.display().to_string())
                .replace("{DESC}", &desc_short)
        }
    } else {
        let candidates: String = skills
            .iter()
            .map(|s| format!("- **{}** — {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n");
        HOOK_MULTI_TEMPLATE.replace("{CANDIDATES}", &candidates)
    }
}

#[derive(Debug, Default, Clone)]
struct RouterCallStats {
    prompt_tokens: i64,
    completion_tokens: i64,
    reasoning_tokens: i64,
    total_tokens: i64,
    cache_hit_tokens: i64,
    cache_miss_tokens: i64,
}

fn call_router(
    cfg: &RecommendConfig,
    api_key: &str,
    user_msg: &str,
) -> Result<(RouterMode, Vec<String>, RouterCallStats, String)> {
    let (raw, stats) = match cfg.provider {
        Provider::OpenaiCompat => call_openai_compat(cfg, api_key, user_msg)?,
        Provider::Anthropic => call_anthropic(cfg, api_key, user_msg)?,
        Provider::ClaudeCli => call_claude_cli(cfg, user_msg)?,
    };
    let (mode, names) = split_mode_and_names(parse_lines(&raw));
    Ok((mode, names, stats, raw))
}

/// Run the router via `claude -p --model <model>`. Uses the user's Claude
/// Code session (cookies + Max plan quota), no API key. Slower than direct
/// API because every spawn boots Claude Code's full system prompt.
fn call_claude_cli(cfg: &RecommendConfig, user_msg: &str) -> Result<(String, RouterCallStats)> {
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
            v.get("duration_ms").map(|x| x.to_string()).unwrap_or_default(),
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

/// Pop the first non-empty line as the mode tag; remaining lines are skill
/// names. Unknown / missing tag defaults to `Exclusive` (the safer choice —
/// the main agent will ask the user to pick).
fn split_mode_and_names(content: Vec<String>) -> (RouterMode, Vec<String>) {
    let mut iter = content.into_iter().filter(|l| !l.is_empty());
    let first = match iter.next() {
        Some(s) => s,
        None => return (RouterMode::Exclusive, Vec::new()),
    };
    let upper = first.to_ascii_uppercase();
    if upper == "COMPATIBLE" {
        (RouterMode::Compatible, iter.collect())
    } else if upper == "EXCLUSIVE" {
        (RouterMode::Exclusive, iter.collect())
    } else {
        // First line wasn't a tag — keep its original case as a skill name
        // and default to Exclusive (safer — main agent will ask user to
        // pick). Defensive against LLMs that forget the tag.
        let mut names = vec![first];
        names.extend(iter);
        (RouterMode::Exclusive, names)
    }
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

fn call_openai_compat(
    cfg: &RecommendConfig,
    api_key: &str,
    user_msg: &str,
) -> Result<(String, RouterCallStats)> {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    // Disable thinking on reasoning models so the router answers instantly.
    // DeepSeek V4 honors `thinking.type=disabled` (drops reasoning_tokens to
    // None). For non-reasoning models or other OpenAI-compat backends this
    // field is silently ignored, so it's safe to always send.
    // max_tokens is intentionally omitted — let the model use its full budget.
    let body = serde_json::json!({
        "model": cfg.model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT_TEMPLATE},
            {"role": "user", "content": user_msg},
        ],
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

fn call_anthropic(
    cfg: &RecommendConfig,
    api_key: &str,
    user_msg: &str,
) -> Result<(String, RouterCallStats)> {
    let url = format!("{}/v1/messages", cfg.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": 256,
        "system": SYSTEM_PROMPT_TEMPLATE,
        "messages": [{"role": "user", "content": user_msg}],
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

/// Read `<cwd>/CLAUDE.md` and any files it `@`-references, trim each to
/// `PER_FILE_LIMIT` chars, and wrap in the PROJECT_CONTEXT template.
/// Returns empty string when CLAUDE.md is absent — AGENTS.md and other docs
/// are only pulled in if CLAUDE.md explicitly references them via `@<path>`.
///
/// Why: the router LLM only sees user prompt + cwd path string — it doesn't
/// know the project's tool conventions. Injecting CLAUDE.md (and the files
/// it points at via Claude Code's `@<file>` reference syntax) lets it learn
/// e.g. "kaiwu has a `kaiwu submit` command", so when the user says "提交
/// 模型" in that cwd it routes correctly instead of defaulting to `github`.
///
/// Scope: CLAUDE.md is the entry point. Its `@<relative-or-absolute-path>`
/// references are resolved one level deep (no recursion through referenced
/// files' own `@` references — keeps prompt size bounded and avoids cycles).
fn read_project_context(cwd: &Path) -> String {
    const PER_FILE_LIMIT: usize = 2500;
    const MAX_REFERENCED_FILES: usize = 5;

    let claude_path = cwd.join("CLAUDE.md");
    let claude_raw = match fs::read_to_string(&claude_path) {
        Ok(s) => s,
        Err(_) => return String::new(),
    };
    let trimmed = claude_raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let mut blocks: Vec<String> = Vec::new();
    blocks.push(format_doc_block("CLAUDE.md", trimmed, PER_FILE_LIMIT));

    // Pull in files referenced by @<path>. Only `.md` / `.txt` files are
    // honored — anything else is probably a code path the LLM doesn't need.
    let refs = extract_at_references(trimmed);
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    seen.insert(claude_path.clone());
    for raw_ref in refs.into_iter().take(MAX_REFERENCED_FILES) {
        let lower = raw_ref.to_ascii_lowercase();
        if !lower.ends_with(".md") && !lower.ends_with(".txt") {
            continue;
        }
        let target = if Path::new(&raw_ref).is_absolute() {
            PathBuf::from(&raw_ref)
        } else {
            cwd.join(&raw_ref)
        };
        let canonical = target.canonicalize().unwrap_or_else(|_| target.clone());
        if !seen.insert(canonical.clone()) {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&target) {
            let t = content.trim();
            if t.is_empty() {
                continue;
            }
            blocks.push(format_doc_block(&raw_ref, t, PER_FILE_LIMIT));
        }
    }

    PROJECT_CONTEXT_TEMPLATE.replace("{PROJECT_DOCS}", &blocks.join("\n\n"))
}

fn format_doc_block(label: &str, body: &str, limit: usize) -> String {
    let snippet: String = body.chars().take(limit).collect();
    let truncated_note = if body.chars().count() > limit {
        "\n[…truncated]"
    } else {
        ""
    };
    format!("--- {label} ---\n{snippet}{truncated_note}")
}

/// Extract `@<path>` references from a CLAUDE.md body. Matches the Claude
/// Code file-reference syntax: an `@` followed by a path token (letters,
/// digits, `._/-`). The leading `@` must be at start-of-line or preceded by
/// whitespace so we don't pick up email addresses or `@mentions`. Returns
/// paths in the order they appear, deduplicated.
fn extract_at_references(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in body.lines() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'@' && (i == 0 || bytes[i - 1].is_ascii_whitespace()) {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() {
                    let c = bytes[end];
                    let ok = c.is_ascii_alphanumeric()
                        || c == b'.'
                        || c == b'_'
                        || c == b'/'
                        || c == b'-';
                    if !ok {
                        break;
                    }
                    end += 1;
                }
                if end > start {
                    let token = &line[start..end];
                    if (token.contains('.') || token.contains('/'))
                        && seen.insert(token.to_string())
                    {
                        out.push(token.to_string());
                    }
                }
                i = end;
            } else {
                i += 1;
            }
        }
    }
    out
}

/// Read the most recent `n` user/assistant text messages from a Claude Code
/// session jsonl, oldest-first. Tool calls/results are dropped; only plain
/// text is kept. Returns empty string on any read or parse error.
pub fn recent_transcript_messages(transcript_path: &Path, n: usize) -> String {
    let raw = match fs::read_to_string(transcript_path) {
        Ok(s) => s,
        Err(_) => return String::new(),
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
    msgs[take_from..]
        .iter()
        .map(|(r, t)| format!("[{r}] {t}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Result of attempting to install the UserPromptSubmit hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookInstallStatus {
    Installed,
    AlreadyPresent,
    Removed,
    NotPresent,
}

const HOOK_COMMAND: &str = "runai recommend";

/// Install the UserPromptSubmit hook into `<home>/.claude/settings.json`.
/// Idempotent: re-running when our hook is already present is a no-op.
/// Other existing hooks (user's own or other tools) are preserved verbatim.
/// A `.runai-bak` snapshot of the previous settings.json is written next to it.
pub fn install_claude_hook(home: &Path) -> Result<HookInstallStatus> {
    let claude_dir = home.join(".claude");
    let path = claude_dir.join("settings.json");
    let mut value = read_settings_json(&path)?;
    let ups_arr = ensure_user_prompt_submit_array(&mut value)?;

    if hook_already_present(ups_arr) {
        return Ok(HookInstallStatus::AlreadyPresent);
    }

    ups_arr.push(serde_json::json!({
        "hooks": [
            {"type": "command", "command": HOOK_COMMAND}
        ]
    }));

    write_settings_json(&path, &value)?;
    Ok(HookInstallStatus::Installed)
}

/// Remove the runai-installed hook from settings.json. Leaves unrelated hook
/// entries (and the rest of the file) untouched.
pub fn uninstall_claude_hook(home: &Path) -> Result<HookInstallStatus> {
    let path = home.join(".claude").join("settings.json");
    if !path.exists() {
        return Ok(HookInstallStatus::NotPresent);
    }
    let mut value = read_settings_json(&path)?;
    let ups_arr = match get_user_prompt_submit_array(&mut value) {
        Some(arr) => arr,
        None => return Ok(HookInstallStatus::NotPresent),
    };
    let before = ups_arr.len();
    ups_arr.retain(|group| {
        let arr = match group.get("hooks").and_then(|h| h.as_array()) {
            Some(a) => a,
            None => return true,
        };
        // Drop the whole group only if every hook inside it is ours.
        let all_ours = !arr.is_empty()
            && arr
                .iter()
                .all(|h| h.get("command").and_then(|c| c.as_str()) == Some(HOOK_COMMAND));
        !all_ours
    });
    if ups_arr.len() == before {
        return Ok(HookInstallStatus::NotPresent);
    }
    write_settings_json(&path, &value)?;
    Ok(HookInstallStatus::Removed)
}

/// Install or remove a `SessionStart` hook in `~/.claude/settings.json` that
/// runs `command_str` (e.g. `runai server --ensure`) every time Claude Code
/// starts a new session. The user's other SessionStart hooks are preserved.
///
/// Identification: we match by command-string equality so re-running the
/// installer is a no-op and uninstall only removes our entry.
pub fn install_session_start_hook(home: &Path, command_str: &str) -> Result<HookInstallStatus> {
    let path = home.join(".claude").join("settings.json");
    let mut value = read_settings_json(&path)?;
    let arr = ensure_named_hook_array(&mut value, "SessionStart")?;
    if hook_command_present(arr, command_str) {
        return Ok(HookInstallStatus::AlreadyPresent);
    }
    arr.push(serde_json::json!({
        "hooks": [{"type": "command", "command": command_str}]
    }));
    write_settings_json(&path, &value)?;
    Ok(HookInstallStatus::Installed)
}

pub fn uninstall_session_start_hook(home: &Path, command_str: &str) -> Result<HookInstallStatus> {
    let path = home.join(".claude").join("settings.json");
    if !path.exists() {
        return Ok(HookInstallStatus::NotPresent);
    }
    let mut value = read_settings_json(&path)?;
    let arr = match get_named_hook_array(&mut value, "SessionStart") {
        Some(a) => a,
        None => return Ok(HookInstallStatus::NotPresent),
    };
    let before = arr.len();
    arr.retain(|group| {
        let h = match group.get("hooks").and_then(|h| h.as_array()) {
            Some(a) => a,
            None => return true,
        };
        let all_ours = !h.is_empty()
            && h.iter()
                .all(|x| x.get("command").and_then(|c| c.as_str()) == Some(command_str));
        !all_ours
    });
    if arr.len() == before {
        return Ok(HookInstallStatus::NotPresent);
    }
    write_settings_json(&path, &value)?;
    Ok(HookInstallStatus::Removed)
}

fn ensure_named_hook_array<'a>(
    value: &'a mut serde_json::Value,
    name: &str,
) -> Result<&'a mut Vec<serde_json::Value>> {
    let obj = value
        .as_object_mut()
        .context("settings.json root must be an object")?;
    let hooks = obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .context("settings.json `hooks` field must be an object")?;
    let entry = hooks_obj
        .entry(name.to_string())
        .or_insert_with(|| serde_json::json!([]));
    entry
        .as_array_mut()
        .with_context(|| format!("settings.json `hooks.{name}` must be an array"))
}

fn get_named_hook_array<'a>(
    value: &'a mut serde_json::Value,
    name: &str,
) -> Option<&'a mut Vec<serde_json::Value>> {
    value
        .as_object_mut()?
        .get_mut("hooks")?
        .as_object_mut()?
        .get_mut(name)?
        .as_array_mut()
}

fn hook_command_present(arr: &[serde_json::Value], command_str: &str) -> bool {
    arr.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|hs| {
                hs.iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command_str))
            })
    })
}

fn read_settings_json(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let txt = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if txt.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&txt).with_context(|| format!("parse {} as JSON", path.display()))
}

fn ensure_user_prompt_submit_array(
    value: &mut serde_json::Value,
) -> Result<&mut Vec<serde_json::Value>> {
    let obj = value
        .as_object_mut()
        .context("settings.json root must be an object")?;
    let hooks_entry = obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks_entry
        .as_object_mut()
        .context("settings.json `hooks` field must be an object")?;
    let ups = hooks_obj
        .entry("UserPromptSubmit".to_string())
        .or_insert_with(|| serde_json::json!([]));
    ups.as_array_mut()
        .context("settings.json `hooks.UserPromptSubmit` must be an array")
}

fn get_user_prompt_submit_array(
    value: &mut serde_json::Value,
) -> Option<&mut Vec<serde_json::Value>> {
    value
        .as_object_mut()?
        .get_mut("hooks")?
        .as_object_mut()?
        .get_mut("UserPromptSubmit")?
        .as_array_mut()
}

fn hook_already_present(ups_arr: &[serde_json::Value]) -> bool {
    ups_arr.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|arr| {
                arr.iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(HOOK_COMMAND))
            })
    })
}

fn write_settings_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    if path.exists() {
        let bak = path.with_extension("json.runai-bak");
        let _ = fs::copy(path, &bak);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(value)?;
    fs::write(path, pretty).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Strip bullets / quotes / whitespace from each line of LLM output. Empty
/// lines are dropped. Caller (split_mode_and_names) interprets the first
/// non-empty line as either a COMPATIBLE/EXCLUSIVE tag or a skill name.
fn parse_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(|l| l.trim().trim_start_matches('-').trim().trim_matches('`'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_disabled() {
        let cfg = RecommendConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.provider, Provider::OpenaiCompat);
        assert_eq!(cfg.base_url, "https://api.deepseek.com/v1");
        assert_eq!(cfg.model, "deepseek-v4-flash");
    }

    #[test]
    fn parse_lines_strips_dash_and_backtick() {
        let raw = "figma-alignment\n- another-skill\n`third-skill`\n\n";
        let names = parse_lines(raw);
        assert_eq!(
            names,
            vec!["figma-alignment", "another-skill", "third-skill"]
        );
    }

    #[test]
    fn parse_empty_input() {
        assert!(parse_lines("").is_empty());
        assert!(parse_lines("   \n\n").is_empty());
    }

    #[test]
    fn extract_at_refs_basic() {
        let body = "# header\n@AGENTS.md\nsome text\n";
        assert_eq!(extract_at_references(body), vec!["AGENTS.md"]);
    }

    #[test]
    fn extract_at_refs_inline_and_relative_paths() {
        let body = "see @docs/spec.md and @../shared.md\nbut not user@example.com";
        let refs = extract_at_references(body);
        assert_eq!(refs, vec!["docs/spec.md", "../shared.md"]);
    }

    #[test]
    fn extract_at_refs_dedupes() {
        let body = "@AGENTS.md\n@AGENTS.md\n@AGENTS.md\n";
        assert_eq!(extract_at_references(body), vec!["AGENTS.md"]);
    }

    #[test]
    fn extract_at_refs_requires_path_like_token() {
        // Plain `@word` (no dot, no slash) — likely an @mention, skip.
        let body = "@mention not-a-file\n@./local.md yes\n";
        assert_eq!(extract_at_references(body), vec!["./local.md"]);
    }

    #[test]
    fn project_context_returns_empty_without_claude_md() {
        let tmp = tempfile::tempdir().unwrap();
        // AGENTS.md alone is no longer enough — CLAUDE.md is the entry point.
        fs::write(tmp.path().join("AGENTS.md"), "# agents only").unwrap();
        assert!(read_project_context(tmp.path()).is_empty());
    }

    #[test]
    fn project_context_inlines_claude_md_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("CLAUDE.md"), "# project rules\nbe nice").unwrap();
        let out = read_project_context(tmp.path());
        assert!(out.contains("--- CLAUDE.md ---"));
        assert!(out.contains("project rules"));
        // No @ refs in this file -> AGENTS.md is NOT pulled in even if it exists.
        fs::write(tmp.path().join("AGENTS.md"), "# secret agents").unwrap();
        let out2 = read_project_context(tmp.path());
        assert!(!out2.contains("secret agents"));
    }

    #[test]
    fn project_context_follows_at_refs_to_agents_md() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("CLAUDE.md"),
            "# project\n@AGENTS.md\nmore content",
        )
        .unwrap();
        fs::write(tmp.path().join("AGENTS.md"), "# agents body\ndo X").unwrap();
        let out = read_project_context(tmp.path());
        assert!(out.contains("--- CLAUDE.md ---"));
        assert!(out.contains("--- AGENTS.md ---"));
        assert!(out.contains("agents body"));
    }

    #[test]
    fn project_context_ignores_nonmd_at_refs() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("CLAUDE.md"),
            "@code.rs\n@notes.md\n@image.png",
        )
        .unwrap();
        fs::write(tmp.path().join("code.rs"), "fn main() {}").unwrap();
        fs::write(tmp.path().join("notes.md"), "# notes inlined").unwrap();
        fs::write(tmp.path().join("image.png"), b"\x89PNG").unwrap();
        let out = read_project_context(tmp.path());
        assert!(out.contains("notes inlined"));
        assert!(!out.contains("fn main"));
        assert!(!out.contains("PNG"));
    }

    fn decision(mode: RouterMode, skills: Vec<RecommendedSkill>) -> RouterDecision {
        RouterDecision { mode, skills }
    }

    #[test]
    fn format_empty_skills_returns_empty_string() {
        assert!(format_for_hook(&decision(RouterMode::Exclusive, vec![])).is_empty());
    }

    #[test]
    fn format_single_match_small_inlines_full_content() {
        let s = RecommendedSkill {
            name: "figma-alignment".into(),
            description: "align vue/h5 to figma".into(),
            path: PathBuf::from("/x/SKILL.md"),
            content: "tiny skill content body".into(),
        };
        let out = format_for_hook(&decision(RouterMode::Exclusive, vec![s]));
        assert!(
            out.len() < 10_000,
            "must stay under 10KB hook cap, got {}",
            out.len()
        );
        assert!(out.contains("激活 skill: figma-alignment"));
        assert!(out.contains("/x/SKILL.md"));
        assert!(out.contains("DO NOT Read the file path"));
        assert!(out.contains("tiny skill content body"));
    }

    #[test]
    fn format_single_match_large_points_at_path_no_inline() {
        let huge = "x".repeat(9000);
        let s = RecommendedSkill {
            name: "huge-skill".into(),
            description: "a very large skill".into(),
            path: PathBuf::from("/x/huge/SKILL.md"),
            content: huge.clone(),
        };
        let out = format_for_hook(&decision(RouterMode::Exclusive, vec![s]));
        assert!(
            out.len() < 10_000,
            "must stay under 10KB hook cap, got {}",
            out.len()
        );
        assert!(out.contains("激活 skill: huge-skill"));
        assert!(out.contains("/x/huge/SKILL.md"));
        assert!(out.contains("Read it ONCE"));
        assert!(!out.contains(&huge), "large content must not be inlined");
    }

    #[test]
    fn format_exclusive_multi_surfaces_candidates_without_injection() {
        let a = RecommendedSkill {
            name: "figma-alignment".into(),
            description: "align vue to figma".into(),
            path: PathBuf::from("/x/figma/SKILL.md"),
            content: "should NOT appear in output".into(),
        };
        let b = RecommendedSkill {
            name: "figma-component-mapping".into(),
            description: "map figma node to vue component".into(),
            path: PathBuf::from("/x/map/SKILL.md"),
            content: String::new(),
        };
        let out = format_for_hook(&decision(RouterMode::Exclusive, vec![a, b]));
        assert!(out.contains("Multiple skills"));
        assert!(out.contains("- **figma-alignment**"));
        assert!(out.contains("- **figma-component-mapping**"));
        assert!(!out.contains("should NOT appear"));
        assert!(!out.contains("/x/figma/SKILL.md"));
        assert!(!out.contains("/x/map/SKILL.md"));
    }

    #[test]
    fn format_compatible_multi_inlines_all_under_budget() {
        let a = RecommendedSkill {
            name: "github".into(),
            description: "gh cli wrapper".into(),
            path: PathBuf::from("/x/github/SKILL.md"),
            content: "github skill body — small".into(),
        };
        let b = RecommendedSkill {
            name: "writing-skills".into(),
            description: "write/edit skills".into(),
            path: PathBuf::from("/x/writing/SKILL.md"),
            content: "writing skill body — also small".into(),
        };
        let out = format_for_hook(&decision(RouterMode::Compatible, vec![a, b]));
        assert!(out.contains("Compatible skill set"));
        assert!(out.contains("激活 skills: github, writing-skills"));
        assert!(out.contains("github skill body"));
        assert!(out.contains("writing skill body"));
        assert!(out.len() < 10_000);
    }

    #[test]
    fn split_mode_compatible_then_skills() {
        let (mode, names) = split_mode_and_names(vec![
            "COMPATIBLE".into(),
            "github".into(),
            "writing-skills".into(),
        ]);
        assert_eq!(mode, RouterMode::Compatible);
        assert_eq!(names, vec!["github", "writing-skills"]);
    }

    #[test]
    fn split_mode_exclusive_then_skills() {
        let (mode, names) = split_mode_and_names(vec![
            "EXCLUSIVE".into(),
            "generate-image".into(),
            "fal-ai-media".into(),
        ]);
        assert_eq!(mode, RouterMode::Exclusive);
        assert_eq!(names, vec!["generate-image", "fal-ai-media"]);
    }

    #[test]
    fn split_mode_missing_tag_defaults_to_exclusive() {
        // If the LLM forgets the tag, treat the first line as a skill and
        // default mode to Exclusive (safer — user decides).
        let (mode, names) =
            split_mode_and_names(vec!["just-one-skill".into(), "another-skill".into()]);
        assert_eq!(mode, RouterMode::Exclusive);
        assert_eq!(names, vec!["just-one-skill", "another-skill"]);
    }

    #[test]
    fn split_mode_empty_returns_exclusive_empty() {
        let (mode, names) = split_mode_and_names(vec![]);
        assert_eq!(mode, RouterMode::Exclusive);
        assert!(names.is_empty());
    }

    #[test]
    fn save_then_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        let cfg = RecommendConfig {
            enabled: true,
            api_key: "test-key".into(),
            ..RecommendConfig::default()
        };
        cfg.save(&paths).unwrap();
        let loaded = RecommendConfig::load(&paths).unwrap();
        assert!(loaded.enabled);
        assert_eq!(loaded.api_key, "test-key");
    }

    #[test]
    fn install_hook_into_empty_home() {
        let tmp = tempfile::tempdir().unwrap();
        let s = install_claude_hook(tmp.path()).unwrap();
        assert_eq!(s, HookInstallStatus::Installed);
        let txt = fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap();
        assert!(txt.contains("UserPromptSubmit"));
        assert!(txt.contains("runai recommend"));
    }

    #[test]
    fn install_hook_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            install_claude_hook(tmp.path()).unwrap(),
            HookInstallStatus::Installed
        );
        assert_eq!(
            install_claude_hook(tmp.path()).unwrap(),
            HookInstallStatus::AlreadyPresent
        );
    }

    #[test]
    fn install_hook_preserves_existing_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_dir = tmp.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let pre = serde_json::json!({
            "theme": "dark",
            "model": "sonnet",
            "hooks": {
                "PostToolUse": [
                    {"hooks": [{"type": "command", "command": "my-formatter"}]}
                ],
                "UserPromptSubmit": [
                    {"hooks": [{"type": "command", "command": "user-existing-hook"}]}
                ]
            }
        });
        fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&pre).unwrap(),
        )
        .unwrap();

        assert_eq!(
            install_claude_hook(tmp.path()).unwrap(),
            HookInstallStatus::Installed
        );
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(after["theme"], "dark");
        assert_eq!(after["model"], "sonnet");
        assert_eq!(
            after["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "my-formatter"
        );
        let ups = after["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 2);
        assert_eq!(ups[0]["hooks"][0]["command"], "user-existing-hook");
        assert_eq!(ups[1]["hooks"][0]["command"], "runai recommend");
        // backup written
        assert!(claude_dir.join("settings.json.runai-bak").exists());
    }

    #[test]
    fn uninstall_hook_removes_only_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let claude_dir = tmp.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let pre = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [
                    {"hooks": [{"type": "command", "command": "user-existing-hook"}]},
                    {"hooks": [{"type": "command", "command": "runai recommend"}]}
                ]
            }
        });
        fs::write(
            claude_dir.join("settings.json"),
            serde_json::to_string_pretty(&pre).unwrap(),
        )
        .unwrap();

        assert_eq!(
            uninstall_claude_hook(tmp.path()).unwrap(),
            HookInstallStatus::Removed
        );
        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(claude_dir.join("settings.json")).unwrap())
                .unwrap();
        let ups = after["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(ups.len(), 1);
        assert_eq!(ups[0]["hooks"][0]["command"], "user-existing-hook");
    }

    #[test]
    fn uninstall_hook_when_missing_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(
            uninstall_claude_hook(tmp.path()).unwrap(),
            HookInstallStatus::NotPresent
        );
    }

    #[test]
    fn load_missing_returns_default() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        let cfg = RecommendConfig::load(&paths).unwrap();
        assert!(!cfg.enabled);
    }

    #[test]
    fn effective_api_key_prefers_config() {
        // SAFETY: test sets+removes env. Mark unsafe per Rust 2024 edition contract.
        unsafe {
            std::env::set_var("RUNAI_RECOMMEND_API_KEY", "from-env");
        }
        let mut cfg = RecommendConfig {
            api_key: "from-config".into(),
            ..RecommendConfig::default()
        };
        assert_eq!(cfg.effective_api_key().as_deref(), Some("from-config"));
        cfg.api_key.clear();
        assert_eq!(cfg.effective_api_key().as_deref(), Some("from-env"));
        unsafe {
            std::env::remove_var("RUNAI_RECOMMEND_API_KEY");
        }
    }
}

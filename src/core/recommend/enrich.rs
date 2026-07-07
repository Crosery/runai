//! AI summary enrichment: batch worker pool + single-skill feedback re-enrich.
//!
//! `enrich_skills` plans per-skill work, then runs a `std::thread::scope`
//! worker pool — each worker clones the shared Arcs (queue / report / progress
//! / cfg / api_key / db_path) and opens its OWN rusqlite `Connection` (it is
//! `!Sync`, so the per-worker `Database::open` is load-bearing, not an
//! accident). Language enforcement runs after parse: a mismatch triggers one
//! loud-correction retry in the batch path, or an outright bail in the
//! interactive `reevaluate_skill` path — nothing wrong-language is ever written.

use anyhow::{Context, Result, bail};
use std::fs;
use std::path::PathBuf;

use crate::core::db::Database;
use crate::core::manager::SkillManager;
use crate::core::resource::{Resource, ResourceKind};

use super::config::{Provider, RecommendConfig};
use super::lang_validation::summary_matches_lang;
use super::llm_call::call_summary_llm;
use super::prompts::{build_enrich_prompt, build_feedback_prompt};
use crate::core::db::SkillAiIndex;
use sha2::{Digest, Sha256};

/// Outcome of an `enrich_skills` run.
#[derive(Debug, Clone, Default)]
pub struct EnrichReport {
    pub generated: usize,
    pub skipped_have_summary: usize,
    pub skipped_no_skill_md: usize,
    pub refreshed_stale: usize,
    pub errors: Vec<(String, String)>,
}

/// What to do with skills that already have a summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichMode {
    /// Only enrich skills that have NO summary at all. Cheapest, used when
    /// a new skill is installed and only that one needs a first pass.
    MissingOnly,
    /// Default: enrich missing skills, plus re-enrich any skill whose
    /// source content or prompt layout hash changed since the stored index
    /// was written.
    Stale,
    /// Re-enrich every skill regardless of state. Expensive — 343 LLM calls.
    Force,
}

/// Per-skill work item produced by the planner.
struct EnrichJob {
    name: String,
    description: String,
    owner_user_id: Option<String>,
    skill_md_path: PathBuf,
    has_summary: bool,
}

fn sha256_hex(parts: &[&[u8]]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    format!("{:x}", hasher.finalize())
}

fn compact_text(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

fn parse_summary_field(summary: &str, label: &str) -> String {
    for line in summary.lines() {
        let trimmed = line.trim();
        if let Some((lhs, rhs)) = trimmed.split_once([':', '：'])
            && lhs.trim().eq_ignore_ascii_case(label)
        {
            return rhs.trim().to_string();
        }
    }
    String::new()
}

fn build_search_doc(name: &str, description: &str, summary: &str, _skill_md: &str) -> String {
    let triggers = parse_summary_field(summary, "triggers");
    let task = parse_summary_field(summary, "task");
    let inputs = parse_summary_field(summary, "inputs");
    let outputs = parse_summary_field(summary, "outputs");
    compact_text(
        &format!("{name} {description} {task} {triggers} {inputs} {outputs}"),
        3000,
    )
}

fn build_router_card(name: &str, description: &str, summary: &str) -> String {
    let task = parse_summary_field(summary, "task");
    let triggers = parse_summary_field(summary, "triggers");
    let inputs = parse_summary_field(summary, "inputs");
    let outputs = parse_summary_field(summary, "outputs");
    let not_for = parse_summary_field(summary, "not-for");
    let body = if task.is_empty() {
        format!("{name} {description} {summary}")
    } else {
        format!("{name}: {task} | {triggers} | {inputs} | {outputs} | {not_for}")
    };
    compact_text(&body, 320)
}

fn prompt_layout_key(summary_lang: &str) -> &'static str {
    match summary_lang.trim() {
        "" | "zh" => "summary-task-triggers-inputs-outputs-not-for-score",
        "en" => "summary-task-triggers-inputs-outputs-not-for-score",
        "ja" => "summary-task-triggers-inputs-outputs-not-for-score",
        "bilingual" => "summary-task-triggers-inputs-outputs-not-for-score",
        _ => "summary-task-triggers-inputs-outputs-not-for-score",
    }
}

fn make_index(
    name: &str,
    description: &str,
    summary: &str,
    score: i64,
    skill_md: &str,
    summary_lang: &str,
) -> SkillAiIndex {
    let search_doc = build_search_doc(name, description, summary, skill_md);
    let router_card = build_router_card(name, description, summary);
    let source_hash = sha256_hex(&[
        name.as_bytes(),
        b"\0",
        description.as_bytes(),
        b"\0",
        skill_md.as_bytes(),
    ]);
    let prompt_key = prompt_layout_key(summary_lang);
    let prompt_hash = sha256_hex(&[summary_lang.trim().as_bytes(), b"\0", prompt_key.as_bytes()]);
    SkillAiIndex {
        summary: summary.trim().to_string(),
        search_doc,
        router_card,
        llm_score: score.clamp(0, 10),
        updated_at: chrono::Utc::now().timestamp(),
        source_hash,
        prompt_hash,
        format_key: prompt_key.to_string(),
    }
}

/// Owner-aware enrich candidate enumeration: the public pool PLUS every user's
/// private skills (scope `"*"`). The old `mgr.list_resources(None, None)` was
/// public-pool only, so private uploads at `<data>/users/<uid>/skills/` never
/// got summarized — they sat permanently "未富集". This is the single source
/// of skills the enricher (and the feedback re-enrich path) considers.
pub(super) fn enrich_candidates(db: &Database) -> Result<Vec<Resource>> {
    db.list_resources_for_user(Some(ResourceKind::Skill), Some("*"))
}

/// SKILL.md path for a skill row — its OWN directory, never the public pool.
/// Private skills live at `<data>/users/<uid>/skills/<name>/`, so resolving via
/// `skills_dir().join(name)` (the old behavior) pointed at a non-existent public
/// path and the enrich silently skipped the skill (`skipped_no_skill_md`).
pub(super) fn enrich_skill_md_path(r: &Resource) -> PathBuf {
    r.directory.join("SKILL.md")
}

/// Generate AI summaries for skills. Uses the configured router LLM (same
/// one the hook calls). Concurrent execution: `concurrency` worker threads
/// pull from a shared queue, each makes one LLM call at a time. DB writes
/// happen on each worker's own connection (SQLite handles WAL concurrency).
///
/// `limit = None` means enrich everything that needs it in one pass.
pub fn enrich_skills(
    mgr: &SkillManager,
    limit: Option<usize>,
    mode: EnrichMode,
    verbose: bool,
    concurrency: usize,
    only_names: Option<&[String]>,
) -> Result<EnrichReport> {
    let cfg = RecommendConfig::load(mgr.paths())?;
    if !cfg.enabled {
        if verbose {
            eprintln!("[enrich] skipped — router not enabled (run `runai recommend setup`)");
        }
        return Ok(EnrichReport::default());
    }
    // Hard gate: never generate summaries until the user has explicitly
    // chosen a summary language. A defaulted/unselected language is what let
    // English-source skills leak English summaries into a "zh" index. This
    // line is always printed (not gated on verbose) because it is a rare,
    // actionable state — the user needs to know enrichment is intentionally
    // held back, and how to release it.
    if !cfg.summary_lang_confirmed {
        eprintln!(
            "[enrich] skipped — summary language not chosen yet. \
             Run `runai recommend setup` (or set it in the dashboard Settings) \
             to pick a summary language before any summaries are generated."
        );
        return Ok(EnrichReport::default());
    }
    let api_key = if cfg.provider == Provider::ClaudeCli {
        String::new()
    } else {
        cfg.effective_api_key()
            .context("enrich: api_key not configured — run `runai recommend setup` first")?
    };

    let existing = mgr
        .db()
        .skill_ai_index_all_by_resource_key()
        .unwrap_or_default();
    let resources = enrich_candidates(mgr.db())?;
    let only_set: Option<std::collections::HashSet<String>> =
        only_names.map(|v| v.iter().cloned().collect());
    let skills: Vec<_> = resources
        .into_iter()
        .filter(|r| r.kind == ResourceKind::Skill)
        .filter(|r| match &only_set {
            Some(set) => set.contains(&r.name),
            None => true,
        })
        .collect();

    // Plan the work first: decide for each skill whether it needs enriching.
    // When only_names is given the caller is signalling "this skill just
    // changed, regenerate regardless of freshness" — mode is overridden to
    // Force for that targeted subset.
    let effective_mode = if only_set.is_some() {
        EnrichMode::Force
    } else {
        mode
    };
    let mut report = EnrichReport::default();
    let mut jobs: Vec<EnrichJob> = Vec::new();
    for r in &skills {
        let skill_md = enrich_skill_md_path(r);
        if !skill_md.exists() {
            report.skipped_no_skill_md += 1;
            continue;
        }
        let index_key = Database::skill_ai_index_key_for_resource(r);
        let has_summary = existing.contains_key(&index_key);
        let existing_row = existing.get(&index_key);
        let current_skill_md = fs::read_to_string(&skill_md).unwrap_or_default();
        let current_source_hash = sha256_hex(&[
            r.name.as_bytes(),
            b"\0",
            r.description.as_bytes(),
            b"\0",
            current_skill_md.as_bytes(),
        ]);
        let current_prompt_hash = sha256_hex(&[
            cfg.summary_lang.trim().as_bytes(),
            b"\0",
            prompt_layout_key(&cfg.summary_lang).as_bytes(),
        ]);
        let is_stale = existing_row
            .map(|row| {
                row.source_hash != current_source_hash || row.prompt_hash != current_prompt_hash
            })
            .unwrap_or(false);
        let should_process = match effective_mode {
            EnrichMode::Force => true,
            EnrichMode::Stale => !has_summary || is_stale,
            EnrichMode::MissingOnly => !has_summary,
        };
        if !should_process {
            report.skipped_have_summary += 1;
            continue;
        }
        jobs.push(EnrichJob {
            name: r.name.clone(),
            description: r.description.clone(),
            owner_user_id: r.owner_user_id.clone(),
            skill_md_path: skill_md,
            has_summary,
        });
    }
    if let Some(n) = limit {
        jobs.truncate(n);
    }
    if jobs.is_empty() {
        return Ok(report);
    }

    let total = jobs.len();
    let workers = concurrency.max(1).min(total);
    let queue = std::sync::Arc::new(std::sync::Mutex::new(jobs.into_iter()));
    let report_mu = std::sync::Arc::new(std::sync::Mutex::new(report));
    let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let db_path = mgr.paths().db_path();

    std::thread::scope(|s| {
        for _ in 0..workers {
            let queue = std::sync::Arc::clone(&queue);
            let report_mu = std::sync::Arc::clone(&report_mu);
            let progress = std::sync::Arc::clone(&progress);
            let cfg = cfg.clone();
            let api_key = api_key.clone();
            let db_path = db_path.clone();
            s.spawn(move || {
                // Each worker opens its own DB connection. rusqlite Connection
                // is !Sync so it can't be shared between threads — SQLite's
                // WAL mode handles concurrent writers fine.
                let db = match crate::core::db::Database::open(&db_path) {
                    Ok(d) => d,
                    Err(e) => {
                        let mut rp = report_mu.lock().unwrap();
                        rp.errors.push(("<db-open>".into(), e.to_string()));
                        return;
                    }
                };
                loop {
                    let job = {
                        let mut q = queue.lock().unwrap();
                        q.next()
                    };
                    let job = match job {
                        Some(j) => j,
                        None => break,
                    };
                    let body = match fs::read_to_string(&job.skill_md_path) {
                        Ok(s) => s,
                        Err(_) => {
                            let mut rp = report_mu.lock().unwrap();
                            rp.skipped_no_skill_md += 1;
                            continue;
                        }
                    };
                    // Pass the WHOLE SKILL.md (no cap). Summary quality
                    // drives router recall directly — seeing all triggers /
                    // examples / edge cases is worth the token cost.
                    // DeepSeek v4-flash 128k context handles even 90KB
                    // files trivially.
                    let user_msg = build_enrich_prompt(
                        &job.name,
                        &job.description,
                        &body,
                        &cfg.summary_lang,
                        None,
                    );

                    let result = call_summary_llm(&cfg, &api_key, &user_msg);
                    let done = progress.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if verbose {
                        eprintln!("[enrich {done}/{total}] {}", job.name);
                    } else {
                        // Lightweight default progress: print every 10 or
                        // last item so the user sees movement.
                        if done == 1 || done.is_multiple_of(10) || done == total {
                            eprintln!("[enrich] {done}/{total}");
                        }
                    }
                    let raw = match result {
                        Ok(r) => r,
                        Err(e) => {
                            let mut rp = report_mu.lock().unwrap();
                            rp.errors.push((job.name.clone(), e.to_string()));
                            continue;
                        }
                    };
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        let mut rp = report_mu.lock().unwrap();
                        rp.errors
                            .push((job.name.clone(), "empty summary returned".into()));
                        continue;
                    }
                    let (mut summary_clean, mut llm_score) = parse_enrich_response(trimmed);
                    if summary_clean.is_empty() {
                        let mut rp = report_mu.lock().unwrap();
                        rp.errors.push((
                            job.name.clone(),
                            "no usable summary lines in response".into(),
                        ));
                        continue;
                    }
                    // Language enforcement: the prompt requests `summary_lang`,
                    // but weak models leak the SKILL.md's source language. If
                    // the prose fields don't match, retry ONCE with a loud
                    // correction; if it still doesn't match, record an error
                    // and write NOTHING rather than poison the index with a
                    // wrong-language summary.
                    if !summary_matches_lang(&summary_clean, &cfg.summary_lang) {
                        let correction = format!(
                            "你上一次把 task / inputs / outputs / not-for 写成了错误的语言。\
                             必须严格用 `{}` 重写这些字段（仅专有名词可保留原文）。",
                            cfg.summary_lang.trim()
                        );
                        let retry_msg = build_enrich_prompt(
                            &job.name,
                            &job.description,
                            &body,
                            &cfg.summary_lang,
                            Some(&correction),
                        );
                        match call_summary_llm(&cfg, &api_key, &retry_msg) {
                            Ok(raw2) => {
                                let (s2, sc2) = parse_enrich_response(raw2.trim());
                                if !s2.is_empty() && summary_matches_lang(&s2, &cfg.summary_lang) {
                                    summary_clean = s2;
                                    llm_score = sc2;
                                } else {
                                    let mut rp = report_mu.lock().unwrap();
                                    rp.errors.push((
                                        job.name.clone(),
                                        format!(
                                            "language enforcement failed after retry (want `{}`)",
                                            cfg.summary_lang.trim()
                                        ),
                                    ));
                                    continue;
                                }
                            }
                            Err(e) => {
                                let mut rp = report_mu.lock().unwrap();
                                rp.errors.push((
                                    job.name.clone(),
                                    format!("lang-retry call failed: {e}"),
                                ));
                                continue;
                            }
                        }
                    }
                    let capped: String = summary_clean.chars().take(600).collect();
                    let index = make_index(
                        &job.name,
                        &job.description,
                        &capped,
                        llm_score,
                        &body,
                        &cfg.summary_lang,
                    );
                    match db.set_skill_ai_index_scoped(
                        &job.name,
                        job.owner_user_id.as_deref(),
                        &index,
                    ) {
                        Ok(()) => {
                            let mut rp = report_mu.lock().unwrap();
                            if job.has_summary {
                                rp.refreshed_stale += 1;
                            } else {
                                rp.generated += 1;
                            }
                        }
                        Err(e) => {
                            let mut rp = report_mu.lock().unwrap();
                            rp.errors.push((job.name.clone(), e.to_string()));
                        }
                    }
                }
            });
        }
    });

    let final_report = std::sync::Arc::try_unwrap(report_mu)
        .map(|m| m.into_inner().unwrap())
        .unwrap_or_else(|arc| arc.lock().unwrap().clone());
    Ok(final_report)
}

/// Outcome of `reevaluate_skill`: before/after llm_score + new summary len.
#[derive(Debug, Clone)]
pub struct FeedbackReport {
    pub old_score: i64,
    pub new_score: i64,
    pub new_summary_len: usize,
}

/// Re-run the enrich pass for a single skill with explicit user feedback
/// mixed into the prompt. Lets the main Claude agent close the loop:
/// "skill X turned out unhelpful for prompt Y" → router LLM rewrites
/// summary + adjusts llm_score (lowering it so future routing avoids X
/// for prompts of that shape).
pub fn reevaluate_skill(
    mgr: &SkillManager,
    skill_name: &str,
    feedback_note: &str,
) -> Result<FeedbackReport> {
    let cfg = RecommendConfig::load(mgr.paths())?;
    if !cfg.enabled {
        bail!("runai recommend not configured — run `runai recommend setup` first");
    }
    let api_key = if cfg.provider == Provider::ClaudeCli {
        String::new()
    } else {
        cfg.effective_api_key()
            .context("feedback: api_key not configured")?
    };
    if feedback_note.trim().is_empty() {
        bail!("--note is empty; pass concrete feedback text");
    }

    let resources = enrich_candidates(mgr.db())?;
    let resource = resources
        .into_iter()
        .find(|r| r.kind == ResourceKind::Skill && r.name == skill_name)
        .ok_or_else(|| anyhow::anyhow!("skill not found: {skill_name}"))?;
    let skill_md_path = enrich_skill_md_path(&resource);
    let skill_md_body = fs::read_to_string(&skill_md_path)
        .with_context(|| format!("read {}", skill_md_path.display()))?;

    let old_index = mgr
        .db()
        .skill_ai_index_for_resource(&resource)
        .unwrap_or_default()
        .unwrap_or_default();
    let old_summary = old_index.summary.clone();
    let old_score = old_index.llm_score;

    let user_msg = build_feedback_prompt(
        &resource.name,
        &resource.description,
        &skill_md_body,
        &old_summary,
        old_score,
        feedback_note,
        &cfg.summary_lang,
    );
    let raw = call_summary_llm(&cfg, &api_key, &user_msg)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("LLM returned empty response");
    }
    let (summary_clean, new_score) = parse_enrich_response(trimmed);
    if summary_clean.is_empty() {
        bail!("no usable summary in response: {trimmed:?}");
    }
    // Same language contract as the batch enrich path: never overwrite a
    // summary with one that leaked into the wrong language. Interactive
    // single-skill call, so bail (no silent retry) and let the caller react.
    if !summary_matches_lang(&summary_clean, &cfg.summary_lang) {
        bail!(
            "summary came back in the wrong language (want `{}`); not saving. \
             Re-run, or switch to a stronger model.",
            cfg.summary_lang.trim()
        );
    }
    let capped: String = summary_clean.chars().take(600).collect();
    let index = make_index(
        &resource.name,
        &resource.description,
        &capped,
        new_score,
        &skill_md_body,
        &cfg.summary_lang,
    );
    mgr.db().set_skill_ai_index_scoped(
        &resource.name,
        resource.owner_user_id.as_deref(),
        &index,
    )?;
    Ok(FeedbackReport {
        old_score,
        new_score,
        new_summary_len: capped.chars().count(),
    })
}

/// Pull `score: NN` out of the enrich-LLM response and return (summary_lines_only, score).
/// summary_lines_only strips the score line so the BM25 doc text doesn't carry numeric noise.
/// Falls back to llm_score=50 when the line is missing or unparseable.
fn parse_enrich_response(raw: &str) -> (String, i64) {
    let mut score: Option<i64> = None;
    let mut kept: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let lower = line.trim_start().to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("score:") {
            // Extract the first integer in the rest of the line.
            let digits: String = rest
                .chars()
                .skip_while(|c| !c.is_ascii_digit() && *c != '-')
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            if let Ok(n) = digits.parse::<i64>() {
                score = Some(n.clamp(0, 10));
            }
            continue;
        }
        kept.push(line);
    }
    let cleaned = kept.join("\n").trim().to_string();
    // Sanity check: a valid summary must contain a `task:` line (the first
    // required field in the prompt format). When the LLM gets confused and
    // emits router-style output like "EXCLUSIVE\nreview", the cleaned text
    // has none of our expected fields — return empty to make the caller
    // treat it as an error rather than writing garbage to DB.
    let has_task_line = cleaned
        .lines()
        .any(|l| l.trim_start().to_ascii_lowercase().starts_with("task:"));
    if !has_task_line {
        return (String::new(), score.unwrap_or(5));
    }
    (cleaned, score.unwrap_or(5))
}

/// Expand a short / ambiguous user prompt into a BM25-friendly keyword
/// string for the prefilter. The LLM is asked to pull out the user's real
/// intent and pad the query with synonyms, jargon, en/zh cross-fills, and
/// verb/noun variants. Output is a single comma-separated line, no prose.
/// Returns `None` on any error (network, parse, empty) — caller falls back
/// to the raw user prompt; nothing depends on rewrite succeeding.
pub(super) fn rewrite_query_for_bm25(
    cfg: &RecommendConfig,
    api_key: &str,
    user_prompt: &str,
) -> Option<String> {
    let prompt = format!(
        "你是 BM25 检索查询扩展器。\n\n\
        任务：把下面的 user prompt 扩展成一行 BM25 检索友好的关键词列表。\n\
        - 提取用户的真实意图（不要逐字复述 prompt）\n\
        - 加同义词、行话、动词名词变体、缩写\n\
        - 中文 prompt 加英文同义词；英文 prompt 加中文等价词\n\
        - 至少 10 个关键词，多多益善\n\
        - **输出格式**：单行，逗号分隔的关键词，不要任何解释 / 标题 / 前后缀 / 引号\n\
        - 不要写句子，只写关键词\n\n\
        反例（不要这样写）：\n\
        - 'I think the user wants ...' （别解释）\n\
        - 'Keywords: a, b, c' （别写前缀）\n\
        - 多行输出\n\n\
        user prompt: {user_prompt}\n\n\
        输出（单行关键词）："
    );
    let raw = call_summary_llm(cfg, api_key, &prompt).ok()?;
    // Take only the first non-empty line; LLM sometimes adds a trailing
    // explanation despite the instructions.
    let line = raw.lines().find(|l| !l.trim().is_empty())?.trim();
    if line.is_empty() {
        return None;
    }
    // Sanity cap to bound the prefilter input.
    let capped: String = line.chars().take(800).collect();
    Some(capped)
}

#[cfg(test)]
mod owner_aware_tests {
    use super::*;
    use crate::core::db::Database;
    use crate::core::resource::Source;
    use std::collections::HashMap;

    fn mk(id: &str, name: &str, dir: &str, owner: Option<&str>) -> Resource {
        Resource {
            id: id.into(),
            name: name.into(),
            kind: ResourceKind::Skill,
            description: "d".into(),
            directory: PathBuf::from(dir),
            source: Source::Local {
                path: PathBuf::from(dir),
            },
            installed_at: 0,
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: owner.map(String::from),
            publish_status: "draft".into(),
        }
    }

    #[test]
    fn enrich_candidates_includes_private_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("t.db")).unwrap();
        db.insert_resource(&mk("local:pub", "pubskill", "/data/skills/pubskill", None))
            .unwrap();
        db.insert_resource(&mk(
            "u:usr_a:local:priv",
            "privskill",
            "/data/users/usr_a/skills/privskill",
            Some("usr_a"),
        ))
        .unwrap();
        let names: Vec<String> = enrich_candidates(&db)
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert!(
            names.contains(&"pubskill".to_string()),
            "public skill must be an enrich candidate"
        );
        assert!(
            names.contains(&"privskill".to_string()),
            "PRIVATE skill must be an enrich candidate (the bug: was public-pool only)"
        );
    }

    #[test]
    fn enrich_skill_md_path_uses_row_directory_not_public_pool() {
        let r = mk(
            "u:usr_a:local:priv",
            "privskill",
            "/data/users/usr_a/skills/privskill",
            Some("usr_a"),
        );
        assert_eq!(
            enrich_skill_md_path(&r),
            PathBuf::from("/data/users/usr_a/skills/privskill/SKILL.md"),
            "enrich must read SKILL.md from the row's own directory, not skills_dir()"
        );
    }
}

use crate::cli::command_enums::RecommendCommands;
use crate::core::manager::SkillManager;
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
enum LocalRecommendAuth {
    Anonymous,
    Authenticated(String),
    InvalidIdentity(String),
}

fn local_recommend_auth(mgr: &SkillManager) -> LocalRecommendAuth {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return LocalRecommendAuth::Anonymous,
    };
    let path = home.join(".runai-identity");
    if !path.exists() {
        return LocalRecommendAuth::Anonymous;
    }
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => {
            return LocalRecommendAuth::InvalidIdentity(format!("read {}: {e}", path.display()));
        }
    };
    let v: serde_json::Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return LocalRecommendAuth::InvalidIdentity(format!(
                "parse {} as JSON: {e}",
                path.display()
            ));
        }
    };
    let Some(api_key) = v.get("api_key").and_then(|x| x.as_str()) else {
        return LocalRecommendAuth::InvalidIdentity(format!("{} missing api_key", path.display()));
    };
    let hash = crate::core::auth::key_hash(&crate::core::auth::BearerToken(api_key.to_string()));
    match mgr.db().find_user_by_api_key_hash(&hash).ok().flatten() {
        Some(u) if !u.disabled => LocalRecommendAuth::Authenticated(u.user_id),
        Some(_) => LocalRecommendAuth::InvalidIdentity(format!(
            "{} points at a disabled user",
            path.display()
        )),
        None => LocalRecommendAuth::InvalidIdentity(format!(
            "{} api_key is not valid for this runai server",
            path.display()
        )),
    }
}

pub(in crate::cli) fn handle_recommend(
    mgr: &SkillManager,
    command: Option<RecommendCommands>,
    prompt: Option<String>,
) -> Result<()> {
    use crate::core::recommend::{Provider, RecommendConfig};

    match (command, prompt) {
        (None, prompt_opt) => {
            // Resolve user prompt + transcript path. Precedence:
            //   1. positional `prompt` arg if given
            //   2. stdin JSON (Claude Code hook protocol: {prompt, transcript_path, ...})
            // Stdin-JSON mode lets the router see recent conversation history,
            // which is how "use figma-component-mapping" replies get auto-routed
            // to the right skill on the next round.
            let (user_prompt, transcript_path, session_id, cwd) = match prompt_opt {
                Some(p) => (p, None, None, None),
                None => {
                    use std::io::Read;
                    let mut buf = String::new();
                    if std::io::stdin().read_to_string(&mut buf).is_err() || buf.trim().is_empty() {
                        anyhow::bail!(
                            "usage: runai recommend <prompt> | runai recommend setup | runai recommend status | runai recommend hook-snippet\n(or pipe Claude Code's UserPromptSubmit hook JSON via stdin)"
                        );
                    }
                    let v: serde_json::Value = serde_json::from_str(&buf)
                        .map_err(|e| anyhow::anyhow!("parse hook stdin JSON: {e}"))?;
                    let p = v
                        .get("prompt")
                        .and_then(|x| x.as_str())
                        .or_else(|| v.get("user_prompt").and_then(|x| x.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let tp = v
                        .get("transcript_path")
                        .and_then(|x| x.as_str())
                        .map(std::path::PathBuf::from);
                    let sid = v
                        .get("session_id")
                        .and_then(|x| x.as_str())
                        .map(String::from);
                    let cwd_s = v.get("cwd").and_then(|x| x.as_str()).map(String::from);
                    (p, tp, sid, cwd_s)
                }
            };

            let cfg = RecommendConfig::load(mgr.paths())?;
            // First-run guidance: if the user hasn't configured the router
            // yet, surface a one-time guide via hook stdout so the main
            // Claude can walk them through `runai recommend setup` instead
            // of silently doing nothing. We mark a `.bootstrap-seen` flag
            // file so this only fires once per machine — no nagging.
            if !cfg.enabled {
                let flag = mgr.paths().data_dir().join(".bootstrap-seen");
                let already_seen = flag.exists();
                if !already_seen {
                    let _ = std::fs::write(&flag, b"1");
                    print!("{}", crate::core::recommend::bootstrap_guide());
                }
                return Ok(());
            }
            let local_auth = local_recommend_auth(mgr);
            let user_id = match &local_auth {
                LocalRecommendAuth::Anonymous => None,
                LocalRecommendAuth::Authenticated(uid) => Some(uid.as_str()),
                LocalRecommendAuth::InvalidIdentity(msg) => {
                    eprintln!("# runai recommend skipped: {msg}");
                    return Ok(());
                }
            };

            match crate::core::recommend::recommend_for_user(
                mgr,
                &user_prompt,
                transcript_path.as_deref(),
                session_id.as_deref(),
                cwd.as_deref(),
                user_id,
            ) {
                Ok(decision) => {
                    // Re-format with the actual session_id + this session's
                    // recommendation history so the `{SESSION_ID}` placeholder
                    // in hook_pointer.md gets the real id (was empty before —
                    // `format_for_hook(&decision)` is the no-session variant).
                    // recommend() already wrote the same string into telemetry
                    // internally; we just rebuild it for stdout to avoid
                    // plumbing a return tuple through the function.
                    let sid = session_id.as_deref().unwrap_or("");
                    let history = if sid.is_empty() {
                        Vec::new()
                    } else {
                        mgr.db()
                            .router_session_recommended_skills(sid)
                            .unwrap_or_default()
                    };
                    // CLI hook path: hook output uses the unified HTTP
                    // protocol — agent will curl `${server_url}/...`.
                    // server_url uses the machine's outbound IPv4 (so the
                    // rendered URL is reachable from any process / host
                    // on the LAN, not loopback-only); falls back to
                    // 127.0.0.1 when offline. No X-Runai-User header in
                    // local mode (single user).
                    let local_server_url = crate::core::recommend::default_local_server_url();
                    let mut cfg_local = crate::core::recommend::RecommendConfig::load(mgr.paths())
                        .unwrap_or_default();
                    if let Some(uid) = user_id
                        && let Ok(Some(user)) = mgr.db().find_user_by_id(uid)
                    {
                        let p = crate::core::prefs::UserPrefs::from_json_str(&user.prefs_json);
                        cfg_local.skip_reminder_enabled = p.skip_reminder_enabled;
                        if !p.skip_reminder_template.is_empty() {
                            cfg_local.skip_reminder_template = p.skip_reminder_template;
                        }
                    }
                    let skip_reminder = if cfg_local.skip_reminder_enabled {
                        cfg_local.skip_reminder_template.as_str()
                    } else {
                        ""
                    };
                    let out = crate::core::recommend::format_for_hook_full(
                        &decision,
                        sid,
                        &history,
                        &local_server_url,
                        "",
                        skip_reminder,
                    );
                    if !out.is_empty() {
                        print!("{out}");
                    }
                }
                Err(e) => {
                    eprintln!("# runai recommend skipped: {e}");
                }
            }
            Ok(())
        }
        (Some(RecommendCommands::Setup), _) => {
            recommend_setup(mgr)?;
            Ok(())
        }
        (Some(RecommendCommands::Status), _) => {
            let cfg = RecommendConfig::load(mgr.paths())?;
            println!("enabled:        {}", cfg.enabled);
            println!(
                "provider:       {}",
                match cfg.provider {
                    Provider::OpenaiCompat => "openai-compat",
                    Provider::Anthropic => "anthropic",
                    Provider::ClaudeCli => "claude-cli",
                }
            );
            println!("base_url:       {}", cfg.base_url);
            println!("model:          {}", cfg.model);
            let key_status = if !cfg.api_key.is_empty() {
                "set in config"
            } else if std::env::var("RUNAI_RECOMMEND_API_KEY").is_ok() {
                "set via RUNAI_RECOMMEND_API_KEY"
            } else {
                "missing"
            };
            println!("api_key:        {key_status}");
            println!("top_k:          {}", cfg.top_k);
            println!("summary_lang:   {}", cfg.summary_lang);
            println!("min_prompt_len: {}", cfg.min_prompt_len);
            println!("config file:    {}", mgr.paths().config_path().display());
            Ok(())
        }
        (Some(RecommendCommands::HookSnippet), _) => {
            println!(
                r#"Add this to ~/.claude/settings.json:

{{
  "hooks": {{
    "UserPromptSubmit": [
      {{
        "hooks": [
          {{ "type": "command", "command": "runai recommend" }}
        ]
      }}
    ]
  }}
}}

Claude Code pipes the hook JSON (prompt, transcript_path, ...) to stdin.
runai recommend reads it, looks at recent conversation history, and emits
the picked SKILL.md to stdout — which Claude Code injects as additional
context for the upcoming turn.

To install/uninstall automatically (preserves existing hooks and theme):
  runai recommend install-hook
  runai recommend uninstall-hook"#
            );
            Ok(())
        }
        (Some(RecommendCommands::InstallHook), _) => {
            use crate::core::recommend::{
                HookInstallStatus, install_claude_hook, install_session_start_hook,
            };
            let home =
                dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
            let path = home.join(".claude/settings.json");
            // Also install a SessionStart hook that runs `runai recommend
            // enrich --missing-only` so newly installed / edited skills get
            // AI summaries automatically the next time Claude Code starts.
            // It's idempotent + fire-and-forget (the enrich pass is itself
            // a no-op when nothing is missing/stale).
            let enrich_cmd = "runai recommend enrich --missing-only";
            let _ = install_session_start_hook(&home, enrich_cmd);
            match install_claude_hook(&home)? {
                HookInstallStatus::Installed => {
                    println!("hook installed into {}", path.display());
                    println!("  + SessionStart enrich auto-trigger: {enrich_cmd}");
                    println!(
                        "backup of prior contents (if any): {}.runai-bak",
                        path.display()
                    );
                }
                HookInstallStatus::AlreadyPresent => {
                    println!("hook already present in {}, no changes", path.display());
                }
                _ => {}
            }
            // If the router isn't configured yet, surface a follow-up so the
            // assistant that just ran install-hook keeps walking the user
            // through `runai recommend setup` instead of stopping here.
            let cfg = RecommendConfig::load(mgr.paths()).unwrap_or_default();
            if !cfg.enabled {
                println!();
                println!("next step: router is not configured yet — `enabled = false`.");
                println!("  run `runai recommend setup` to pick a provider + paste an API key.");
                println!(
                    "  after setup the router auto-enriches all skills and starts routing on the next prompt."
                );
            }
            Ok(())
        }
        (Some(RecommendCommands::Stats { hours, recent }), _) => {
            let since_ts = hours.map(|h| chrono::Utc::now().timestamp() - h * 3600);
            let summary = mgr.db().router_stats_summary(since_ts)?;
            let window_label = match hours {
                Some(h) => format!("last {h}h"),
                None => "all-time".to_string(),
            };
            println!("Router LLM telemetry ({window_label})");
            println!("  total calls:          {}", summary.total_calls);
            println!("  errors:               {}", summary.errors);
            if let Some(ms) = summary.avg_latency_ms {
                println!("  avg latency (ok):     {ms:.0} ms");
            }
            println!("  prompt tokens:        {}", summary.total_prompt_tokens);
            println!(
                "  completion tokens:    {}",
                summary.total_completion_tokens
            );
            println!("  reasoning tokens:     {}", summary.total_reasoning_tokens);
            println!("  total tokens:         {}", summary.total_tokens);
            if !summary.per_model.is_empty() {
                println!("\n  per model:");
                for m in &summary.per_model {
                    println!(
                        "    {:<30} {:>6} calls  {:>10} tokens",
                        m.model, m.calls, m.total_tokens
                    );
                }
            }
            if recent > 0 {
                let events = mgr.db().router_recent_events(recent)?;
                println!("\n  recent calls (newest first):");
                for ev in &events {
                    let when = chrono::DateTime::<chrono::Utc>::from_timestamp(ev.ts, 0)
                        .map(|d| {
                            d.with_timezone(&chrono::Local)
                                .format("%m-%d %H:%M:%S")
                                .to_string()
                        })
                        .unwrap_or_default();
                    println!(
                        "    {when}  {:<22}  {:>5}t  {:>5}ms  {}",
                        ev.model, ev.total_tokens, ev.latency_ms, ev.chosen_skills_json
                    );
                }
            }
            Ok(())
        }
        (Some(RecommendCommands::UninstallHook), _) => {
            use crate::core::recommend::{HookInstallStatus, uninstall_claude_hook};
            let home =
                dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
            let path = home.join(".claude/settings.json");
            match uninstall_claude_hook(&home)? {
                HookInstallStatus::Removed => {
                    println!("hook removed from {}", path.display());
                }
                HookInstallStatus::NotPresent => {
                    println!("hook not present in {}, no changes", path.display());
                }
                _ => {}
            }
            Ok(())
        }
        (Some(RecommendCommands::Feedback { skill, note }), _) => {
            let report = crate::core::recommend::reevaluate_skill(mgr, &skill, &note)?;
            println!(
                "feedback applied to {skill}\n  llm_score: {} → {}\n  summary updated: {} chars",
                report.old_score, report.new_score, report.new_summary_len
            );
            Ok(())
        }
        (Some(RecommendCommands::Get { skill }), _) => {
            let path = mgr.paths().skills_dir().join(&skill).join("SKILL.md");
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("runai recommend get: cannot read {}: {e}", path.display());
                    std::process::exit(2);
                }
            };
            // Atomic: read succeeded → record adoption.
            let _ = mgr.record_usage(&skill);
            let sid = std::env::var("CLAUDE_SESSION_ID").unwrap_or_default();
            if !sid.is_empty() {
                let _ = mgr.db().record_session_adoption(&sid, &skill);
            }
            // Print path on stderr (debug visibility) + full SKILL.md body on
            // stdout so the main agent gets the content directly.
            eprintln!("# skill: {skill}");
            eprintln!("# path: {}", path.display());
            eprintln!("# usage_count +1 recorded");
            print!("{content}");
            Ok(())
        }
        (Some(RecommendCommands::ResetScoring { yes }), _) => {
            if !yes {
                use std::io::{BufRead, Write};
                print!("about to wipe all LLM summaries. continue? [y/N] ");
                std::io::stdout().flush().ok();
                let stdin = std::io::stdin();
                let line = stdin.lock().lines().next().transpose()?.unwrap_or_default();
                if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    println!("aborted");
                    return Ok(());
                }
            }
            let s = mgr.db().reset_summaries()?;
            println!("deleted: {s} summaries");
            Ok(())
        }
        (
            Some(RecommendCommands::Enrich {
                limit,
                force,
                missing_only,
                fix_lang,
                verbose,
                concurrency,
                names,
            }),
            _,
        ) => {
            use crate::core::recommend::EnrichMode;
            let mode = if force {
                EnrichMode::Force
            } else if missing_only {
                EnrichMode::MissingOnly
            } else {
                EnrichMode::Stale
            };
            // `--fix-lang` overrides the name set with exactly the skills whose
            // stored summary leaked the wrong language. only_names forces a
            // re-enrich of that subset (and nothing else). If nothing is
            // mismatched, stop here — falling through with an empty set would
            // mean "no name filter" and re-enrich everything stale.
            let names = if fix_lang {
                let mismatched = crate::core::recommend::find_language_mismatched_skills(mgr)?;
                if mismatched.is_empty() {
                    println!("--fix-lang: no language-mismatched summaries found, nothing to do");
                    return Ok(());
                }
                println!(
                    "--fix-lang: {} skill(s) have wrong-language summaries, re-enriching them",
                    mismatched.len()
                );
                mismatched
            } else {
                names
            };
            let (have, _oldest, _newest) =
                mgr.db().skill_ai_summary_stats().unwrap_or((0, None, None));
            let names_label = if names.is_empty() {
                "all".to_string()
            } else {
                format!("only={:?}", names)
            };
            println!(
                "enriching skill summaries (currently {have} have summaries)\n\
                 limit={} mode={:?} concurrency={concurrency} {names_label}",
                limit.map(|n| n.to_string()).unwrap_or_else(|| "all".into()),
                mode,
            );
            let only_names: Option<&[String]> = if names.is_empty() {
                None
            } else {
                Some(&names[..])
            };
            let report = crate::core::recommend::enrich_skills(
                mgr,
                limit,
                mode,
                verbose,
                concurrency,
                only_names,
            )?;
            println!(
                "\nenrichment done:\n  generated:           {}\n  refreshed (stale):   {}\n  skipped (up-to-date): {}\n  skipped (no SKILL.md): {}\n  errors:              {}",
                report.generated,
                report.refreshed_stale,
                report.skipped_have_summary,
                report.skipped_no_skill_md,
                report.errors.len()
            );
            for (name, msg) in report.errors.iter().take(10) {
                println!("    {name}: {msg}");
            }
            if report.errors.len() > 10 {
                println!("    ... +{} more", report.errors.len() - 10);
            }
            Ok(())
        }
    }
}

fn recommend_setup(mgr: &SkillManager) -> Result<()> {
    use crate::core::recommend::{Provider, RecommendConfig};
    use std::io::{BufRead, Write};

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut lock = stdin.lock();

    let mut cur = RecommendConfig::load(mgr.paths()).unwrap_or_default();

    let ask = |prompt: &str, default: &str, lock: &mut std::io::StdinLock<'_>| -> Result<String> {
        print!("{prompt} [{default}]: ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        lock.read_line(&mut line)?;
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() {
            Ok(default.to_string())
        } else {
            Ok(trimmed)
        }
    };

    writeln!(
        stdout,
        "runai recommend setup\n\
         留空回车保留默认。Provider 选 openai-compat（DeepSeek / Moonshot / Groq 等）或 anthropic。"
    )?;

    let provider_str = ask(
        "provider (openai-compat / anthropic / claude-cli)",
        match cur.provider {
            Provider::OpenaiCompat => "openai-compat",
            Provider::Anthropic => "anthropic",
            Provider::ClaudeCli => "claude-cli",
        },
        &mut lock,
    )?;
    cur.provider = match provider_str.as_str() {
        "anthropic" => Provider::Anthropic,
        "claude-cli" => Provider::ClaudeCli,
        _ => Provider::OpenaiCompat,
    };

    // claude-cli reuses the user's Claude Code session; no base_url / api_key
    // needed. Skip those prompts.
    if cur.provider != Provider::ClaudeCli {
        let default_base = match cur.provider {
            Provider::OpenaiCompat => {
                if cur.base_url.is_empty() {
                    "https://api.deepseek.com/v1"
                } else {
                    cur.base_url.as_str()
                }
            }
            Provider::Anthropic => {
                if cur.base_url.is_empty() || cur.base_url.contains("deepseek") {
                    "https://api.anthropic.com"
                } else {
                    cur.base_url.as_str()
                }
            }
            Provider::ClaudeCli => unreachable!(),
        };
        cur.base_url = ask("base_url", default_base, &mut lock)?;
    } else {
        cur.base_url = String::new();
    }

    let default_model = match cur.provider {
        Provider::OpenaiCompat => "deepseek-v4-flash",
        Provider::Anthropic => "claude-haiku-4-5-20251001",
        Provider::ClaudeCli => "haiku",
    };
    let model_default = if cur.model.is_empty() {
        default_model
    } else {
        cur.model.as_str()
    };
    cur.model = ask("model", model_default, &mut lock)?;

    if cur.provider != Provider::ClaudeCli {
        print!("api_key (input hidden? no — paste then enter): ");
        stdout.flush()?;
        let mut key_line = String::new();
        lock.read_line(&mut key_line)?;
        let key_trimmed = key_line.trim().to_string();
        if !key_trimmed.is_empty() {
            cur.api_key = key_trimmed;
        }
    } else {
        cur.api_key = String::new();
    }

    // Ask the user which language to write skill summaries in. Matching the
    // daily chat language gives the best BM25 recall — the summary is what
    // the router queries against, so keyword overlap matters.
    writeln!(stdout)?;
    writeln!(
        stdout,
        "summary_lang: AI summary 用什么语言写? (按你日常对话的主语言选，BM25 检索靠它命中)\n\
         可选: zh / en / ja / bilingual / 或自定义字符串 (例: '中文 + 英文关键词')"
    )?;
    let lang_default = if cur.summary_lang.is_empty() {
        "zh"
    } else {
        cur.summary_lang.as_str()
    };
    cur.summary_lang = ask("summary_lang", lang_default, &mut lock)?;
    // The user just deliberately chose a summary language — release the
    // enrich gate. Without this flag, `enrich` refuses to generate any
    // summary (prevents the mixed-language index from a defaulted language).
    cur.summary_lang_confirmed = true;

    cur.enabled = true;
    cur.save(mgr.paths())?;
    println!(
        "\nSaved to {}\nenabled=true. To wire the hook, run:\n  runai recommend hook-snippet",
        mgr.paths().config_path().display()
    );

    // Auto-trigger background enrichment for any skill that doesn't have an
    // AI summary yet. First-run UX: setup finishes immediately, summaries
    // populate over the next few minutes in the background. Dashboard shows
    // progress under /skills (enriched / total). Idempotent — re-running
    // setup later is a no-op when nothing is missing.
    let (already_have, _, _) = mgr.db().skill_ai_summary_stats().unwrap_or((0, None, None));
    let total_skills = mgr
        .list_resources(None, None)
        .map(|rs| {
            rs.iter()
                .filter(|r| r.kind == crate::core::resource::ResourceKind::Skill)
                .count()
        })
        .unwrap_or(0);
    let missing = total_skills.saturating_sub(already_have as usize);
    if missing > 0
        && let Ok(exe) = std::env::current_exe()
    {
        let spawn = std::process::Command::new(exe)
            .arg("recommend")
            .arg("enrich")
            .arg("--missing-only")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match spawn {
            Ok(_) => {
                println!(
                    "\nspawned background enrich for {missing} skills missing AI summary.\n  follow progress at http://127.0.0.1:17888/#/skills"
                );
            }
            Err(e) => {
                eprintln!("(warn) could not spawn background enrich: {e}");
                eprintln!("       run manually: runai recommend enrich --missing-only");
            }
        }
    }
    Ok(())
}

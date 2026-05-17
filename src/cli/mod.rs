use crate::core::cli_target::CliTarget;
use crate::core::group::{Group, GroupKind, GroupMember, MemberType};
use crate::core::manager::SkillManager;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "runai",
    version,
    about = "AI CLI resource manager for skills and MCP servers"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Scan CLI directories and adopt unmanaged skills
    Scan,
    /// Discover all SKILL.md files on disk (fast recursive search)
    Discover {
        /// Root directory to search (default: home directory)
        #[arg(long)]
        root: Option<String>,
    },
    /// List resources
    List {
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        target: Option<String>,
    },
    /// Enable a resource or group
    Enable {
        name: String,
        #[arg(long, default_value = "claude")]
        target: String,
    },
    /// Disable a resource or group
    Disable {
        name: String,
        #[arg(long, default_value = "claude")]
        target: String,
    },
    /// Install a skill from GitHub
    Install { source: String },
    /// Install a skill from market
    MarketInstall {
        name: String,
        #[arg(long)]
        source: Option<String>,
    },
    /// Uninstall a resource
    Uninstall { name: String },
    /// Trash management
    Trash {
        #[command(subcommand)]
        command: TrashCommands,
    },
    /// Restore from backup (uses latest backup by default)
    Restore {
        /// Backup timestamp (omit for latest)
        #[arg(long)]
        timestamp: Option<String>,
    },
    /// Create a backup now
    Backup,
    /// List available backups (newest first)
    Backups,
    /// Search across installed resources and market
    Search { query: String },
    /// Browse market skills
    Market {
        /// Filter by source label or repo
        #[arg(long)]
        source: Option<String>,
        /// Search keyword in name/repo path/source label
        #[arg(long)]
        search: Option<String>,
    },
    /// Group management
    Group {
        #[command(subcommand)]
        command: GroupCommands,
    },
    /// Show status summary
    Status {
        #[arg(long, default_value = "claude")]
        target: String,
    },
    /// Start MCP server (stdio)
    McpServe,
    /// Start HTTP dashboard for router telemetry on localhost
    Server {
        /// Port to bind (default: 17888)
        #[arg(long, default_value_t = 17888)]
        port: u16,
        /// Host to bind (default: 127.0.0.1 — localhost only).
        /// Use 0.0.0.0 to expose on LAN, but note the DB contains user prompts.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Idempotent "ensure-running": exit immediately if the port is
        /// already serving; otherwise spawn the server as a detached
        /// background process and return. Designed for SessionStart / shell
        /// rc auto-launch — call it every session, it stays cheap.
        #[arg(long)]
        ensure: bool,
        /// Install a SessionStart hook in ~/.claude/settings.json that runs
        /// `runai server --ensure --port <port>` on every new Claude Code
        /// session so the dashboard auto-launches. Idempotent.
        #[arg(long, conflicts_with = "uninstall_hook")]
        install_hook: bool,
        /// Remove the SessionStart hook installed by `--install-hook`.
        #[arg(long)]
        uninstall_hook: bool,
    },
    /// Register runai as MCP server in all CLI configs
    Register,
    /// Unregister runai from all CLI configs
    Unregister,
    /// Show usage statistics (most used skills/MCPs)
    Usage {
        /// Show only top N entries
        #[arg(long)]
        top: Option<usize>,
    },
    /// Update runai to the latest version
    Update,
    /// Run health checks on runai installation
    Doctor {
        /// Repair what can be repaired automatically: prune dangling
        /// `~/.{claude,codex,gemini,opencode}/skills/` symlinks and re-run
        /// the skill-row dedupe pass.
        #[arg(long)]
        fix: bool,
    },
    /// LLM-driven skill router (off by default; run `runai recommend setup`).
    Recommend {
        #[command(subcommand)]
        command: Option<RecommendCommands>,
        /// Run router for the given prompt (positional, no subcommand)
        prompt: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RecommendCommands {
    /// Interactive setup: pick provider, paste API key, write ~/.runai/config.toml
    Setup,
    /// Show current router config (api_key redacted)
    Status,
    /// Print the hook JSON snippet to drop into ~/.claude/settings.json
    HookSnippet,
    /// Install the UserPromptSubmit hook into ~/.claude/settings.json (idempotent; backs up the old file)
    InstallHook,
    /// Remove the runai-installed UserPromptSubmit hook from ~/.claude/settings.json
    UninstallHook,
    /// Show router LLM usage telemetry: tokens per model, latency, recent calls
    Stats {
        /// Only count events in the last N hours (omit for all-time)
        #[arg(long)]
        hours: Option<i64>,
        /// Also print the N most recent calls
        #[arg(long, default_value = "0")]
        recent: usize,
    },
    /// Mine implicit feedback signals out of router_events: when a user
    /// next-prompt in the same session contains negative cues ("不对", "换一个",
    /// "wrong") about a prior chosen skill, lower that skill's auto rating;
    /// positive cues ("可以", "完美") raise it. Auto ratings never overwrite
    /// manual ratings entered via the dashboard. Token cost: 0.
    MineFeedback {
        /// Look at events from the last N hours (default 168 = 7 days).
        #[arg(long, default_value_t = 168)]
        hours: i64,
    },
    /// Wipe LLM summaries / scores and/or user ratings. By default clears
    /// BOTH. Use `--only summaries` or `--only ratings` to narrow scope.
    ResetScoring {
        /// "summaries" | "ratings" | "all" (default)
        #[arg(long, default_value = "all")]
        only: String,
        /// Skip the "are you sure" prompt (for scripts / hooks)
        #[arg(long)]
        yes: bool,
    },
    /// Generate bilingual AI summaries for skills (improves BM25 prefilter
    /// recall, especially for cross-language queries). Default mode picks up
    /// missing summaries AND re-enriches skills whose SKILL.md mtime is
    /// newer than the stored summary's timestamp.
    Enrich {
        /// Process at most N skills this run (omit for all that need it)
        #[arg(long)]
        limit: Option<usize>,
        /// Regenerate every skill's summary, ignoring mtime/exists checks
        #[arg(long, conflicts_with = "missing_only")]
        force: bool,
        /// Only enrich skills that have NO summary yet — skip stale-mtime
        /// refresh (cheapest mode, for "first launch / new install" use)
        #[arg(long, conflicts_with = "force")]
        missing_only: bool,
        /// Print per-skill progress
        #[arg(long)]
        verbose: bool,
    },
}

#[derive(Subcommand)]
pub enum GroupCommands {
    /// Create a new group
    Create {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "custom")]
        kind: String,
    },
    /// Add a resource to a group
    Add {
        group: String,
        resource: String,
        #[arg(long, default_value = "skill")]
        resource_type: String,
    },
    /// Remove a resource from a group
    Remove { group: String, resource: String },
    /// List all groups
    List,
    /// Delete a group (does not delete its members)
    Delete { id: String },
    /// Update group metadata (display name and/or description)
    Update {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Show one group's full details (description + members)
    Show { id: String },
}

#[derive(Subcommand)]
pub enum TrashCommands {
    /// List trash entries
    List,
    /// Restore a trashed resource by trash ID or resource name
    Restore { query: String },
    /// Permanently delete a trashed resource by trash ID or resource name
    Purge { query: String },
    /// Permanently delete everything in trash
    Empty,
}

pub fn run(cli: Cli) -> Result<()> {
    let mgr = if let Ok(dir) =
        std::env::var("RUNE_DATA_DIR").or_else(|_| std::env::var("SKILL_MANAGER_DATA_DIR"))
    {
        SkillManager::with_base(std::path::PathBuf::from(dir))?
    } else {
        SkillManager::new()?
    };

    match cli.command {
        None => {
            crate::tui::run_tui(mgr)?;
            Ok(())
        }
        Some(Commands::Scan) => {
            let result = mgr.scan()?;
            println!(
                "Scan complete: {} adopted, {} skipped, {} errors",
                result.adopted,
                result.skipped,
                result.errors.len()
            );
            for err in &result.errors {
                eprintln!("  error: {err}");
            }
            Ok(())
        }
        Some(Commands::Discover { root }) => {
            use crate::core::scanner::SkillStatus;
            let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
            let search_root = root.map(std::path::PathBuf::from).unwrap_or(home);
            println!("Scanning {}...", search_root.display());
            let start = std::time::Instant::now();
            let found = crate::core::scanner::Scanner::discover_skills(&search_root);
            let elapsed = start.elapsed();

            let managed = found
                .iter()
                .filter(|s| s.status == SkillStatus::Managed)
                .count();
            let cli = found
                .iter()
                .filter(|s| s.status == SkillStatus::CliDir)
                .count();
            let unmanaged = found
                .iter()
                .filter(|s| s.status == SkillStatus::Unmanaged)
                .count();

            println!(
                "Found {} skills in {:.1}s ({managed} managed, {cli} CLI, {unmanaged} unmanaged)\n",
                found.len(),
                elapsed.as_secs_f64()
            );

            for s in &found {
                let tag = match s.status {
                    SkillStatus::Managed => "●",
                    SkillStatus::CliDir => "◆",
                    SkillStatus::Unmanaged => "○",
                };
                println!("  {tag} {:<40} {}", s.name, s.path.display());
            }
            Ok(())
        }
        Some(Commands::List {
            group,
            kind,
            target,
        }) => {
            let kind_filter = kind.as_deref().and_then(|k| k.parse().ok());
            let target_filter = target.as_deref().and_then(|t| t.parse().ok());

            let resources = if let Some(group_id) = &group {
                mgr.db().get_group_members(group_id)?
            } else {
                mgr.list_resources(kind_filter, target_filter)?
            };

            if resources.is_empty() {
                println!("No resources found.");
            } else {
                for r in &resources {
                    let enabled_targets: Vec<&str> = CliTarget::ALL
                        .iter()
                        .filter(|t| r.is_enabled_for(**t))
                        .map(|t| t.name())
                        .collect();
                    let enabled_str = if enabled_targets.is_empty() {
                        "disabled".to_string()
                    } else {
                        enabled_targets.join(", ")
                    };
                    let kind_badge = r.kind.as_str();
                    let desc: String = r.description.chars().take(60).collect();
                    println!("  [{kind_badge}] {} — {desc} [{enabled_str}]", r.name);
                }
                println!("\nTotal: {} resources", resources.len());
            }
            Ok(())
        }
        Some(Commands::Enable { name, target }) => {
            let target = target
                .parse::<CliTarget>()
                .map_err(|_| anyhow::anyhow!("unknown target: {target}"))?;
            let groups = mgr.list_groups()?;
            if groups.iter().any(|(id, _)| id == &name) {
                mgr.enable_group(&name, target, None)?;
                println!("Group '{name}' enabled for {target}");
            } else {
                let resource_id = find_resource_id_by_name(&mgr, &name)?;
                mgr.enable_resource(&resource_id, target, None)?;
                println!("Resource '{name}' enabled for {target}");
            }
            Ok(())
        }
        Some(Commands::Disable { name, target }) => {
            let target = target
                .parse::<CliTarget>()
                .map_err(|_| anyhow::anyhow!("unknown target: {target}"))?;
            let groups = mgr.list_groups()?;
            if groups.iter().any(|(id, _)| id == &name) {
                mgr.disable_group(&name, target, None)?;
                println!("Group '{name}' disabled for {target}");
            } else {
                let resource_id = find_resource_id_by_name(&mgr, &name)?;
                mgr.disable_resource(&resource_id, target, None)?;
                println!("Resource '{name}' disabled for {target}");
            }
            Ok(())
        }
        Some(Commands::Install { source }) => {
            let input = source
                .trim()
                .trim_start_matches("https://github.com/")
                .trim_end_matches('/');
            let (repo_part, branch) = if input.contains('@') {
                let parts: Vec<&str> = input.splitn(2, '@').collect();
                (parts[0], parts[1].to_string())
            } else {
                (input, "main".to_string())
            };
            let parts: Vec<&str> = repo_part.splitn(2, '/').collect();
            if parts.len() != 2 {
                anyhow::bail!("Invalid format. Use: owner/repo or owner/repo@branch");
            }
            let target = CliTarget::Claude;
            println!("Installing from {}/{}@{branch}...", parts[0], parts[1]);
            let (group_id, names) = mgr.install_github_repo(parts[0], parts[1], &branch, target)?;
            println!("Installed {} skills, group '{group_id}':", names.len());
            for name in &names {
                println!("  {name}");
            }
            Ok(())
        }
        Some(Commands::MarketInstall { name, source }) => {
            let data_dir = mgr.paths().data_dir().to_path_buf();
            let sources = crate::core::market::load_sources(&data_dir);
            let skill = crate::core::market::find_skill_in_sources(
                &data_dir,
                &sources,
                &name,
                source.as_deref(),
            )
            .ok_or_else(|| anyhow::anyhow!("Skill '{name}' not found in market"))?;
            let source_repo = skill.source_repo.clone();
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::core::market::Market::install_single(
                &skill,
                mgr.paths(),
            ))?;
            let _ = mgr.register_local_skill(&skill.name);
            if let Some(id) = mgr.find_resource_id(&skill.name) {
                let _ = mgr.enable_resource(&id, CliTarget::Claude, None);
            }
            println!("Installed '{name}' from {source_repo}");
            Ok(())
        }
        Some(Commands::Uninstall { name }) => {
            let resource_id = find_resource_id_by_name(&mgr, &name)?;
            mgr.uninstall(&resource_id)?;
            println!("Resource '{name}' moved to trash");
            Ok(())
        }
        Some(Commands::Trash { command }) => {
            handle_trash_command(&mgr, command)?;
            Ok(())
        }
        Some(Commands::Backup) => {
            let paths = mgr.paths();
            match crate::core::backup::create_backup(paths) {
                Ok(dir) => println!("Backup created: {}", dir.display()),
                Err(e) => eprintln!("Backup failed: {e}"),
            }
            Ok(())
        }
        Some(Commands::Backups) => {
            let paths = mgr.paths();
            let list = crate::core::backup::list_backups(paths);
            if list.is_empty() {
                println!("No backups found.");
            } else {
                for ts in &list {
                    println!("  {ts}");
                }
                println!("\nTotal: {} backups", list.len());
            }
            Ok(())
        }
        Some(Commands::Search { query }) => {
            use crate::core::search::{fuzzy_score_any, new_matcher};
            let mut matcher = new_matcher();
            let resources = mgr.list_resources(None, None).unwrap_or_default();
            let mut local_scored: Vec<(&_, u32)> = resources
                .iter()
                .filter_map(|r| {
                    fuzzy_score_any(&mut matcher, &query, &[&r.name, &r.description])
                        .map(|s| (r, s))
                })
                .collect();
            local_scored.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.usage_count.cmp(&a.0.usage_count)));

            if !local_scored.is_empty() {
                println!("── Installed ({}) ──", local_scored.len());
                for (r, _) in &local_scored {
                    let icon = if r.enabled.values().any(|&v| v) {
                        "●"
                    } else {
                        "○"
                    };
                    let usage = if r.usage_count > 0 {
                        format!(" [{}x]", r.usage_count)
                    } else {
                        String::new()
                    };
                    println!("  {icon} {:<5} {}{usage}", r.kind.as_str(), r.name);
                }
            }

            let data_dir = mgr.paths().data_dir().to_path_buf();
            let sources = crate::core::market::load_sources(&data_dir);
            let installed_names: Vec<String> = resources.iter().map(|r| r.name.clone()).collect();
            let mut market_scored: Vec<(String, u32)> = Vec::new();
            for src in &sources {
                if !src.enabled {
                    continue;
                }
                if let Some(cached) = crate::core::market::load_cache(&data_dir, src) {
                    for skill in cached {
                        if installed_names.contains(&skill.name) {
                            continue;
                        }
                        if let Some(score) =
                            fuzzy_score_any(&mut matcher, &query, &[&skill.name, &skill.repo_path])
                        {
                            market_scored.push((
                                format!("  {} ({})", skill.name, skill.source_label),
                                score,
                            ));
                        }
                    }
                }
            }
            market_scored.sort_by(|a, b| b.1.cmp(&a.1));

            if !market_scored.is_empty() {
                println!("\n── Market ({}) ──", market_scored.len());
                for (line, _) in market_scored.iter().take(20) {
                    println!("{line}");
                }
                println!("Use 'runai market-install <name>' to install.");
            }

            if local_scored.is_empty() && market_scored.is_empty() {
                println!("No matches for '{query}'.");
            }
            Ok(())
        }
        Some(Commands::Market { source, search }) => {
            use crate::core::search::{fuzzy_score_any, new_matcher};
            let data_dir = mgr.paths().data_dir().to_path_buf();
            let sources = crate::core::market::load_sources(&data_dir);
            let installed: Vec<String> = mgr
                .list_resources(None, None)
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.name)
                .collect();
            let mut matcher = new_matcher();

            let mut rows: Vec<(String, u32)> = Vec::new();
            for src in &sources {
                if !src.enabled {
                    continue;
                }
                if let Some(ref filter) = source {
                    let f = filter.to_lowercase();
                    if !src.label.to_lowercase().contains(&f)
                        && !src.repo_id().to_lowercase().contains(&f)
                    {
                        continue;
                    }
                }
                if let Some(cached) = crate::core::market::load_cache(&data_dir, src) {
                    for skill in cached {
                        let score = if let Some(ref q) = search {
                            match fuzzy_score_any(
                                &mut matcher,
                                q,
                                &[&skill.name, &skill.repo_path, &skill.source_label],
                            ) {
                                Some(s) => s,
                                None => continue,
                            }
                        } else {
                            0
                        };
                        let tag = if installed.contains(&skill.name) {
                            "●"
                        } else {
                            "○"
                        };
                        rows.push((
                            format!("  {tag} {:<40} {}", skill.name, skill.source_label),
                            score,
                        ));
                    }
                }
            }
            if search.is_some() {
                rows.sort_by(|a, b| b.1.cmp(&a.1));
            }
            for (line, _) in &rows {
                println!("{line}");
            }
            if rows.is_empty() {
                println!("No market skills matched.");
            } else {
                println!("\nTotal: {} skills", rows.len());
            }
            Ok(())
        }
        Some(Commands::Restore { timestamp }) => {
            let paths = mgr.paths();
            let ts = match timestamp {
                Some(t) => t,
                None => {
                    let backups = crate::core::backup::list_backups(paths);
                    match backups.first() {
                        Some(t) => t.clone(),
                        None => {
                            eprintln!("No backups found. Run 'runai backup' first.");
                            return Ok(());
                        }
                    }
                }
            };
            println!("Restoring from backup: {ts}");
            match crate::core::backup::restore_backup(paths, &ts) {
                Ok(n) => println!("Restored {n} items"),
                Err(e) => eprintln!("Restore failed: {e}"),
            }
            Ok(())
        }
        Some(Commands::Group { command }) => handle_group_command(&mgr, command),
        Some(Commands::Status { target }) => {
            let target = target
                .parse::<CliTarget>()
                .map_err(|_| anyhow::anyhow!("unknown target: {target}"))?;
            let (skills, mcps) = mgr.status(target)?;
            let (total_skills, total_mcps) = mgr.resource_count();
            println!("Target: {target}");
            println!("  Skills: {skills}/{total_skills} enabled");
            println!("  MCPs:   {mcps}/{total_mcps} enabled");
            Ok(())
        }
        Some(Commands::McpServe) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::mcp::serve())?;
            Ok(())
        }
        Some(Commands::Server {
            port,
            host,
            ensure,
            install_hook,
            uninstall_hook,
        }) => {
            if install_hook {
                let home = dirs::home_dir().context("locate home dir")?;
                let cmd = format!("runai server --ensure --port {port}");
                let status = crate::core::recommend::install_session_start_hook(&home, &cmd)?;
                println!(
                    "SessionStart hook ({cmd}) in {}: {:?}",
                    home.join(".claude/settings.json").display(),
                    status
                );
                return Ok(());
            }
            if uninstall_hook {
                let home = dirs::home_dir().context("locate home dir")?;
                let cmd = format!("runai server --ensure --port {port}");
                let status = crate::core::recommend::uninstall_session_start_hook(&home, &cmd)?;
                println!("SessionStart hook removal: {:?}", status);
                return Ok(());
            }
            if ensure {
                match crate::server::ensure_running(&host, port)? {
                    crate::server::EnsureStatus::AlreadyRunning => {
                        println!("runai dashboard already running at http://{host}:{port}");
                    }
                    crate::server::EnsureStatus::Started => {
                        println!("runai dashboard started at http://{host}:{port}");
                    }
                }
                return Ok(());
            }
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::server::serve(&host, port))?;
            Ok(())
        }
        Some(Commands::Register) => {
            let home = dirs::home_dir().unwrap_or_default();
            let result = crate::core::mcp_register::McpRegister::register_all(&home);
            for name in &result.registered {
                println!("  ✓ Registered to {name}");
            }
            for name in &result.skipped {
                println!("  · {name} (already registered)");
            }
            for err in &result.errors {
                eprintln!("  ⚠ {err}");
            }
            Ok(())
        }
        Some(Commands::Usage { top }) => {
            use crate::core::resource::format_time_ago;
            let stats = mgr.usage_stats()?;
            let limit = top.unwrap_or(usize::MAX);
            if stats.is_empty() {
                println!("No usage data yet.");
            } else {
                println!("{:>5}  {:>10}  {:<5}  name", "uses", "last", "type");
                for (i, s) in stats.iter().enumerate() {
                    if i >= limit {
                        break;
                    }
                    let ago = format_time_ago(s.last_used_at);
                    let kind = if s.id.starts_with("mcp:") {
                        "mcp"
                    } else {
                        "skill"
                    };
                    println!("{:>5}  {:>10}  {:<5}  {}", s.count, ago, kind, s.name);
                }
            }
            Ok(())
        }
        Some(Commands::Unregister) => {
            let home = dirs::home_dir().unwrap_or_default();
            crate::core::mcp_register::McpRegister::unregister_all(&home)?;
            println!("Unregistered from all CLIs");
            Ok(())
        }
        Some(Commands::Update) => {
            let data_dir = crate::core::paths::data_dir();
            let rt = tokio::runtime::Runtime::new()?;
            let msg = rt.block_on(crate::core::updater::perform_update(&data_dir))?;
            println!("{msg}");
            // Exit immediately. Two reasons:
            // 1. The running process is still the *old* binary in memory
            //    (CARGO_PKG_VERSION is a compile-time constant) — any
            //    `update_notification` that runs on the way out compares
            //    stale current against fresh latest and re-notifies.
            // 2. `main.rs` spawned a background `check_for_update` that
            //    main joins before its post-exit notification. If that
            //    check finishes after `perform_update` wrote its
            //    just-upgraded suppression signal, it overwrites the
            //    cache with the stale current_version and defeats the
            //    suppression. Skipping straight to exit sidesteps both.
            std::process::exit(0);
        }
        Some(Commands::Recommend { command, prompt }) => {
            handle_recommend(&mgr, command, prompt)?;
            Ok(())
        }
        Some(Commands::Doctor { fix }) => {
            println!("runai doctor v{}\n", env!("CARGO_PKG_VERSION"));
            let results = crate::core::doctor::run_doctor();
            let mut has_fail = false;
            for r in &results {
                let icon = r.icon();
                println!("  {icon} {:<15} {}", r.name, r.detail);
                if r.status == crate::core::doctor::CheckStatus::Fail {
                    has_fail = true;
                }
            }
            println!();
            if fix {
                let report = crate::core::doctor::run_doctor_fix();
                println!("--- repair ---");
                println!(
                    "  pruned {} broken symlinks",
                    report.broken_symlinks_removed.len()
                );
                for s in &report.broken_symlinks_removed {
                    println!("    {s}");
                }
                println!(
                    "  removed {} duplicate skill DB rows",
                    report.dedupe_rows_removed
                );
                println!();
            }
            if has_fail {
                println!("Some checks failed. Run 'runai register' to fix MCP registration.");
            } else {
                println!("All checks passed.");
            }
            Ok(())
        }
    }
}

fn handle_group_command(mgr: &SkillManager, command: GroupCommands) -> Result<()> {
    match command {
        GroupCommands::Create {
            id,
            name,
            description,
            kind,
        } => {
            let kind = match kind.as_str() {
                "default" => GroupKind::Default,
                "ecosystem" => GroupKind::Ecosystem,
                _ => GroupKind::Custom,
            };
            let group = Group {
                name,
                description,
                kind,
                auto_enable: false,
                members: vec![],
            };
            mgr.create_group(&id, &group)?;
            println!("Group '{id}' created");
            Ok(())
        }
        GroupCommands::Add {
            group,
            resource,
            resource_type,
        } => {
            let resource_id = find_resource_id_by_name(mgr, &resource)?;
            mgr.db().add_group_member(&group, &resource_id)?;

            let path = mgr.paths().groups_dir().join(format!("{group}.toml"));
            if path.exists() {
                let mut g = Group::load_from_file(&path)?;
                let member_type = match resource_type.as_str() {
                    "mcp" => MemberType::Mcp,
                    _ => MemberType::Skill,
                };
                if !g.members.iter().any(|m| m.name == resource) {
                    g.members.push(GroupMember {
                        name: resource.clone(),
                        member_type,
                    });
                    g.save_to_file(&path)?;
                }
            }
            println!("Added '{resource}' to group '{group}'");
            Ok(())
        }
        GroupCommands::Remove { group, resource } => {
            let resource_id = find_resource_id_by_name(mgr, &resource)?;
            mgr.db().remove_group_member(&group, &resource_id)?;

            let path = mgr.paths().groups_dir().join(format!("{group}.toml"));
            if path.exists() {
                let mut g = Group::load_from_file(&path)?;
                g.members.retain(|m| m.name != resource);
                g.save_to_file(&path)?;
            }
            println!("Removed '{resource}' from group '{group}'");
            Ok(())
        }
        GroupCommands::List => {
            let groups = mgr.list_groups()?;
            if groups.is_empty() {
                println!("No groups defined.");
            } else {
                for (id, g) in &groups {
                    let members = mgr.db().get_group_members(id).unwrap_or_default();
                    let kind_str = match g.kind {
                        GroupKind::Default => "default",
                        GroupKind::Ecosystem => "ecosystem",
                        GroupKind::Custom => "custom",
                    };
                    println!(
                        "  [{kind_str}] {id} — {} ({} members)",
                        g.name,
                        members.len()
                    );
                    if !g.description.is_empty() {
                        let desc: String = g.description.chars().take(120).collect();
                        let ellipsis = if g.description.chars().count() > 120 {
                            "…"
                        } else {
                            ""
                        };
                        println!("      {desc}{ellipsis}");
                    }
                }
                println!("\nTip: `runai group show <id>` for full description + member list.");
            }
            Ok(())
        }
        GroupCommands::Show { id } => {
            let groups = mgr.list_groups()?;
            let (gid, g) = groups
                .iter()
                .find(|(gid, _)| gid == &id)
                .ok_or_else(|| anyhow::anyhow!("group not found: {id}"))?;
            let members = mgr.db().get_group_members(gid).unwrap_or_default();
            let kind_str = match g.kind {
                GroupKind::Default => "default",
                GroupKind::Ecosystem => "ecosystem",
                GroupKind::Custom => "custom",
            };
            println!("Group: {gid}");
            println!("  Display name: {}", g.name);
            println!("  Kind:         {kind_str}");
            println!("  Members:      {}", members.len());
            if g.description.is_empty() {
                println!("  Description:  (none)");
            } else {
                println!("  Description:");
                for line in g.description.lines() {
                    println!("    {line}");
                }
            }
            if !members.is_empty() {
                println!("\nMembers:");
                for r in &members {
                    let badge = r.kind.as_str();
                    let desc: String = r.description.chars().take(70).collect();
                    println!("  [{badge}] {} — {desc}", r.name);
                }
            }
            Ok(())
        }
        GroupCommands::Delete { id } => {
            let path = mgr.paths().groups_dir().join(format!("{id}.toml"));
            if !path.exists() {
                anyhow::bail!("Group not found: {id}");
            }
            std::fs::remove_file(&path)?;
            println!("Group '{id}' deleted");
            Ok(())
        }
        GroupCommands::Update {
            id,
            name,
            description,
        } => {
            mgr.update_group(&id, name.as_deref(), description.as_deref())?;
            let mut changes = Vec::new();
            if let Some(n) = &name {
                changes.push(format!("name='{n}'"));
            }
            if let Some(d) = &description {
                changes.push(format!("desc='{d}'"));
            }
            if changes.is_empty() {
                println!("Group '{id}' unchanged (pass --name and/or --description)");
            } else {
                println!("Group '{id}' updated: {}", changes.join(", "));
            }
            Ok(())
        }
    }
}

fn find_resource_id_by_name(mgr: &SkillManager, name: &str) -> Result<String> {
    mgr.find_resource_id(name)
        .ok_or_else(|| anyhow::anyhow!("resource not found: {name}"))
}

fn find_trash_id_by_query(mgr: &SkillManager, query: &str) -> Result<String> {
    mgr.find_trash_id(query)
        .ok_or_else(|| anyhow::anyhow!("trash entry not found: {query}"))
}

fn handle_recommend(
    mgr: &SkillManager,
    command: Option<RecommendCommands>,
    prompt: Option<String>,
) -> Result<()> {
    use crate::core::recommend::{Provider, RecommendConfig, format_for_hook, recommend};

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
            if !cfg.enabled {
                return Ok(());
            }
            match recommend(
                mgr,
                &user_prompt,
                transcript_path.as_deref(),
                session_id.as_deref(),
                cwd.as_deref(),
            ) {
                Ok(decision) => {
                    let out = format_for_hook(&decision);
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
            use crate::core::recommend::{HookInstallStatus, install_claude_hook};
            let home =
                dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot resolve home directory"))?;
            let path = home.join(".claude/settings.json");
            match install_claude_hook(&home)? {
                HookInstallStatus::Installed => {
                    println!("hook installed into {}", path.display());
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
        (Some(RecommendCommands::MineFeedback { hours }), _) => {
            let report = crate::core::feedback::mine_feedback(mgr, hours)?;
            println!(
                "feedback mining done (last {hours}h):\n  signals counted:      {}\n  ratings written:      {}\n  skipped (manual present): {}",
                report.total_signals, report.ratings_written, report.skipped_manual_present
            );
            for (skill, score) in &report.per_skill {
                println!("    {skill:30}  auto={score}");
            }
            Ok(())
        }
        (Some(RecommendCommands::ResetScoring { only, yes }), _) => {
            let only = only.to_ascii_lowercase();
            let (do_sum, do_rat) = match only.as_str() {
                "summaries" => (true, false),
                "ratings" => (false, true),
                "all" | "both" => (true, true),
                other => anyhow::bail!("--only must be one of summaries|ratings|all, got {other}"),
            };
            if !yes {
                use std::io::{BufRead, Write};
                print!(
                    "about to wipe {}{}{}. continue? [y/N] ",
                    if do_sum { "LLM summaries+scores" } else { "" },
                    if do_sum && do_rat { " AND " } else { "" },
                    if do_rat { "user ratings" } else { "" }
                );
                std::io::stdout().flush().ok();
                let stdin = std::io::stdin();
                let line = stdin.lock().lines().next().transpose()?.unwrap_or_default();
                if !matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    println!("aborted");
                    return Ok(());
                }
            }
            let (s, r) = mgr.db().reset_scoring(do_sum, do_rat)?;
            println!("deleted: {s} summaries, {r} user ratings");
            Ok(())
        }
        (
            Some(RecommendCommands::Enrich {
                limit,
                force,
                missing_only,
                verbose,
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
            let (have, _oldest, _newest) =
                mgr.db().skill_ai_summary_stats().unwrap_or((0, None, None));
            println!(
                "enriching skill summaries (currently {have} have summaries)\n\
                 limit={} mode={:?}",
                limit.map(|n| n.to_string()).unwrap_or_else(|| "all".into()),
                mode,
            );
            let report = crate::core::recommend::enrich_skills(mgr, limit, mode, verbose)?;
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
        .map(|rs| rs.iter().filter(|r| r.kind == crate::core::resource::ResourceKind::Skill).count())
        .unwrap_or(0);
    let missing = total_skills.saturating_sub(already_have as usize);
    if missing > 0 {
        if let Ok(exe) = std::env::current_exe() {
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
    }
    Ok(())
}

fn handle_trash_command(mgr: &SkillManager, command: TrashCommands) -> Result<()> {
    match command {
        TrashCommands::List => {
            use crate::core::resource::format_time_ago;

            let entries = mgr.list_trash()?;
            if entries.is_empty() {
                println!("Trash is empty.");
            } else {
                for entry in &entries {
                    let deleted = format_time_ago(Some(entry.deleted_at));
                    println!(
                        "  [{}] {} — {} ({})",
                        entry.kind.as_str(),
                        entry.id,
                        entry.name,
                        deleted
                    );
                }
                println!("\nTotal: {} trashed resources", entries.len());
            }
            Ok(())
        }
        TrashCommands::Restore { query } => {
            let trash_id = find_trash_id_by_query(mgr, &query)?;
            mgr.restore_from_trash(&trash_id)?;
            println!("Restored '{query}'");
            Ok(())
        }
        TrashCommands::Purge { query } => {
            let trash_id = find_trash_id_by_query(mgr, &query)?;
            mgr.purge_trash(&trash_id)?;
            println!("Permanently deleted '{query}'");
            Ok(())
        }
        TrashCommands::Empty => {
            let count = mgr.empty_trash()?;
            println!("Emptied trash ({count} items)");
            Ok(())
        }
    }
}

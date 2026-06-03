use super::command_enums::{Cli, Commands};
use super::handlers::{handle_group_command, handle_recommend, handle_trash_command};
use super::helpers::{find_resource_id_by_name, spawn_targeted_enrich};
use crate::core::cli_target::CliTarget;
use crate::core::manager::SkillManager;
use anyhow::{Context, Result};

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
            // Auto-spawn the dashboard server (idempotent: no-op when the
            // port is already bound). Lets `runai` alone bring up TUI +
            // dashboard together so the dashboard URL on the live-strip is
            // immediately clickable. Failures are non-fatal — the TUI is
            // the primary interface and runs fine without the dashboard.
            // Set `RUNAI_NO_AUTOSPAWN=1` to skip.
            if std::env::var_os("RUNAI_NO_AUTOSPAWN").is_none() {
                let _ = crate::server::ensure_running("127.0.0.1", 17888);
            }
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
            spawn_targeted_enrich(&result.adopted_names);
            if !result.adopted_names.is_empty() {
                println!(
                    "(spawned background enrich for {} newly-adopted skill(s))",
                    result.adopted_names.len()
                );
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
            spawn_targeted_enrich(&names);
            if !names.is_empty() {
                println!(
                    "(spawned background enrich for {} new skill(s) — dashboard /skills will update once summaries land)",
                    names.len()
                );
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
                &mgr.paths().skills_dir(),
            ))?;
            let _ = mgr.register_local_skill(&skill.name);
            if let Some(id) = mgr.find_resource_id(&skill.name) {
                let _ = mgr.enable_resource(&id, CliTarget::Claude, None);
            }
            println!("Installed '{name}' from {source_repo}");
            spawn_targeted_enrich(std::slice::from_ref(&skill.name));
            println!(
                "(spawned background enrich for '{}' — dashboard /skills will update once summary lands)",
                skill.name
            );
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
            install_autostart,
            uninstall_autostart,
        }) => {
            if install_autostart {
                use crate::core::autostart::{self, AutostartStatus};
                match autostart::install(&host, port)? {
                    AutostartStatus::Installed { path } => println!(
                        "autostart installed at {} — server will start at every login",
                        path.display()
                    ),
                    AutostartStatus::Reinstalled { path } => println!(
                        "autostart reinstalled at {} — refreshed binary path / port",
                        path.display()
                    ),
                    other => println!("autostart: {other:?}"),
                }
                return Ok(());
            }
            if uninstall_autostart {
                use crate::core::autostart::{self, AutostartStatus};
                match autostart::uninstall()? {
                    AutostartStatus::Uninstalled { path } => println!(
                        "autostart removed from {} — server will no longer start at login",
                        path.display()
                    ),
                    AutostartStatus::NotInstalled => println!("autostart was not installed; nothing to do"),
                    other => println!("autostart: {other:?}"),
                }
                return Ok(());
            }
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

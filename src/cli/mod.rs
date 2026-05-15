use crate::core::cli_target::CliTarget;
use crate::core::group::{Group, GroupKind, GroupMember, MemberType};
use crate::core::manager::SkillManager;
use anyhow::Result;
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

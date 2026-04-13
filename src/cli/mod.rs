use crate::core::cli_target::CliTarget;
use crate::core::group::{Group, GroupKind, GroupMember, MemberType};
use crate::core::manager::SkillManager;
use crate::core::resource::ResourceKind;
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
    /// Restore from backup (uses latest backup by default)
    Restore {
        /// Backup timestamp (omit for latest)
        #[arg(long)]
        timestamp: Option<String>,
    },
    /// Create a backup now
    Backup,
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
    Doctor,
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
            let kind_filter = kind.as_deref().and_then(ResourceKind::from_str);
            let target_filter = target.as_deref().and_then(CliTarget::from_str);

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
            let target = CliTarget::from_str(&target)
                .ok_or_else(|| anyhow::anyhow!("unknown target: {target}"))?;
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
            let target = CliTarget::from_str(&target)
                .ok_or_else(|| anyhow::anyhow!("unknown target: {target}"))?;
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
            println!("Resource '{name}' uninstalled");
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
            let target = CliTarget::from_str(&target)
                .ok_or_else(|| anyhow::anyhow!("unknown target: {target}"))?;
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
                println!("{:>5}  {:>10}  {:<5}  {}", "uses", "last", "type", "name");
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
            Ok(())
        }
        Some(Commands::Doctor) => {
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
    }
}

fn find_resource_id_by_name(mgr: &SkillManager, name: &str) -> Result<String> {
    mgr.find_resource_id(name)
        .ok_or_else(|| anyhow::anyhow!("resource not found: {name}"))
}

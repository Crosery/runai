use clap::{Parser, Subcommand};
use anyhow::Result;
use crate::core::cli_target::CliTarget;
use crate::core::group::{Group, GroupKind, GroupMember, MemberType};
use crate::core::manager::SkillManager;
use crate::core::resource::ResourceKind;

#[derive(Parser)]
#[command(name = "skill-manager", version, about = "AI CLI resource manager for skills and MCP servers")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Scan CLI directories and adopt unmanaged skills
    Scan,
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
    Install {
        source: String,
    },
    /// Uninstall a resource
    Uninstall {
        name: String,
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
    /// Register skill-manager as MCP server in all CLI configs
    Register,
    /// Unregister skill-manager from all CLI configs
    Unregister,
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
    Remove {
        group: String,
        resource: String,
    },
    /// List all groups
    List,
}

pub fn run(cli: Cli) -> Result<()> {
    let mgr = if let Ok(dir) = std::env::var("SKILL_MANAGER_DATA_DIR") {
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
            println!("Scan complete: {} adopted, {} skipped, {} errors",
                result.adopted, result.skipped, result.errors.len());
            for err in &result.errors {
                eprintln!("  error: {err}");
            }
            Ok(())
        }
        Some(Commands::List { group, kind, target }) => {
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
                    let enabled_targets: Vec<&str> = CliTarget::ALL.iter()
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
        Some(Commands::Install { source: _ }) => {
            println!("Install — not yet implemented in CLI (use MCP interface)");
            Ok(())
        }
        Some(Commands::Uninstall { name }) => {
            let resource_id = find_resource_id_by_name(&mgr, &name)?;
            mgr.uninstall(&resource_id)?;
            println!("Resource '{name}' uninstalled");
            Ok(())
        }
        Some(Commands::Group { command }) => handle_group_command(&mgr, command),
        Some(Commands::Status { target }) => {
            let target = CliTarget::from_str(&target)
                .ok_or_else(|| anyhow::anyhow!("unknown target: {target}"))?;
            let (skills, mcps) = mgr.status(target)?;
            let (total_skills, total_mcps) = mgr.db().resource_count()?;
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
        Some(Commands::Unregister) => {
            let home = dirs::home_dir().unwrap_or_default();
            crate::core::mcp_register::McpRegister::unregister_all(&home)?;
            println!("Unregistered from all CLIs");
            Ok(())
        }
    }
}

fn handle_group_command(mgr: &SkillManager, command: GroupCommands) -> Result<()> {
    match command {
        GroupCommands::Create { id, name, description, kind } => {
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
        GroupCommands::Add { group, resource, resource_type } => {
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
                    g.members.push(GroupMember { name: resource.clone(), member_type });
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
                    println!("  [{kind_str}] {id} — {} ({} members)", g.name, members.len());
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

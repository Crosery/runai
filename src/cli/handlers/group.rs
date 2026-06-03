use crate::cli::command_enums::GroupCommands;
use crate::cli::helpers::find_resource_id_by_name;
use crate::core::group::{Group, GroupKind, GroupMember, MemberType};
use crate::core::manager::SkillManager;
use anyhow::Result;

pub(in crate::cli) fn handle_group_command(
    mgr: &SkillManager,
    command: GroupCommands,
) -> Result<()> {
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

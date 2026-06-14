use super::SkillManager;
use crate::core::classifier::Classifier;
use crate::core::cli_target::CliTarget;
use crate::core::group::Group;
use crate::core::resource::{Resource, ResourceKind, Source};
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

impl SkillManager {
    // --- Group management ---

    pub fn create_group(&self, group_id: &str, group: &Group) -> Result<()> {
        let path = self.paths.groups_dir().join(format!("{group_id}.toml"));
        group.save_to_file(&path)?;

        for member in &group.members {
            if let Some(rid) = self.find_resource_id(&member.name) {
                self.db.add_group_member(group_id, &rid)?;
            }
        }

        Ok(())
    }

    pub fn list_groups(&self) -> Result<Vec<(String, Group)>> {
        let dir = self.paths.groups_dir();
        let mut groups = Vec::new();

        if !dir.exists() {
            return Ok(groups);
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                match Group::load_from_file(&path) {
                    Ok(group) => groups.push((id, group)),
                    Err(_) => continue,
                }
            }
        }

        groups.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        Ok(groups)
    }

    /// Get group members, resolving mcp: IDs from config files dynamically.
    pub fn get_group_members(&self, group_id: &str) -> Result<Vec<Resource>> {
        let ids = self.db.get_group_member_ids(group_id)?;
        let mcp_status = Self::read_mcp_status_from_configs();
        let mut members = Vec::new();

        for id in &ids {
            if let Some(mcp_name) = id.strip_prefix("mcp:") {
                let enabled = mcp_status.get(mcp_name).cloned().unwrap_or_default();
                members.push(Resource {
                    id: id.clone(),
                    name: mcp_name.to_string(),
                    kind: ResourceKind::Mcp,
                    description: String::new(),
                    directory: PathBuf::new(),
                    source: Source::Local {
                        path: PathBuf::new(),
                    },
                    installed_at: 0,
                    enabled,
                    usage_count: 0,
                    last_used_at: None,
                    owner_user_id: None,
                    publish_status: "draft".to_string(),
                });
            } else if let Ok(Some(mut res)) = self.db.get_resource(id) {
                res.enabled = self.check_skill_symlinks(&res.name);
                members.push(res);
            }
        }

        Ok(members)
    }

    pub fn enable_group(
        &self,
        group_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        let members = self.get_group_members(group_id)?;
        for member in &members {
            self.enable_resource(&member.id, target, cli_dir_override)?;
        }
        Ok(())
    }

    pub fn disable_group(
        &self,
        group_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        let members = self.get_group_members(group_id)?;
        for member in &members {
            self.disable_resource(&member.id, target, cli_dir_override)?;
        }
        Ok(())
    }

    /// Update group name and/or description. Pass None to keep unchanged.
    pub fn update_group(
        &self,
        group_id: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<()> {
        let path = self.paths.groups_dir().join(format!("{group_id}.toml"));
        if !path.exists() {
            bail!("Group not found: {group_id}");
        }
        let mut group = Group::load_from_file(&path)?;
        if let Some(n) = name {
            group.name = n.to_string();
        }
        if let Some(d) = description {
            group.description = d.to_string();
        }
        group.save_to_file(&path)?;
        Ok(())
    }

    /// Fuzzy find group_id: exact match > contains > starts_with.
    pub fn find_group_id(&self, query: &str) -> Option<String> {
        let groups = self.list_groups().ok()?;
        let q = query.to_lowercase();
        // exact match on id or name
        if let Some((id, _)) = groups
            .iter()
            .find(|(id, g)| id.to_lowercase() == q || g.name.to_lowercase() == q)
        {
            return Some(id.clone());
        }
        // contains match
        if let Some((id, _)) = groups
            .iter()
            .find(|(id, g)| id.to_lowercase().contains(&q) || g.name.to_lowercase().contains(&q))
        {
            return Some(id.clone());
        }
        None
    }

    /// Convenience wrapper for backward compat.
    pub fn rename_group(&self, group_id: &str, new_name: &str) -> Result<()> {
        self.update_group(group_id, Some(new_name), None)
    }

    pub fn get_suggested_groups(&self, name: &str, description: &str) -> Vec<String> {
        Classifier::suggest_groups(name, description)
    }
}

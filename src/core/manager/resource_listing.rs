use super::SkillManager;
use crate::core::cli_target::CliTarget;
use crate::core::resource::{Resource, ResourceKind, Source};
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;

impl SkillManager {
    pub fn list_resources(
        &self,
        kind: Option<ResourceKind>,
        enabled_for: Option<CliTarget>,
    ) -> Result<Vec<Resource>> {
        let mut resources = Vec::new();

        // Skills: from DB, enabled state from symlinks, deduplicated by name
        if kind.is_none() || kind == Some(ResourceKind::Skill) {
            let mut skills = self.db.list_resources(Some(ResourceKind::Skill), None)?;
            // Deduplicate by name — keep first occurrence (alphabetical by id from DB)
            let mut seen_names = std::collections::HashSet::new();
            skills.retain(|s| seen_names.insert(s.name.clone()));
            for skill in &mut skills {
                skill.enabled = self.check_skill_symlinks(&skill.name);
            }
            if let Some(target) = enabled_for {
                skills.retain(|s| s.is_enabled_for(target));
            }
            resources.extend(skills);
        }

        // MCPs: from config files (enabled) + backup dir (disabled by SM)
        if kind.is_none() || kind == Some(ResourceKind::Mcp) {
            let mcp_status = Self::read_mcp_status_from_configs();
            let mut seen = std::collections::HashSet::new();
            let mut mcp_resources = Vec::new();

            // 1. Active MCPs from config files
            for (name, targets) in &mcp_status {
                seen.insert(name.clone());
                mcp_resources.push(Resource {
                    id: format!("mcp:{name}"),
                    name: name.clone(),
                    kind: ResourceKind::Mcp,
                    description: String::new(),
                    directory: PathBuf::new(),
                    source: Source::Local {
                        path: PathBuf::new(),
                    },
                    installed_at: 0,
                    enabled: targets.clone(),
                    usage_count: 0,
                    last_used_at: None,
                    owner_user_id: None,
                    publish_status: "draft".to_string(),
                });
            }

            // 2. Disabled MCPs from backup dir (removed from config by SM)
            let mcps_dir = self.paths.mcps_dir();
            if mcps_dir.exists()
                && let Ok(entries) = std::fs::read_dir(&mcps_dir)
            {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    if name.is_empty() || seen.contains(&name) {
                        continue;
                    }
                    // This MCP was disabled by SM — show as disabled
                    mcp_resources.push(Resource {
                        id: format!("mcp:{name}"),
                        name,
                        kind: ResourceKind::Mcp,
                        description: String::new(),
                        directory: PathBuf::new(),
                        source: Source::Local {
                            path: PathBuf::new(),
                        },
                        installed_at: 0,
                        enabled: HashMap::new(), // no targets = disabled
                        usage_count: 0,
                        last_used_at: None,
                        owner_user_id: None,
                        publish_status: "draft".to_string(),
                    });
                }
            }

            // Filter by enabled_for if requested
            if let Some(target) = enabled_for {
                mcp_resources.retain(|r| r.is_enabled_for(target));
            }

            // Sort for stable order
            mcp_resources.sort_by(|a, b| a.name.cmp(&b.name));
            resources.extend(mcp_resources);
        }

        Ok(resources)
    }

    pub fn find_resource_id(&self, name: &str) -> Option<String> {
        for prefix in &["local:", "adopted:", "github:"] {
            let id = format!("{prefix}{name}");
            if let Ok(Some(_)) = self.db.get_resource(&id) {
                return Some(id);
            }
        }
        // C5 (scan_findings.md): this entrypoint is public-pool-only (feeds
        // destructive local ops — uninstall / sm_delete / batch_delete /
        // group member resolution). The fallback must NOT reach another user's
        // private row, or a name collision could trash their private skill.
        // `list_resources_for_user(None, None)` filters `owner_user_id IS NULL`,
        // still catching public rows (incl. github:owner/repo:name) whose id
        // the prefix probes above miss.
        if let Ok(all) = self.db.list_resources_for_user(None, None) {
            for r in all {
                if r.name == name {
                    return Some(r.id);
                }
            }
        }
        // Check MCP config files
        let mcp_status = Self::read_mcp_status_from_configs();
        if mcp_status.contains_key(name) {
            return Some(format!("mcp:{name}"));
        }
        if self.paths.mcps_dir().join(format!("{name}.json")).exists() {
            return Some(format!("mcp:{name}"));
        }
        None
    }
}

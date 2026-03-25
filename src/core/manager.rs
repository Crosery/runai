use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Result, bail};
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::group::Group;
use crate::core::linker::Linker;
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};
use crate::core::scanner::Scanner;
use crate::core::classifier::Classifier;

pub struct SkillManager {
    paths: AppPaths,
    db: Database,
}

impl SkillManager {
    pub fn new() -> Result<Self> {
        let paths = AppPaths::default_path();
        paths.ensure_dirs()?;
        let db = Database::open(&paths.db_path())?;
        Ok(Self { paths, db })
    }

    pub fn with_base(base: PathBuf) -> Result<Self> {
        let paths = AppPaths::with_base(base);
        paths.ensure_dirs()?;
        let db = Database::open(&paths.db_path())?;
        Ok(Self { paths, db })
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    // --- Scan ---

    pub fn scan(&self) -> Result<crate::core::scanner::ScanResult> {
        Scanner::scan_all(&self.paths, &self.db)
    }

    // --- Resource management ---

    pub fn register_local_skill(&self, name: &str) -> Result<()> {
        let dir = self.paths.skills_dir().join(name);
        if !dir.exists() {
            bail!("skill directory not found: {}", dir.display());
        }

        let description = Self::extract_description(&dir);
        let source = Source::Local { path: dir.clone() };
        let id = Resource::generate_id(&source, name);

        let resource = Resource {
            id,
            name: name.to_string(),
            kind: ResourceKind::Skill,
            description,
            directory: dir,
            source,
            installed_at: chrono::Utc::now().timestamp(),
            enabled: HashMap::new(),
        };

        self.db.insert_resource(&resource)?;
        Ok(())
    }

    pub fn enable_resource(
        &self,
        resource_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        if let Some(mcp_name) = resource_id.strip_prefix("mcp:") {
            self.restore_mcp(mcp_name, target)
        } else {
            let resource = self.db.get_resource(resource_id)?
                .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;
            let cli_dir = cli_dir_override
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| target.skills_dir());
            std::fs::create_dir_all(&cli_dir)?;
            let link_path = cli_dir.join(&resource.name);
            if !link_path.exists() {
                Linker::create_link(&resource.directory, &link_path)?;
            }
            Ok(())
        }
    }

    pub fn disable_resource(
        &self,
        resource_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        if let Some(mcp_name) = resource_id.strip_prefix("mcp:") {
            if mcp_name == "skill-manager" {
                bail!("Cannot disable skill-manager — it would remove its own MCP connection");
            }
            self.remove_mcp(mcp_name, target)
        } else {
            let resource = self.db.get_resource(resource_id)?
                .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;
            let cli_dir = cli_dir_override
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| target.skills_dir());
            let link_path = cli_dir.join(&resource.name);
            if Linker::is_our_symlink(&link_path, self.paths.data_dir()) {
                Linker::remove_link(&link_path)?;
            }
            Ok(())
        }
    }

    /// Disable MCP: save config to backup, remove entry from CLI config file.
    fn remove_mcp(&self, mcp_name: &str, target: CliTarget) -> Result<()> {
        let config_path = Self::cli_config_path(target);
        if !config_path.exists() { return Ok(()); }

        let content = std::fs::read_to_string(&config_path)?;
        let mut config: serde_json::Value = serde_json::from_str(&content)?;

        if let Some(servers) = config.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
            if let Some(entry) = servers.remove(mcp_name) {
                // Save backup before removing
                let backup_dir = self.paths.mcps_dir();
                std::fs::create_dir_all(&backup_dir)?;
                let backup_path = backup_dir.join(format!("{mcp_name}.json"));
                std::fs::write(&backup_path, serde_json::to_string_pretty(&entry)?)?;

                std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
            }
            // Already removed — noop
        }

        Ok(())
    }

    /// Enable MCP: restore saved config back into CLI config file.
    fn restore_mcp(&self, mcp_name: &str, target: CliTarget) -> Result<()> {
        let config_path = Self::cli_config_path(target);

        // Read backup
        let backup_path = self.paths.mcps_dir().join(format!("{mcp_name}.json"));
        if !backup_path.exists() {
            bail!("No saved config for MCP '{mcp_name}'. Use 'claude mcp add' to register it first.");
        }
        let backup_content = std::fs::read_to_string(&backup_path)?;
        let mut entry: serde_json::Value = serde_json::from_str(&backup_content)?;

        // Remove disabled field if present (clean restore)
        if let Some(obj) = entry.as_object_mut() {
            obj.remove("disabled");
        }

        // Read or create config
        let mut config: serde_json::Value = if config_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(&config_path)?)?
        } else {
            serde_json::json!({})
        };

        let servers = config.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("config is not an object"))?
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));

        servers.as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcpServers is not an object"))?
            .insert(mcp_name.to_string(), entry);

        std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
        Ok(())
    }

    fn cli_config_path(target: CliTarget) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        match target {
            CliTarget::Claude => home.join(".claude.json"),
            CliTarget::Gemini => home.join(".gemini/settings.json"),
            CliTarget::Codex => home.join(".codex/settings.json"),
            CliTarget::OpenCode => home.join(".opencode/settings.json"),
        }
    }

    /// Read MCP enabled/disabled status directly from CLI config files.
    /// Returns mcp_name -> { target -> enabled }.
    pub fn read_mcp_status_from_configs() -> HashMap<String, HashMap<CliTarget, bool>> {
        let home = dirs::home_dir().unwrap_or_default();
        let configs: &[(CliTarget, &str)] = &[
            (CliTarget::Claude, ".claude.json"),
            (CliTarget::Gemini, ".gemini/settings.json"),
            (CliTarget::Codex, ".codex/settings.json"),
            (CliTarget::OpenCode, ".opencode/settings.json"),
        ];

        let mut result: HashMap<String, HashMap<CliTarget, bool>> = HashMap::new();

        for (target, rel) in configs {
            let path = home.join(rel);
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let config: serde_json::Value = match serde_json::from_str(&content) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let servers = match config.get("mcpServers").and_then(|s| s.as_object()) {
                Some(s) => s,
                None => continue,
            };
            for (name, _server) in servers {
                if name.starts_with('_') { continue; }
                // Entry exists in config = enabled for this target
                result.entry(name.clone())
                    .or_default()
                    .insert(*target, true);
            }
        }

        result
    }

    pub fn list_resources(
        &self,
        kind: Option<ResourceKind>,
        enabled_for: Option<CliTarget>,
    ) -> Result<Vec<Resource>> {
        let mut resources = Vec::new();

        // Skills: from DB, enabled state from symlinks
        if kind.is_none() || kind == Some(ResourceKind::Skill) {
            let mut skills = self.db.list_resources(Some(ResourceKind::Skill), None)?;
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
                    source: Source::Local { path: PathBuf::new() },
                    installed_at: 0,
                    enabled: targets.clone(),
                });
            }

            // 2. Disabled MCPs from backup dir (removed from config by SM)
            let mcps_dir = self.paths.mcps_dir();
            if mcps_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&mcps_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
                        let name = path.file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("")
                            .to_string();
                        if name.is_empty() || seen.contains(&name) { continue; }
                        // This MCP was disabled by SM — show as disabled
                        mcp_resources.push(Resource {
                            id: format!("mcp:{name}"),
                            name,
                            kind: ResourceKind::Mcp,
                            description: String::new(),
                            directory: PathBuf::new(),
                            source: Source::Local { path: PathBuf::new() },
                            installed_at: 0,
                            enabled: HashMap::new(), // no targets = disabled
                        });
                    }
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

    /// Check which CLI targets have this skill (symlink or direct dir in .agents/skills/ or skills/).
    fn check_skill_symlinks(&self, name: &str) -> HashMap<CliTarget, bool> {
        let mut map = HashMap::new();
        for target in CliTarget::ALL {
            // Check primary (.agents/skills/) and legacy (skills/) locations
            let primary = target.skills_dir().join(name);
            let legacy = target.agents_skills_dir().join(name);
            let enabled = primary.exists() || legacy.exists();
            map.insert(*target, enabled);
        }
        map
    }

    pub fn uninstall(&self, resource_id: &str) -> Result<()> {
        let resource = self.db.get_resource(resource_id)?
            .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;

        for target in CliTarget::ALL {
            let link = target.skills_dir().join(&resource.name);
            if Linker::is_our_symlink(&link, self.paths.data_dir()) {
                Linker::remove_link(&link)?;
            }
        }

        self.db.delete_resource(resource_id)?;
        Ok(())
    }

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
                let id = path.file_stem()
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
                    source: Source::Local { path: PathBuf::new() },
                    installed_at: 0,
                    enabled,
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

    pub fn get_suggested_groups(&self, name: &str, description: &str) -> Vec<String> {
        Classifier::suggest_groups(name, description)
    }

    pub fn status(&self, target: CliTarget) -> Result<(usize, usize)> {
        let mut skill_enabled = 0;
        if let Ok(skills) = self.db.list_resources(Some(ResourceKind::Skill), None) {
            for skill in &skills {
                // Check both skills/ and .agents/skills/
                let primary = target.skills_dir().join(&skill.name);
                let agents = target.agents_skills_dir().join(&skill.name);
                if primary.exists() || agents.exists() {
                    skill_enabled += 1;
                }
            }
        }
        let mcp_status = Self::read_mcp_status_from_configs();
        let mcp_enabled = mcp_status.values()
            .filter(|targets| targets.get(&target).copied().unwrap_or(false))
            .count();
        Ok((skill_enabled, mcp_enabled))
    }

    // --- Internal ---

    fn extract_description(skill_dir: &Path) -> String {
        let skill_md = skill_dir.join("SKILL.md");
        if let Ok(content) = std::fs::read_to_string(&skill_md) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                return trimmed.chars().take(200).collect();
            }
        }
        String::new()
    }

    pub fn is_first_launch(&self) -> bool {
        let (skills, mcps) = self.resource_count();
        skills + mcps == 0
    }

    /// Count total skills (from DB) + total MCPs (active + disabled by SM).
    pub fn resource_count(&self) -> (usize, usize) {
        let skills = self.db.skill_count().unwrap_or(0);
        // Active MCPs from config files
        let active_mcps = Self::read_mcp_status_from_configs();
        // Disabled MCPs backed up by SM
        let mut total_mcp_names: std::collections::HashSet<String> =
            active_mcps.keys().cloned().collect();
        let mcps_dir = self.paths.mcps_dir();
        if mcps_dir.exists() {
            if let Ok(entries) = std::fs::read_dir(&mcps_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                        if let Some(name) = entry.path().file_stem().and_then(|s| s.to_str()) {
                            total_mcp_names.insert(name.to_string());
                        }
                    }
                }
            }
        }
        (skills, total_mcp_names.len())
    }

    pub fn find_resource_id(&self, name: &str) -> Option<String> {
        for prefix in &["local:", "adopted:", "github:"] {
            let id = format!("{prefix}{name}");
            if let Ok(Some(_)) = self.db.get_resource(&id) {
                return Some(id);
            }
        }
        if let Ok(all) = self.db.list_resources(None, None) {
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
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // set_mcp_disabled reads dirs::home_dir() which checks HOME env var.
    // We serialize tests that modify HOME to avoid races.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    /// Helper: temporarily set HOME to a given path, run a closure, restore HOME.
    fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
        let _guard = HOME_LOCK.lock().unwrap();
        let original = std::env::var("HOME").ok();
        // SAFETY: we hold HOME_LOCK so no other test thread modifies HOME concurrently.
        unsafe {
            std::env::set_var("HOME", tmp);
        }
        f();
        unsafe {
            match original {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn is_first_launch_false_when_mcps_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "mcpServers": { "x": { "command": "x" } }
        });
        std::fs::write(
            tmp.path().join(".claude.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        ).unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            assert!(!mgr.is_first_launch());
        });
    }

    #[test]
    fn get_group_members_resolves_mcp_dynamically() {
        let tmp = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "mcpServers": {
                "my-mcp": { "command": "mcp-cmd", "args": [] }
            }
        });
        std::fs::write(
            tmp.path().join(".claude.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        ).unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            mgr.db().add_group_member("test-group", "mcp:my-mcp").unwrap();

            let members = mgr.get_group_members("test-group").unwrap();
            assert_eq!(members.len(), 1);
            assert_eq!(members[0].name, "my-mcp");
            assert_eq!(members[0].kind, ResourceKind::Mcp);
            assert!(members[0].is_enabled_for(CliTarget::Claude));
        });
    }

    #[test]
    fn find_resource_id_discovers_mcp_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "mcpServers": {
                "my-tool": { "command": "tool", "args": [] }
            }
        });
        std::fs::write(
            tmp.path().join(".claude.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        ).unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            let id = mgr.find_resource_id("my-tool");
            assert_eq!(id, Some("mcp:my-tool".to_string()));
        });
    }

    /// Helper: create a realistic .claude.json with multiple MCPs (mimics real user config)
    fn write_realistic_claude_json(dir: &Path) {
        let config = serde_json::json!({
            "numStartups": 42,
            "theme": "dark",
            "mcpServers": {
                "pencil": {
                    "command": "/tmp/pencil-mcp",
                    "args": ["--app", "desktop"],
                    "env": {},
                    "type": "stdio"
                },
                "github": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "type": "stdio"
                },
                "skill-manager": {
                    "command": "/home/user/.local/bin/skill-manager",
                    "args": ["mcp-serve"],
                    "description": "Skill Manager"
                }
            }
        });
        std::fs::write(
            dir.join(".claude.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        ).unwrap();
    }

    #[test]
    fn disable_mcp_removes_entry_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());
        let sm_data = tmp.path().join("sm-data");

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

            // Disable pencil
            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None).unwrap();

            // Verify: pencil entry removed from .claude.json
            let content: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap()
            ).unwrap();
            assert!(content["mcpServers"].get("pencil").is_none(),
                "pencil should be removed from config");

            // Verify: other entries untouched
            assert!(content["mcpServers"].get("github").is_some(),
                "github should still be in config");
            assert!(content["mcpServers"].get("skill-manager").is_some(),
                "skill-manager should still be in config");

            // Verify: non-MCP config preserved
            assert_eq!(content["theme"], "dark");
            assert_eq!(content["numStartups"], 42);

            // Verify: backup saved to mcp-backups dir
            let backup_path = sm_data.join("mcps").join("pencil.json");
            assert!(backup_path.exists(), "MCP config backup should exist");
            let backup: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&backup_path).unwrap()
            ).unwrap();
            assert_eq!(backup["command"], "/tmp/pencil-mcp");
            assert_eq!(backup["args"][0], "--app");
        });
    }

    #[test]
    fn enable_mcp_restores_entry_to_config() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());
        let sm_data = tmp.path().join("sm-data");

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

            // Disable then enable
            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None).unwrap();
            mgr.enable_resource("mcp:pencil", CliTarget::Claude, None).unwrap();

            // Verify: pencil is back in config with original fields
            let content: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap()
            ).unwrap();
            let pencil = content["mcpServers"].get("pencil")
                .expect("pencil should be restored");
            assert_eq!(pencil["command"], "/tmp/pencil-mcp");
            assert_eq!(pencil["args"][0], "--app");
            // Should NOT have disabled field
            assert!(pencil.get("disabled").is_none(),
                "restored MCP should not have disabled field");
        });
    }

    #[test]
    fn disable_mcp_after_disable_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());
        let sm_data = tmp.path().join("sm-data");

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None).unwrap();
            // Second disable should not error (already removed)
            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None).unwrap();

            // Backup should still be valid
            let backup_path = sm_data.join("mcps").join("pencil.json");
            assert!(backup_path.exists());
        });
    }

    #[test]
    fn disable_skill_manager_self_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            // Should refuse to disable itself
            let result = mgr.disable_resource("mcp:skill-manager", CliTarget::Claude, None);
            assert!(result.is_err(), "SM should refuse to disable itself");

            // Verify: skill-manager still in config
            let content: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap()
            ).unwrap();
            assert!(content["mcpServers"].get("skill-manager").is_some());
        });
    }

    #[test]
    fn disabled_mcp_still_visible_but_marked_disabled() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            // Before disable: 3 MCPs, all enabled
            let before = mgr.list_resources(Some(ResourceKind::Mcp), None).unwrap();
            assert_eq!(before.len(), 3);
            let pencil_before = before.iter().find(|r| r.name == "pencil").unwrap();
            assert!(pencil_before.is_enabled_for(CliTarget::Claude));

            // Disable pencil
            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None).unwrap();

            // After disable: still 3 MCPs, but pencil is disabled
            let after = mgr.list_resources(Some(ResourceKind::Mcp), None).unwrap();
            assert_eq!(after.len(), 3, "disabled MCP should still be visible");
            let pencil_after = after.iter().find(|r| r.name == "pencil")
                .expect("pencil should still appear in list");
            assert!(!pencil_after.is_enabled_for(CliTarget::Claude),
                "pencil should show as disabled");

            // Other MCPs unchanged
            let github = after.iter().find(|r| r.name == "github").unwrap();
            assert!(github.is_enabled_for(CliTarget::Claude));
        });
    }

    #[test]
    fn list_resources_mcp_reads_from_config_files() {
        let tmp = tempfile::tempdir().unwrap();

        // Write a .claude.json with MCPs — entry exists = enabled
        let config = serde_json::json!({
            "mcpServers": {
                "server-a": { "command": "a", "args": [] },
                "server-b": { "command": "b", "args": [] }
            }
        });
        std::fs::write(
            tmp.path().join(".claude.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        ).unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            let mcps = mgr.list_resources(
                Some(crate::core::resource::ResourceKind::Mcp), None
            ).unwrap();

            assert_eq!(mcps.len(), 2);
            let a = mcps.iter().find(|r| r.name == "server-a").unwrap();
            assert_eq!(a.id, "mcp:server-a");
            assert!(a.is_enabled_for(CliTarget::Claude));

            // Both entries exist = both enabled
            let b = mcps.iter().find(|r| r.name == "server-b").unwrap();
            assert!(b.is_enabled_for(CliTarget::Claude));
        });
    }
}

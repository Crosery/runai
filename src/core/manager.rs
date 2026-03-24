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
            Self::set_mcp_disabled(mcp_name, target, false)
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
            Self::set_mcp_disabled(mcp_name, target, true)
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

    /// Set `disabled` field on an MCP server in a CLI's config file.
    fn set_mcp_disabled(mcp_name: &str, target: CliTarget, disabled: bool) -> Result<()> {
        let home = dirs::home_dir().unwrap_or_default();
        let config_path = match target {
            CliTarget::Claude => home.join(".claude.json"),
            CliTarget::Gemini => home.join(".gemini/settings.json"),
            CliTarget::Codex => home.join(".codex/settings.json"),
            CliTarget::OpenCode => home.join(".opencode/settings.json"),
        };

        if !config_path.exists() { return Ok(()); }

        let content = std::fs::read_to_string(&config_path)?;
        let mut config: serde_json::Value = serde_json::from_str(&content)?;

        if let Some(servers) = config.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
            if let Some(server) = servers.get_mut(mcp_name).and_then(|s| s.as_object_mut()) {
                if disabled {
                    server.insert("disabled".into(), serde_json::Value::Bool(true));
                } else {
                    server.remove("disabled");
                }
                std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
            }
        }

        Ok(())
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
            for (name, server) in servers {
                if name.starts_with('_') { continue; }
                let disabled = server.get("disabled").and_then(|v| v.as_bool()).unwrap_or(false);
                result.entry(name.clone())
                    .or_default()
                    .insert(*target, !disabled);
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

        // MCPs: entirely from config files (sorted for stable order)
        if kind.is_none() || kind == Some(ResourceKind::Mcp) {
            let mcp_status = Self::read_mcp_status_from_configs();
            let mut mcp_names: Vec<_> = mcp_status.keys().collect();
            mcp_names.sort();
            for name in mcp_names {
                let targets = &mcp_status[name];
                if let Some(target) = enabled_for {
                    if !targets.get(&target).copied().unwrap_or(false) {
                        continue;
                    }
                }
                resources.push(Resource {
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
        }

        Ok(resources)
    }

    /// Check which CLI targets have this skill (symlink or direct dir in .agents/skills/ or skills/).
    fn check_skill_symlinks(&self, name: &str) -> HashMap<CliTarget, bool> {
        let mut map = HashMap::new();
        for target in CliTarget::ALL {
            // Check primary (.agents/skills/) and legacy (skills/) locations
            let primary = target.skills_dir().join(name);
            let legacy = target.legacy_skills_dir().join(name);
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
                let link = target.skills_dir().join(&skill.name);
                if Linker::is_our_symlink(&link, self.paths.data_dir()) {
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

    /// Count total skills (from DB) + total MCPs (from config files).
    pub fn resource_count(&self) -> (usize, usize) {
        let skills = self.db.skill_count().unwrap_or(0);
        let mcps = Self::read_mcp_status_from_configs().len();
        (skills, mcps)
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
    fn set_mcp_disabled_adds_disabled_true() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join(".claude.json");

        let config = serde_json::json!({
            "mcpServers": {
                "my-mcp": {
                    "command": "my-mcp",
                    "args": ["serve"]
                }
            }
        });
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        with_home(tmp.path(), || {
            SkillManager::set_mcp_disabled("my-mcp", CliTarget::Claude, true).unwrap();
        });

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert_eq!(
            content["mcpServers"]["my-mcp"]["disabled"],
            serde_json::Value::Bool(true),
        );
        // command field should still be present
        assert_eq!(
            content["mcpServers"]["my-mcp"]["command"],
            serde_json::Value::String("my-mcp".into()),
        );
    }

    #[test]
    fn set_mcp_disabled_removes_disabled_field_on_enable() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join(".claude.json");

        let config = serde_json::json!({
            "mcpServers": {
                "my-mcp": {
                    "command": "my-mcp",
                    "args": ["serve"],
                    "disabled": true
                }
            }
        });
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        with_home(tmp.path(), || {
            SkillManager::set_mcp_disabled("my-mcp", CliTarget::Claude, false).unwrap();
        });

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        // disabled field should be removed
        assert!(content["mcpServers"]["my-mcp"].get("disabled").is_none());
        // other fields preserved
        assert_eq!(
            content["mcpServers"]["my-mcp"]["command"],
            serde_json::Value::String("my-mcp".into()),
        );
    }

    #[test]
    fn set_mcp_disabled_preserves_other_servers() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join(".claude.json");

        let config = serde_json::json!({
            "mcpServers": {
                "server-a": {
                    "command": "a"
                },
                "server-b": {
                    "command": "b"
                }
            },
            "otherKey": "untouched"
        });
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        with_home(tmp.path(), || {
            SkillManager::set_mcp_disabled("server-a", CliTarget::Claude, true).unwrap();
        });

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        // server-a is disabled
        assert_eq!(content["mcpServers"]["server-a"]["disabled"], serde_json::Value::Bool(true));
        // server-b is unchanged (no disabled field)
        assert!(content["mcpServers"]["server-b"].get("disabled").is_none());
        assert_eq!(content["mcpServers"]["server-b"]["command"], serde_json::Value::String("b".into()));
        // top-level key preserved
        assert_eq!(content["otherKey"], serde_json::Value::String("untouched".into()));
    }

    #[test]
    fn set_mcp_disabled_nonexistent_mcp_does_not_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join(".claude.json");

        let config = serde_json::json!({
            "mcpServers": {
                "existing": { "command": "x" }
            }
        });
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        with_home(tmp.path(), || {
            // Should succeed silently — no such server in config
            let result = SkillManager::set_mcp_disabled("nonexistent", CliTarget::Claude, true);
            assert!(result.is_ok());
        });

        // Config should be unchanged (not even rewritten)
        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(content["mcpServers"]["existing"]["command"] == serde_json::Value::String("x".into()));
    }

    #[test]
    fn set_mcp_disabled_missing_config_file_does_not_crash() {
        let tmp = tempfile::tempdir().unwrap();
        // No .claude.json file exists

        with_home(tmp.path(), || {
            let result = SkillManager::set_mcp_disabled("anything", CliTarget::Claude, true);
            assert!(result.is_ok());
        });
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

    #[test]
    fn enable_disable_mcp_by_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "mcpServers": {
                "test-mcp": { "command": "test", "args": [], "disabled": true }
            }
        });
        let config_path = tmp.path().join(".claude.json");
        std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            mgr.enable_resource("mcp:test-mcp", CliTarget::Claude, None).unwrap();
            let content: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
            assert!(content["mcpServers"]["test-mcp"].get("disabled").is_none());

            mgr.disable_resource("mcp:test-mcp", CliTarget::Claude, None).unwrap();
            let content: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
            assert_eq!(content["mcpServers"]["test-mcp"]["disabled"], true);
        });
    }

    #[test]
    fn list_resources_mcp_reads_from_config_files() {
        let tmp = tempfile::tempdir().unwrap();

        // Write a fake .claude.json with MCPs
        let config = serde_json::json!({
            "mcpServers": {
                "server-a": { "command": "a", "args": [] },
                "server-b": { "command": "b", "args": [], "disabled": true }
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

            let b = mcps.iter().find(|r| r.name == "server-b").unwrap();
            assert!(!b.is_enabled_for(CliTarget::Claude));
        });
    }
}

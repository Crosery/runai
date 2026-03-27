use crate::core::classifier::Classifier;
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::group::Group;
use crate::core::linker::Linker;
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};
use crate::core::scanner::Scanner;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Convert a TOML MCP entry to JSON (for backup storage).
fn toml_to_json_mcp(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::Table(t) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in t {
                obj.insert(k.clone(), toml_val_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
        other => toml_val_to_json(other),
    }
}

fn toml_val_to_json(val: &toml::Value) -> serde_json::Value {
    match val {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Boolean(b) => serde_json::json!(b),
        toml::Value::Array(a) => serde_json::Value::Array(a.iter().map(toml_val_to_json).collect()),
        toml::Value::Table(t) => {
            let mut obj = serde_json::Map::new();
            for (k, v) in t {
                obj.insert(k.clone(), toml_val_to_json(v));
            }
            serde_json::Value::Object(obj)
        }
        _ => serde_json::Value::Null,
    }
}

/// Convert a JSON MCP entry to TOML (for restoring into config.toml).
fn json_to_toml_mcp(val: &serde_json::Value) -> toml::Value {
    match val {
        serde_json::Value::Object(obj) => {
            let mut table = toml::Table::new();
            for (k, v) in obj {
                table.insert(k.clone(), json_val_to_toml(v));
            }
            toml::Value::Table(table)
        }
        other => json_val_to_toml(other),
    }
}

fn json_val_to_toml(val: &serde_json::Value) -> toml::Value {
    match val {
        serde_json::Value::String(s) => toml::Value::String(s.clone()),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else {
                toml::Value::String(n.to_string())
            }
        }
        serde_json::Value::Bool(b) => toml::Value::Boolean(*b),
        serde_json::Value::Array(a) => toml::Value::Array(a.iter().map(json_val_to_toml).collect()),
        serde_json::Value::Object(obj) => {
            let mut table = toml::Table::new();
            for (k, v) in obj {
                table.insert(k.clone(), json_val_to_toml(v));
            }
            toml::Value::Table(table)
        }
        serde_json::Value::Null => toml::Value::String(String::new()),
    }
}

/// Convert JSON MCP backup to OpenCode format.
/// Handles both standard format (command=string, args=array) and
/// OpenCode native format (command=array) since backup may contain either.
fn json_to_opencode_mcp(entry: &serde_json::Value) -> serde_json::Value {
    let command_val = entry.get("command");

    let command_arr = if let Some(arr) = command_val.and_then(|v| v.as_array()) {
        // Already OpenCode format: command is an array
        arr.clone()
    } else {
        // Standard format: command is string, args is separate array
        let cmd = command_val
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let args: Vec<String> = entry
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let mut arr = vec![serde_json::Value::String(cmd)];
        arr.extend(args.into_iter().map(serde_json::Value::String));
        arr
    };

    serde_json::json!({
        "command": command_arr,
        "enabled": true,
        "type": "local",
    })
}

pub struct SkillManager {
    paths: AppPaths,
    db: Database,
}

impl SkillManager {
    pub fn new() -> Result<Self> {
        // Auto-migrate old "skill-manager" MCP entries to "runai" on first launch
        if let Some(home) = dirs::home_dir() {
            crate::core::mcp_register::McpRegister::migrate_all(&home);
        }

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
            usage_count: 0,
            last_used_at: None,
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
            let resource = self
                .db
                .get_resource(resource_id)?
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
            if mcp_name == "runai" || mcp_name == "skill-manager" {
                bail!("Cannot disable runai — it would remove its own MCP connection");
            }
            self.remove_mcp(mcp_name, target)
        } else {
            let resource = self
                .db
                .get_resource(resource_id)?
                .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;
            let cli_dir = cli_dir_override
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| target.skills_dir());
            let link_path = cli_dir.join(&resource.name);
            // Remove symlink regardless of target — handles both our managed dir
            // and legacy paths (e.g. old .skill-manager/ symlinks)
            if Linker::is_symlink(&link_path) {
                Linker::remove_link(&link_path)?;
            }
            Ok(())
        }
    }

    /// Disable MCP: save config to backup, remove entry from CLI config file.
    fn remove_mcp(&self, mcp_name: &str, target: CliTarget) -> Result<()> {
        let config_path = Self::cli_config_path(target);
        if !config_path.exists() {
            return Ok(());
        }

        let content = std::fs::read_to_string(&config_path)?;

        if target.uses_toml() {
            // Codex: TOML format
            let mut table: toml::Table = content.parse()?;
            if let Some(toml::Value::Table(servers)) = table.get_mut("mcp_servers") {
                if let Some(entry) = servers.remove(mcp_name) {
                    // Save backup as JSON for cross-format restore
                    let backup_dir = self.paths.mcps_dir();
                    std::fs::create_dir_all(&backup_dir)?;
                    let backup_path = backup_dir.join(format!("{mcp_name}.json"));
                    let json_entry = toml_to_json_mcp(&entry);
                    std::fs::write(&backup_path, serde_json::to_string_pretty(&json_entry)?)?;

                    std::fs::write(&config_path, toml::to_string_pretty(&table)?)?;
                }
            }
        } else {
            // JSON format (Claude/Gemini/OpenCode)
            let mut config: serde_json::Value = serde_json::from_str(&content)?;
            let mcp_key = if target.uses_opencode_format() {
                "mcp"
            } else {
                "mcpServers"
            };
            if let Some(servers) = config.get_mut(mcp_key).and_then(|s| s.as_object_mut()) {
                if let Some(entry) = servers.remove(mcp_name) {
                    let backup_dir = self.paths.mcps_dir();
                    std::fs::create_dir_all(&backup_dir)?;
                    let backup_path = backup_dir.join(format!("{mcp_name}.json"));
                    std::fs::write(&backup_path, serde_json::to_string_pretty(&entry)?)?;

                    std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
                }
            }
        }

        Ok(())
    }

    /// Enable MCP: restore saved config back into CLI config file.
    fn restore_mcp(&self, mcp_name: &str, target: CliTarget) -> Result<()> {
        let config_path = Self::cli_config_path(target);

        // Read backup
        let backup_path = self.paths.mcps_dir().join(format!("{mcp_name}.json"));
        if !backup_path.exists() {
            bail!(
                "No saved config for MCP '{mcp_name}'. Use 'claude mcp add' to register it first."
            );
        }
        let backup_content = std::fs::read_to_string(&backup_path)?;
        let mut entry: serde_json::Value = serde_json::from_str(&backup_content)?;

        // Remove disabled field if present (clean restore)
        if let Some(obj) = entry.as_object_mut() {
            obj.remove("disabled");
        }

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if target.uses_toml() {
            // Codex: restore into TOML config.toml
            let mut table: toml::Table = if config_path.exists() {
                std::fs::read_to_string(&config_path)?.parse()?
            } else {
                toml::Table::new()
            };
            let servers = table
                .entry("mcp_servers")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(s) = servers {
                s.insert(mcp_name.to_string(), json_to_toml_mcp(&entry));
            }
            std::fs::write(&config_path, toml::to_string_pretty(&table)?)?;
        } else {
            // JSON format
            let mut config: serde_json::Value = if config_path.exists() {
                serde_json::from_str(&std::fs::read_to_string(&config_path)?)?
            } else {
                serde_json::json!({})
            };

            let mcp_key = if target.uses_opencode_format() {
                "mcp"
            } else {
                "mcpServers"
            };

            // For OpenCode, convert backup JSON to OpenCode format
            if target.uses_opencode_format() {
                entry = json_to_opencode_mcp(&entry);
            }

            let servers = config
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("config is not an object"))?
                .entry(mcp_key)
                .or_insert_with(|| serde_json::json!({}));

            servers
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("{mcp_key} is not an object"))?
                .insert(mcp_name.to_string(), entry);

            std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
        }
        Ok(())
    }

    fn cli_config_path(target: CliTarget) -> PathBuf {
        target.mcp_config_path()
    }

    /// Read MCP enabled/disabled status directly from CLI config files.
    /// Returns mcp_name -> { target -> enabled }.
    pub fn read_mcp_status_from_configs() -> HashMap<String, HashMap<CliTarget, bool>> {
        let mut result: HashMap<String, HashMap<CliTarget, bool>> = HashMap::new();

        for target in CliTarget::ALL {
            let path = target.mcp_config_path();
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            if target.uses_toml() {
                // Codex: parse TOML, look for [mcp_servers.*]
                let table: toml::Table = match content.parse() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if let Some(toml::Value::Table(servers)) = table.get("mcp_servers") {
                    for name in servers.keys() {
                        if name.starts_with('_') {
                            continue;
                        }
                        result
                            .entry(name.clone())
                            .or_default()
                            .insert(*target, true);
                    }
                }
            } else if target.uses_opencode_format() {
                // OpenCode: key="mcp", command=array, has "enabled" field
                let config: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let servers = match config.get("mcp").and_then(|s| s.as_object()) {
                    Some(s) => s,
                    None => continue,
                };
                for (name, server) in servers {
                    if name.starts_with('_') {
                        continue;
                    }
                    // OpenCode has explicit enabled field; default true if absent
                    let enabled = server
                        .get("enabled")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true);
                    if enabled {
                        result
                            .entry(name.clone())
                            .or_default()
                            .insert(*target, true);
                    }
                }
            } else {
                // JSON: Claude/Gemini (mcpServers key)
                let config: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let servers = match config.get("mcpServers").and_then(|s| s.as_object()) {
                    Some(s) => s,
                    None => continue,
                };
                for (name, _server) in servers {
                    if name.starts_with('_') {
                        continue;
                    }
                    result
                        .entry(name.clone())
                        .or_default()
                        .insert(*target, true);
                }
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
                });
            }

            // 2. Disabled MCPs from backup dir (removed from config by SM)
            let mcps_dir = self.paths.mcps_dir();
            if mcps_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&mcps_dir) {
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
            let primary = target.skills_dir().join(name);
            let legacy = target.agents_skills_dir().join(name);
            // Use symlink_metadata (doesn't follow symlink) to detect even broken symlinks,
            // plus exists() for real directories
            let enabled = primary.symlink_metadata().is_ok() || legacy.symlink_metadata().is_ok();
            map.insert(*target, enabled);
        }
        map
    }

    pub fn uninstall(&self, resource_id: &str) -> Result<()> {
        let resource = self
            .db
            .get_resource(resource_id)?
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
        let mcp_enabled = mcp_status
            .values()
            .filter(|targets| targets.get(&target).copied().unwrap_or(false))
            .count();
        Ok((skill_enabled, mcp_enabled))
    }

    // --- Install from GitHub ---

    /// Install skills from a GitHub repo, register in DB, create group, enable for target.
    /// Uses Market API: first discovers skills via git tree, then downloads each via Contents API.
    /// Returns (group_id, skill_names).
    pub fn install_github_repo(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        target: CliTarget,
    ) -> Result<(String, Vec<String>)> {
        use crate::core::market::{Market, SourceEntry};

        let source = SourceEntry::from_input(&format!("{owner}/{repo}@{branch}"))?;
        let rt = tokio::runtime::Runtime::new()?;

        // Step 1: Discover skills via git tree API (fast, single request)
        let extract = rt.block_on(Market::fetch(&source))?;

        if extract.plugin_detected && extract.skills.is_empty() {
            bail!(
                "This is a Claude Code plugin, not a skill collection.\n\
                   Install with: /plugin install {repo}@<marketplace>"
            );
        }
        if extract.skills.is_empty() {
            bail!("No skills found in {owner}/{repo}");
        }

        // Step 2: Download ALL files across ALL skills concurrently
        let tasks = Market::collect_download_tasks(&extract, self.paths());
        let downloaded = rt.block_on(Market::execute_downloads(tasks));

        if downloaded.is_empty() {
            bail!("All skill downloads failed for {owner}/{repo}");
        }

        // Step 3: Register downloaded skills in DB + enable
        let mut skill_names: Vec<String> = downloaded.into_iter().collect();
        skill_names.sort();
        for name in &skill_names {
            let resource_id = format!("github:{owner}/{repo}:{name}");
            let dir = self.paths.skills_dir().join(name);
            let description = Self::extract_description(&dir);
            let resource = Resource {
                id: resource_id.clone(),
                name: name.clone(),
                kind: ResourceKind::Skill,
                description,
                directory: dir,
                source: Source::GitHub {
                    owner: owner.to_string(),
                    repo: repo.to_string(),
                    branch: branch.to_string(),
                },
                installed_at: chrono::Utc::now().timestamp(),
                enabled: HashMap::new(),
                usage_count: 0,
                last_used_at: None,
            };
            let _ = self.db.insert_resource(&resource);
            let _ = self.enable_resource(&resource_id, target, None);
        }

        // Step 4: Auto-create group
        let group_id = repo.to_lowercase();
        let group = crate::core::group::Group {
            name: repo.to_string(),
            description: format!("Skills from {owner}/{repo}"),
            kind: crate::core::group::GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        let _ = self.create_group(&group_id, &group);

        for name in &skill_names {
            let rid = format!("github:{owner}/{repo}:{name}");
            let _ = self.db.add_group_member(&group_id, &rid);
        }

        Ok((group_id, skill_names))
    }

    /// Register already-downloaded skills (in managed dir) and create group.
    /// Used by install_github_repo after download, and testable without network.
    pub fn register_and_group_skills(
        &self,
        skill_names: &[String],
        group_id: &str,
        group_name: &str,
        target: CliTarget,
    ) -> Result<usize> {
        let mut registered = 0;

        // Create group
        let group = crate::core::group::Group {
            name: group_name.to_string(),
            description: format!("Skills group: {group_name}"),
            kind: crate::core::group::GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        let _ = self.create_group(group_id, &group);

        for name in skill_names {
            let dir = self.paths.skills_dir().join(name);
            if !dir.exists() {
                continue;
            }

            let description = Self::extract_description(&dir);
            let resource_id = format!("local:{name}");
            let resource = Resource {
                id: resource_id.clone(),
                name: name.clone(),
                kind: ResourceKind::Skill,
                description,
                directory: dir,
                source: Source::Local {
                    path: self.paths.skills_dir().join(name),
                },
                installed_at: chrono::Utc::now().timestamp(),
                enabled: HashMap::new(),
                usage_count: 0,
                last_used_at: None,
            };
            if self.db.insert_resource(&resource).is_ok() {
                let _ = self.enable_resource(&resource_id, target, None);
                let _ = self.db.add_group_member(group_id, &resource_id);
                registered += 1;
            }
        }

        Ok(registered)
    }

    // --- Batch operations ---

    /// Delete multiple resources by name. Returns (deleted_count, errors).
    pub fn batch_delete(&self, names: &[String]) -> Result<(usize, Vec<String>)> {
        let mut deleted = 0;
        let mut errors = Vec::new();
        for name in names {
            match self.find_resource_id(name) {
                Some(id) => match self.uninstall(&id) {
                    Ok(_) => deleted += 1,
                    Err(e) => errors.push(format!("{name}: {e}")),
                },
                None => errors.push(format!("{name}: not found")),
            }
        }
        Ok((deleted, errors))
    }

    // --- Usage tracking ---

    /// Record a usage event for a resource by name.
    pub fn record_usage(&self, name: &str) -> Result<()> {
        let id = self
            .find_resource_id(name)
            .ok_or_else(|| anyhow::anyhow!("resource not found: {name}"))?;
        let affected = self.db.record_usage(&id)?;
        if affected == 0 {
            bail!("resource not found in DB: {id}");
        }
        Ok(())
    }

    /// Get usage stats for all DB resources, sorted by usage_count DESC.
    pub fn usage_stats(&self) -> Result<Vec<crate::core::resource::UsageStat>> {
        self.db.get_usage_stats()
    }

    // --- Internal ---

    fn extract_description(skill_dir: &Path) -> String {
        Scanner::extract_description(skill_dir)
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
        )
        .unwrap();

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
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            mgr.db()
                .add_group_member("test-group", "mcp:my-mcp")
                .unwrap();

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
        )
        .unwrap();

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
                "runai": {
                    "command": "/home/user/.local/bin/runai",
                    "args": ["mcp-serve"],
                    "description": "Runai — AI skill manager"
                }
            }
        });
        std::fs::write(
            dir.join(".claude.json"),
            serde_json::to_string_pretty(&config).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn disable_mcp_removes_entry_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());
        let sm_data = tmp.path().join("sm-data");

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

            // Disable pencil
            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
                .unwrap();

            // Verify: pencil entry removed from .claude.json
            let content: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
            )
            .unwrap();
            assert!(
                content["mcpServers"].get("pencil").is_none(),
                "pencil should be removed from config"
            );

            // Verify: other entries untouched
            assert!(
                content["mcpServers"].get("github").is_some(),
                "github should still be in config"
            );
            assert!(
                content["mcpServers"].get("runai").is_some(),
                "runai should still be in config"
            );

            // Verify: non-MCP config preserved
            assert_eq!(content["theme"], "dark");
            assert_eq!(content["numStartups"], 42);

            // Verify: backup saved to mcp-backups dir
            let backup_path = sm_data.join("mcps").join("pencil.json");
            assert!(backup_path.exists(), "MCP config backup should exist");
            let backup: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&backup_path).unwrap()).unwrap();
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
            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
                .unwrap();
            mgr.enable_resource("mcp:pencil", CliTarget::Claude, None)
                .unwrap();

            // Verify: pencil is back in config with original fields
            let content: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
            )
            .unwrap();
            let pencil = content["mcpServers"]
                .get("pencil")
                .expect("pencil should be restored");
            assert_eq!(pencil["command"], "/tmp/pencil-mcp");
            assert_eq!(pencil["args"][0], "--app");
            // Should NOT have disabled field
            assert!(
                pencil.get("disabled").is_none(),
                "restored MCP should not have disabled field"
            );
        });
    }

    #[test]
    fn disable_mcp_after_disable_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());
        let sm_data = tmp.path().join("sm-data");

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
                .unwrap();
            // Second disable should not error (already removed)
            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
                .unwrap();

            // Backup should still be valid
            let backup_path = sm_data.join("mcps").join("pencil.json");
            assert!(backup_path.exists());
        });
    }

    #[test]
    fn disable_rune_self_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            // Should refuse to disable itself
            let result = mgr.disable_resource("mcp:runai", CliTarget::Claude, None);
            assert!(result.is_err(), "Runai should refuse to disable itself");

            // Verify: runai still in config
            let content: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
            )
            .unwrap();
            assert!(content["mcpServers"].get("runai").is_some());
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
            mgr.disable_resource("mcp:pencil", CliTarget::Claude, None)
                .unwrap();

            // After disable: still 3 MCPs, but pencil is disabled
            let after = mgr.list_resources(Some(ResourceKind::Mcp), None).unwrap();
            assert_eq!(after.len(), 3, "disabled MCP should still be visible");
            let pencil_after = after
                .iter()
                .find(|r| r.name == "pencil")
                .expect("pencil should still appear in list");
            assert!(
                !pencil_after.is_enabled_for(CliTarget::Claude),
                "pencil should show as disabled"
            );

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
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            let mcps = mgr
                .list_resources(Some(crate::core::resource::ResourceKind::Mcp), None)
                .unwrap();

            assert_eq!(mcps.len(), 2);
            let a = mcps.iter().find(|r| r.name == "server-a").unwrap();
            assert_eq!(a.id, "mcp:server-a");
            assert!(a.is_enabled_for(CliTarget::Claude));

            // Both entries exist = both enabled
            let b = mcps.iter().find(|r| r.name == "server-b").unwrap();
            assert!(b.is_enabled_for(CliTarget::Claude));
        });
    }

    #[test]
    fn register_and_group_skills_creates_group_and_enables() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");

        // Create fake managed skill dirs with realistic SKILL.md
        let skills_dir = sm_data.join("skills");
        std::fs::create_dir_all(skills_dir.join("debugging")).unwrap();
        std::fs::write(skills_dir.join("debugging/SKILL.md"),
            "---\nname: debugging\ndescription: \"Systematic debugging skill\"\n---\n\n# Debugging\n").unwrap();
        std::fs::create_dir_all(skills_dir.join("tdd")).unwrap();
        std::fs::write(
            skills_dir.join("tdd/SKILL.md"),
            "---\nname: tdd\ndescription: \"Test-driven development\"\n---\n\n# TDD\n",
        )
        .unwrap();

        // Also create the skills_dir for symlinking
        let claude_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&claude_skills).unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

            let count = mgr
                .register_and_group_skills(
                    &["debugging".into(), "tdd".into()],
                    "my-toolkit",
                    "My Toolkit",
                    CliTarget::Claude,
                )
                .unwrap();

            assert_eq!(count, 2, "should register 2 skills");

            // Group created with members
            let members = mgr.get_group_members("my-toolkit").unwrap();
            assert_eq!(members.len(), 2);

            // Skills enabled (symlinks created)
            assert!(
                claude_skills.join("debugging").exists(),
                "debugging symlink should exist"
            );
            assert!(
                claude_skills.join("tdd").exists(),
                "tdd symlink should exist"
            );

            // Descriptions parsed from frontmatter
            let resources = mgr.list_resources(Some(ResourceKind::Skill), None).unwrap();
            let dbg = resources.iter().find(|r| r.name == "debugging").unwrap();
            assert_eq!(dbg.description, "Systematic debugging skill");
        });
    }

    #[test]
    fn update_group_name_only() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            let group = crate::core::group::Group {
                name: "Old Name".into(),
                description: "old desc".into(),
                kind: crate::core::group::GroupKind::Custom,
                auto_enable: false,
                members: vec![],
            };
            mgr.create_group("my-group", &group).unwrap();

            // Update name only
            mgr.update_group("my-group", Some("New Name"), None)
                .unwrap();

            let groups = mgr.list_groups().unwrap();
            let (_, g) = groups.iter().find(|(id, _)| id == "my-group").unwrap();
            assert_eq!(g.name, "New Name");
            assert_eq!(g.description, "old desc"); // unchanged
        });
    }

    #[test]
    fn update_group_description_only() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            let group = crate::core::group::Group {
                name: "My Group".into(),
                description: "old desc".into(),
                kind: crate::core::group::GroupKind::Custom,
                auto_enable: false,
                members: vec![],
            };
            mgr.create_group("my-group", &group).unwrap();

            // Update description only
            mgr.update_group("my-group", None, Some("new desc"))
                .unwrap();

            let groups = mgr.list_groups().unwrap();
            let (_, g) = groups.iter().find(|(id, _)| id == "my-group").unwrap();
            assert_eq!(g.name, "My Group"); // unchanged
            assert_eq!(g.description, "new desc");
        });
    }

    #[test]
    fn update_group_both() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            let group = crate::core::group::Group {
                name: "Old".into(),
                description: "old".into(),
                kind: crate::core::group::GroupKind::Custom,
                auto_enable: false,
                members: vec![],
            };
            mgr.create_group("g1", &group).unwrap();

            mgr.update_group("g1", Some("New"), Some("new")).unwrap();

            let groups = mgr.list_groups().unwrap();
            let (_, g) = groups.iter().find(|(id, _)| id == "g1").unwrap();
            assert_eq!(g.name, "New");
            assert_eq!(g.description, "new");
        });
    }

    #[test]
    fn update_nonexistent_group_fails() {
        let tmp = tempfile::tempdir().unwrap();
        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            let result = mgr.update_group("nonexistent", Some("x"), None);
            assert!(result.is_err());
        });
    }

    #[test]
    fn batch_delete_removes_multiple_resources() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        let skills_dir = sm_data.join("skills");
        for name in &["skill-a", "skill-b", "skill-c"] {
            std::fs::create_dir_all(skills_dir.join(name)).unwrap();
            std::fs::write(skills_dir.join(format!("{name}/SKILL.md")), "# X\n").unwrap();
        }

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            for name in &["skill-a", "skill-b", "skill-c"] {
                mgr.register_local_skill(name).unwrap();
            }

            let result =
                mgr.batch_delete(&["skill-a".into(), "skill-b".into(), "nonexistent".into()]);
            let (deleted, errors) = result.unwrap();
            assert_eq!(deleted, 2);
            assert_eq!(errors.len(), 1); // nonexistent

            // skill-c should still exist
            assert!(mgr.find_resource_id("skill-c").is_some());
            assert!(mgr.find_resource_id("skill-a").is_none());
            assert!(mgr.find_resource_id("skill-b").is_none());
        });
    }

    #[test]
    fn record_usage_by_name_increments_count() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        let skills_dir = sm_data.join("skills");
        std::fs::create_dir_all(skills_dir.join("my-skill")).unwrap();
        std::fs::write(skills_dir.join("my-skill/SKILL.md"), "# My Skill\n").unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            mgr.register_local_skill("my-skill").unwrap();

            mgr.record_usage("my-skill").unwrap();
            mgr.record_usage("my-skill").unwrap();

            let stats = mgr.usage_stats().unwrap();
            let entry = stats.iter().find(|s| s.name == "my-skill").unwrap();
            assert_eq!(entry.count, 2);
            assert!(entry.last_used_at.is_some());
        });
    }

    #[test]
    fn record_usage_unknown_name_errors() {
        let tmp = tempfile::tempdir().unwrap();
        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            let result = mgr.record_usage("nonexistent");
            assert!(result.is_err());
        });
    }

    #[test]
    fn usage_stats_returns_all_resources_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        let skills_dir = sm_data.join("skills");
        for name in &["alpha", "beta"] {
            std::fs::create_dir_all(skills_dir.join(name)).unwrap();
            std::fs::write(skills_dir.join(format!("{name}/SKILL.md")), "# Skill\n").unwrap();
        }

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            mgr.register_local_skill("alpha").unwrap();
            mgr.register_local_skill("beta").unwrap();

            mgr.record_usage("beta").unwrap();
            mgr.record_usage("beta").unwrap();
            mgr.record_usage("alpha").unwrap();

            let stats = mgr.usage_stats().unwrap();
            assert_eq!(stats.len(), 2);
            assert_eq!(stats[0].name, "beta"); // 2 uses, should be first
            assert_eq!(stats[1].name, "alpha"); // 1 use
        });
    }

    #[test]
    fn disable_enable_mcp_on_codex_target() {
        let tmp = tempfile::tempdir().unwrap();
        // Create codex config with TOML format
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            r#"
[mcp_servers.test-mcp]
type = "stdio"
command = "test-cmd"
args = ["--flag"]
"#,
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            // Disable MCP on codex
            mgr.disable_resource("mcp:test-mcp", CliTarget::Codex, None)
                .unwrap();

            // Config should have entry removed
            let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
            assert!(
                !content.contains("[mcp_servers.test-mcp]"),
                "test-mcp should be removed from TOML"
            );

            // Re-enable
            mgr.enable_resource("mcp:test-mcp", CliTarget::Codex, None)
                .unwrap();

            let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
            assert!(
                content.contains("[mcp_servers.test-mcp]"),
                "test-mcp should be restored to TOML"
            );
            assert!(content.contains("test-cmd"), "command should be restored");
        });
    }

    #[test]
    fn enable_mcp_creates_config_for_missing_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");

        // First create a backup for the MCP (simulate previous disable)
        let mcps_dir = sm_data.join("mcps");
        std::fs::create_dir_all(&mcps_dir).unwrap();
        std::fs::write(
            mcps_dir.join("my-mcp.json"),
            r#"{"command":"my-cmd","args":[]}"#,
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

            // Enable on gemini — no .gemini/settings.json exists yet
            mgr.enable_resource("mcp:my-mcp", CliTarget::Gemini, None)
                .unwrap();

            // Config file should now exist with the MCP entry
            let gemini_config = tmp.path().join(".gemini").join("settings.json");
            assert!(gemini_config.exists(), "gemini config should be created");

            let content: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&gemini_config).unwrap()).unwrap();
            assert!(content["mcpServers"]["my-mcp"].is_object());
        });
    }

    #[test]
    fn read_mcp_status_from_multiple_clis() {
        let tmp = tempfile::tempdir().unwrap();

        // Claude config (JSON)
        let claude_config = serde_json::json!({
            "mcpServers": { "shared-mcp": { "command": "x" } }
        });
        std::fs::write(
            tmp.path().join(".claude.json"),
            serde_json::to_string_pretty(&claude_config).unwrap(),
        )
        .unwrap();

        // Codex config (TOML)
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            r#"
[mcp_servers.shared-mcp]
type = "stdio"
command = "x"

[mcp_servers.codex-only]
type = "stdio"
command = "y"
"#,
        )
        .unwrap();

        with_home(tmp.path(), || {
            let status = SkillManager::read_mcp_status_from_configs();

            // shared-mcp enabled on both claude and codex
            let shared = status.get("shared-mcp").unwrap();
            assert!(shared.get(&CliTarget::Claude).copied().unwrap_or(false));
            assert!(shared.get(&CliTarget::Codex).copied().unwrap_or(false));

            // codex-only only on codex
            let codex_only = status.get("codex-only").unwrap();
            assert!(!codex_only.get(&CliTarget::Claude).copied().unwrap_or(false));
            assert!(codex_only.get(&CliTarget::Codex).copied().unwrap_or(false));
        });
    }

    #[test]
    fn read_mcp_status_reads_codex_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            r#"
model = "gpt-5"

[mcp_servers.pencil]
type = "stdio"
command = "npx"
args = ["-y", "@anthropic-ai/pencil-mcp"]

[mcp_servers.github]
type = "stdio"
command = "gh-mcp"
args = []
"#,
        )
        .unwrap();

        with_home(tmp.path(), || {
            let status = SkillManager::read_mcp_status_from_configs();
            let pencil = status.get("pencil").unwrap();
            assert!(pencil.get(&CliTarget::Codex).copied().unwrap_or(false));
            let github = status.get("github").unwrap();
            assert!(github.get(&CliTarget::Codex).copied().unwrap_or(false));
        });
    }

    #[test]
    fn disable_enable_mcp_on_codex_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            r#"
model = "gpt-5"

[mcp_servers.pencil]
type = "stdio"
command = "npx"
args = ["-y", "@anthropic-ai/pencil-mcp"]

[mcp_servers.github]
type = "stdio"
command = "gh-mcp"
args = []
"#,
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            // Disable pencil on codex
            mgr.disable_resource("mcp:pencil", CliTarget::Codex, None)
                .unwrap();

            // pencil should be removed from config.toml
            let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
            assert!(
                !content.contains("[mcp_servers.pencil]"),
                "pencil should be removed from TOML"
            );
            // github should still be there
            assert!(
                content.contains("[mcp_servers.github]"),
                "github should remain in TOML"
            );
            // model should be preserved
            assert!(
                content.contains("model"),
                "non-MCP config should be preserved"
            );

            // Re-enable pencil
            mgr.enable_resource("mcp:pencil", CliTarget::Codex, None)
                .unwrap();

            let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
            assert!(
                content.contains("[mcp_servers.pencil]"),
                "pencil should be restored to TOML"
            );
        });
    }

    #[test]
    fn register_codex_writes_toml() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("config.toml"), "model = \"gpt-5\"\n").unwrap();

        let result = crate::core::mcp_register::McpRegister::register_all(tmp.path());
        assert!(
            result.registered.contains(&"codex".to_string()),
            "codex should be registered"
        );

        let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
        assert!(
            content.contains("[mcp_servers.runai]"),
            "runai should be in TOML"
        );
        assert!(
            content.contains("mcp-serve"),
            "mcp-serve arg should be present"
        );
        // Non-MCP config preserved
        assert!(content.contains("model"), "existing config preserved");
    }

    // --- OpenCode tests ---

    #[test]
    fn read_mcp_status_reads_opencode_format() {
        let tmp = tempfile::tempdir().unwrap();
        let oc_dir = tmp.path().join(".config").join("opencode");
        std::fs::create_dir_all(&oc_dir).unwrap();
        std::fs::write(
            oc_dir.join("opencode.json"),
            r#"{
                "mcp": {
                    "pencil": {
                        "command": ["npx", "-y", "@anthropic-ai/pencil-mcp"],
                        "enabled": true,
                        "type": "local"
                    },
                    "disabled-one": {
                        "command": ["node", "server.js"],
                        "enabled": false,
                        "type": "local"
                    }
                }
            }"#,
        )
        .unwrap();

        with_home(tmp.path(), || {
            let status = SkillManager::read_mcp_status_from_configs();
            // pencil should be detected as enabled on OpenCode
            let pencil = status.get("pencil").unwrap();
            assert!(
                pencil.get(&CliTarget::OpenCode).copied().unwrap_or(false),
                "pencil should be enabled for opencode"
            );
            // disabled-one should NOT be in status (enabled=false)
            let disabled = status.get("disabled-one");
            let oc_enabled = disabled
                .and_then(|m| m.get(&CliTarget::OpenCode))
                .copied()
                .unwrap_or(false);
            assert!(!oc_enabled, "disabled MCP should not show as enabled");
        });
    }

    #[test]
    fn disable_enable_mcp_on_opencode() {
        let tmp = tempfile::tempdir().unwrap();
        let oc_dir = tmp.path().join(".config").join("opencode");
        std::fs::create_dir_all(&oc_dir).unwrap();
        std::fs::write(
            oc_dir.join("opencode.json"),
            r#"{
                "mcp": {
                    "pencil": {
                        "command": ["npx", "-y", "@anthropic-ai/pencil-mcp"],
                        "enabled": true,
                        "type": "local"
                    },
                    "other": {
                        "command": ["other-cmd"],
                        "enabled": true,
                        "type": "local"
                    }
                }
            }"#,
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            // Disable pencil
            mgr.disable_resource("mcp:pencil", CliTarget::OpenCode, None)
                .unwrap();

            let content: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(oc_dir.join("opencode.json")).unwrap(),
            )
            .unwrap();
            // pencil should be removed from mcp
            assert!(
                content["mcp"].get("pencil").is_none(),
                "pencil should be removed"
            );
            // other should remain
            assert!(content["mcp"]["other"].is_object(), "other should remain");

            // Re-enable
            mgr.enable_resource("mcp:pencil", CliTarget::OpenCode, None)
                .unwrap();

            let content: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(oc_dir.join("opencode.json")).unwrap(),
            )
            .unwrap();
            let pencil = &content["mcp"]["pencil"];
            assert!(pencil.is_object(), "pencil should be restored");
            // Command array must be preserved correctly
            let cmd = pencil["command"]
                .as_array()
                .expect("command should be array");
            assert_eq!(cmd[0], "npx", "first element should be npx");
            assert_eq!(cmd[1], "-y");
            assert_eq!(cmd[2], "@anthropic-ai/pencil-mcp");
            assert_eq!(pencil["enabled"], true);
            assert_eq!(pencil["type"], "local");
        });
    }

    #[test]
    fn list_resources_deduplicates_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        let skills_dir = sm_data.join("skills");
        std::fs::create_dir_all(skills_dir.join("dupe")).unwrap();
        std::fs::write(skills_dir.join("dupe/SKILL.md"), "# Dupe").unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            // Register same name with two different IDs
            mgr.register_local_skill("dupe").unwrap();
            // Manually insert a second resource with different ID but same name
            let source = crate::core::resource::Source::Adopted {
                original_cli: "codex".into(),
            };
            let res = crate::core::resource::Resource {
                id: "adopted:dupe".into(),
                name: "dupe".into(),
                kind: crate::core::resource::ResourceKind::Skill,
                description: "duplicate".into(),
                directory: skills_dir.join("dupe"),
                source,
                installed_at: 0,
                enabled: std::collections::HashMap::new(),
                usage_count: 0,
                last_used_at: None,
            };
            mgr.db().insert_resource(&res).unwrap();

            let skills = mgr
                .list_resources(Some(crate::core::resource::ResourceKind::Skill), None)
                .unwrap();
            let dupe_count = skills.iter().filter(|s| s.name == "dupe").count();
            assert_eq!(
                dupe_count, 1,
                "should deduplicate by name, got {dupe_count}"
            );
        });
    }

    #[test]
    fn check_symlinks_uses_is_symlink_not_exists() {
        // Verifies that a symlink whose target doesn't exist is still detected
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        let skills_dir = sm_data.join("skills");
        std::fs::create_dir_all(skills_dir.join("test-skill")).unwrap();
        std::fs::write(skills_dir.join("test-skill/SKILL.md"), "# Test").unwrap();

        // Create CLI skills dir with a broken symlink (target doesn't exist)
        let claude_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&claude_skills).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            "/nonexistent/path/test-skill",
            claude_skills.join("test-skill"),
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            mgr.register_local_skill("test-skill").unwrap();

            let skills = mgr
                .list_resources(Some(crate::core::resource::ResourceKind::Skill), None)
                .unwrap();
            let skill = skills.iter().find(|s| s.name == "test-skill").unwrap();
            // Even though symlink target is broken, skill should show as enabled
            // because a symlink EXISTS in the CLI skills dir
            assert!(
                skill.is_enabled_for(CliTarget::Claude),
                "broken symlink should still count as enabled"
            );
        });
    }

    #[test]
    fn register_opencode_writes_correct_format() {
        let tmp = tempfile::tempdir().unwrap();
        let oc_dir = tmp.path().join(".config").join("opencode");
        std::fs::create_dir_all(&oc_dir).unwrap();
        std::fs::write(oc_dir.join("opencode.json"), r#"{"provider":{}}"#).unwrap();

        let result = crate::core::mcp_register::McpRegister::register_all(tmp.path());
        assert!(
            result.registered.contains(&"opencode".to_string()),
            "opencode should be registered"
        );

        let content: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(oc_dir.join("opencode.json")).unwrap())
                .unwrap();
        let sm = &content["mcp"]["runai"];
        assert!(sm.is_object(), "runai should be in mcp");
        // command should be an array (OpenCode format)
        assert!(sm["command"].is_array(), "command should be array");
        assert_eq!(sm["type"], "local");
        assert_eq!(sm["enabled"], true);
        // provider should be preserved
        assert!(content["provider"].is_object(), "existing config preserved");
    }

    #[test]
    fn disable_skill_removes_any_symlink_not_just_ours() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        let skills_dir = sm_data.join("skills");
        std::fs::create_dir_all(skills_dir.join("test-skill")).unwrap();
        std::fs::write(skills_dir.join("test-skill/SKILL.md"), "# Test").unwrap();

        // Create CLI skills dir with a symlink pointing to some OTHER path (not our managed dir)
        let claude_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&claude_skills).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            "/some/other/path/test-skill",
            claude_skills.join("test-skill"),
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            mgr.register_local_skill("test-skill").unwrap();

            // Should be detected as enabled (symlink exists)
            let skills = mgr.list_resources(Some(ResourceKind::Skill), None).unwrap();
            let skill = skills.iter().find(|s| s.name == "test-skill").unwrap();
            assert!(skill.is_enabled_for(CliTarget::Claude));

            // Disable should work even though symlink doesn't point to our managed dir
            mgr.disable_resource(&skill.id, CliTarget::Claude, None)
                .unwrap();

            // Symlink should be gone
            assert!(
                !claude_skills.join("test-skill").symlink_metadata().is_ok(),
                "symlink should be removed"
            );
        });
    }
}

use crate::core::classifier::Classifier;
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::group::Group;
use crate::core::linker::Linker;
use crate::core::mcp_canonical::{
    canonical_to_codex_toml, codex_toml_to_canonical, from_canonical_for_json_target, is_corrupt,
    to_canonical,
};
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source, TrashEntry};
use crate::core::scanner::Scanner;
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
        // Normalize MCP backups to canonical shape; quarantine corrupt ones.
        let _ = Self::migrate_mcp_backups(&paths);
        let db = Database::open(&paths.db_path())?;
        // Silent dedupe at startup: collapse any duplicate skill rows that
        // accumulated from prior installs/adopts. The header showing
        // `0/280 skills` while the list shows 278 is exactly this — fix at
        // load time so the user never sees the divergence again.
        let _ = db.dedupe_skills_by_name();
        Ok(Self { paths, db })
    }

    pub fn with_base(base: PathBuf) -> Result<Self> {
        let paths = AppPaths::with_base(base);
        paths.ensure_dirs()?;
        let _ = Self::migrate_mcp_backups(&paths);
        let db = Database::open(&paths.db_path())?;
        let _ = db.dedupe_skills_by_name();
        Ok(Self { paths, db })
    }

    /// Walk `~/.runai/mcps/*.json` and normalize backups in place:
    ///   - Rewrite OpenCode-shaped entries (command:array) into canonical (command:string + args).
    ///   - Move corrupt entries (empty command) into `mcps/.corrupt/<name>.json`.
    ///   - Leave already-canonical entries untouched (idempotent).
    ///
    /// Returns `(rewritten, quarantined)` for diagnostics. Errors are logged, never propagated.
    pub fn migrate_mcp_backups(paths: &AppPaths) -> (usize, usize) {
        let mcps_dir = paths.mcps_dir();
        if !mcps_dir.exists() {
            return (0, 0);
        }

        let entries = match std::fs::read_dir(&mcps_dir) {
            Ok(d) => d,
            Err(_) => return (0, 0),
        };

        let mut rewritten = 0usize;
        let mut quarantined = 0usize;

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let name = match path.file_stem().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            let raw = match std::fs::read_to_string(&path) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let value: serde_json::Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };

            if is_corrupt(&value) {
                let corrupt_dir = mcps_dir.join(".corrupt");
                if std::fs::create_dir_all(&corrupt_dir).is_err() {
                    continue;
                }
                let dest = corrupt_dir.join(format!("{name}.json"));
                eprintln!(
                    "[runai] quarantining corrupt MCP backup '{name}' -> {}",
                    dest.display()
                );
                if std::fs::rename(&path, &dest).is_ok() {
                    quarantined += 1;
                }
                continue;
            }

            let canonical = to_canonical(&value);
            if canonical == value {
                continue; // already canonical
            }
            match serde_json::to_string_pretty(&canonical)
                .ok()
                .and_then(|out| std::fs::write(&path, out).ok())
            {
                Some(()) => {
                    eprintln!("[runai] normalized MCP backup '{name}' to canonical format");
                    rewritten += 1;
                }
                None => continue,
            }
        }

        (rewritten, quarantined)
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    fn trash_entry_id(resource_id: &str, deleted_at_ms: i64) -> String {
        format!("trash:{deleted_at_ms}:{resource_id}")
    }

    fn trash_payload_path(&self, name: &str, deleted_at_ms: i64) -> PathBuf {
        let slug = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let slug = if slug.is_empty() { "resource" } else { &slug };
        self.paths
            .trash_dir()
            .join(format!("{deleted_at_ms}-{slug}"))
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
            // `link_path.exists()` follows symlinks, so a DANGLING symlink
            // returns false here — making the old `if !exists()` check pass
            // and then fail with EEXIST inside `create_link`. Use the force
            // variant so any pre-existing symlink (including dangling) is
            // unlinked and recreated to point at the right managed dir.
            // Real directories at the link path are still left alone (force
            // only clobbers symlinks) — those would surface as a loud error.
            Linker::create_link_force(&resource.directory, &link_path)?;
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

    fn read_mcp_backup(&self, mcp_name: &str) -> Result<Option<serde_json::Value>> {
        let backup_path = self.paths.mcps_dir().join(format!("{mcp_name}.json"));
        if !backup_path.exists() {
            return Ok(None);
        }
        let backup_content = std::fs::read_to_string(&backup_path)?;
        Ok(Some(serde_json::from_str(&backup_content)?))
    }

    fn write_mcp_backup(&self, mcp_name: &str, entry: &serde_json::Value) -> Result<()> {
        let backup_dir = self.paths.mcps_dir();
        std::fs::create_dir_all(&backup_dir)?;
        let backup_path = backup_dir.join(format!("{mcp_name}.json"));
        std::fs::write(&backup_path, serde_json::to_string_pretty(entry)?)?;
        Ok(())
    }

    fn remove_mcp_backup(&self, mcp_name: &str) -> Result<()> {
        let backup_path = self.paths.mcps_dir().join(format!("{mcp_name}.json"));
        if backup_path.exists() {
            std::fs::remove_file(backup_path)?;
        }
        Ok(())
    }

    /// Read the named MCP entry out of `target`'s config file, normalize it
    /// into canonical (Claude/Gemini-style) JSON, and remove it from the file.
    /// Returns `None` if the entry is absent or the config file doesn't exist.
    ///
    /// The returned canonical Value is what callers should persist as backup.
    fn remove_mcp_entry_from_target(
        &self,
        mcp_name: &str,
        target: CliTarget,
    ) -> Result<Option<serde_json::Value>> {
        let config_path = Self::cli_config_path(target);
        if !config_path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&config_path)?;

        if target.uses_toml() {
            let mut table: toml::Table = content.parse()?;
            let removed = if let Some(toml::Value::Table(servers)) = table.get_mut("mcp_servers") {
                servers
                    .remove(mcp_name)
                    .map(|entry| codex_toml_to_canonical(&entry))
            } else {
                None
            };
            if removed.is_some() {
                std::fs::write(&config_path, toml::to_string_pretty(&table)?)?;
            }
            Ok(removed)
        } else {
            let mut config: serde_json::Value = serde_json::from_str(&content)?;
            let mcp_key = if target.uses_opencode_format() {
                "mcp"
            } else {
                "mcpServers"
            };
            let removed =
                if let Some(servers) = config.get_mut(mcp_key).and_then(|s| s.as_object_mut()) {
                    servers.remove(mcp_name).map(|raw| to_canonical(&raw))
                } else {
                    None
                };
            if removed.is_some() {
                std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
            }
            Ok(removed)
        }
    }

    /// Write a canonical entry into `target`'s config file, in `target`'s native shape.
    /// Refuses to write entries flagged corrupt by `mcp_canonical::is_corrupt`.
    fn write_mcp_entry_to_target(
        &self,
        mcp_name: &str,
        target: CliTarget,
        canonical: &serde_json::Value,
    ) -> Result<()> {
        if is_corrupt(canonical) {
            bail!(
                "refusing to write corrupt MCP entry '{mcp_name}' to {} (empty/missing command)",
                target.name()
            );
        }

        let config_path = Self::cli_config_path(target);

        // Strip transient `disabled` before emitting — enabling means "disabled is gone".
        let mut canonical = canonical.clone();
        if let Some(obj) = canonical.as_object_mut() {
            obj.remove("disabled");
        }

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if target.uses_toml() {
            let mut table: toml::Table = if config_path.exists() {
                std::fs::read_to_string(&config_path)?.parse()?
            } else {
                toml::Table::new()
            };
            let servers = table
                .entry("mcp_servers")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(s) = servers {
                s.insert(mcp_name.to_string(), canonical_to_codex_toml(&canonical));
            }
            std::fs::write(&config_path, toml::to_string_pretty(&table)?)?;
        } else {
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

            let target_entry = from_canonical_for_json_target(&canonical, target);

            let servers = config
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("config is not an object"))?
                .entry(mcp_key)
                .or_insert_with(|| serde_json::json!({}));

            servers
                .as_object_mut()
                .ok_or_else(|| anyhow::anyhow!("{mcp_key} is not an object"))?
                .insert(mcp_name.to_string(), target_entry);

            std::fs::write(&config_path, serde_json::to_string_pretty(&config)?)?;
        }

        Ok(())
    }

    /// Disable MCP: save config to backup, remove entry from CLI config file.
    /// Corrupt entries (empty command, no url) are removed from the CLI but NOT
    /// persisted as backup — re-enabling a corrupt entry would just fail at the
    /// `is_corrupt` write guard. The user is told to re-register manually.
    fn remove_mcp(&self, mcp_name: &str, target: CliTarget) -> Result<()> {
        if let Some(entry) = self.remove_mcp_entry_from_target(mcp_name, target)? {
            if is_corrupt(&entry) {
                eprintln!(
                    "[runai] removed corrupt MCP entry '{mcp_name}' from {} — no backup created (re-register the MCP via your CLI to recover)",
                    target.name()
                );
            } else {
                self.write_mcp_backup(mcp_name, &entry)?;
            }
        }
        Ok(())
    }

    /// Enable MCP: restore saved config back into CLI config file.
    ///
    /// If no backup exists (MCP was never disabled from this CLI), falls back to
    /// discovering the MCP definition from any other registered CLI config and
    /// cross-registering it into the target CLI. This allows enabling a
    /// Claude-only MCP for Codex without requiring a prior disable/backup cycle.
    fn restore_mcp(&self, mcp_name: &str, target: CliTarget) -> Result<()> {
        // Read backup — fall back to discovery if no backup exists
        let entry: serde_json::Value = if let Some(entry) = self.read_mcp_backup(mcp_name)? {
            entry
        } else {
            // No backup: try to discover from any CLI config that has this MCP
            let home = dirs::home_dir().unwrap_or_default();
            let discovered = crate::core::mcp_discovery::McpDiscovery::discover_all(&home);
            let found = discovered.into_iter().find(|e| e.name == mcp_name);
            match found {
                Some(e) => serde_json::json!({
                    "command": e.command,
                    "args": e.args,
                }),
                None => bail!(
                    "MCP '{mcp_name}' not found in any CLI config. \
                     Register it first with your CLI (e.g. 'claude mcp add')."
                ),
            }
        };
        self.write_mcp_entry_to_target(mcp_name, target, &entry)
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

    fn remove_skill_links(&self, name: &str) -> Result<()> {
        for target in CliTarget::ALL {
            for link in [
                target.skills_dir().join(name),
                target.agents_skills_dir().join(name),
            ] {
                if Linker::is_our_symlink(&link, self.paths.data_dir()) {
                    Linker::remove_link(&link)?;
                }
            }
        }
        Ok(())
    }

    pub fn list_trash(&self) -> Result<Vec<TrashEntry>> {
        self.db.list_trash_entries()
    }

    pub fn find_trash_id(&self, query: &str) -> Option<String> {
        let entries = self.list_trash().ok()?;
        if let Some(entry) = entries.iter().find(|entry| entry.id == query) {
            return Some(entry.id.clone());
        }
        entries
            .into_iter()
            .find(|entry| entry.name == query)
            .map(|entry| entry.id)
    }

    pub fn trash_resource(&self, resource_id: &str) -> Result<TrashEntry> {
        let now = chrono::Utc::now();
        let deleted_at = now.timestamp();
        let deleted_at_ms = now.timestamp_millis();

        if let Some(mcp_name) = resource_id.strip_prefix("mcp:") {
            let mut enabled_targets = Vec::new();
            let mut mcp_configs = HashMap::new();
            for target in CliTarget::ALL {
                if let Some(entry) = self.remove_mcp_entry_from_target(mcp_name, *target)? {
                    enabled_targets.push(*target);
                    mcp_configs.insert(*target, entry);
                }
            }

            let disabled_backup = self.read_mcp_backup(mcp_name)?;
            self.remove_mcp_backup(mcp_name)?;
            let group_ids = self.db.take_groups_for_resource(resource_id)?;

            if mcp_configs.is_empty() && disabled_backup.is_none() {
                bail!("resource not found: {resource_id}");
            }

            let entry = TrashEntry {
                id: Self::trash_entry_id(resource_id, deleted_at_ms),
                resource_id: resource_id.to_string(),
                name: mcp_name.to_string(),
                kind: ResourceKind::Mcp,
                description: String::new(),
                directory: PathBuf::new(),
                source: Source::Local {
                    path: PathBuf::new(),
                },
                installed_at: 0,
                usage_count: 0,
                last_used_at: None,
                deleted_at,
                payload_path: None,
                enabled_targets,
                group_ids,
                mcp_configs,
                disabled_backup,
            };
            self.db.insert_trash_entry(&entry)?;
            return Ok(entry);
        }

        let resource = self
            .db
            .get_resource(resource_id)?
            .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;

        let enabled_map = self.check_skill_symlinks(&resource.name);
        let enabled_targets = CliTarget::ALL
            .iter()
            .copied()
            .filter(|target| enabled_map.get(target).copied().unwrap_or(false))
            .collect::<Vec<_>>();
        let payload_path = self.trash_payload_path(&resource.name, deleted_at_ms);

        self.remove_skill_links(&resource.name)?;
        if resource.directory.exists() {
            if let Some(parent) = payload_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Linker::move_dir(&resource.directory, &payload_path)?;
        } else {
            bail!(
                "resource directory missing: {}",
                resource.directory.display()
            );
        }

        let group_ids = self.db.take_groups_for_resource(resource_id)?;
        // Drop AI summary + user rating rows so they don't linger in the
        // dashboard's enrichment-progress count for a skill that's been
        // trashed. If the user restores the skill, they can re-run
        // `runai recommend enrich` to regenerate.
        self.db.delete_skill_scoring(&resource.name)?;
        self.db.delete_resource(resource_id)?;

        let entry = TrashEntry {
            id: Self::trash_entry_id(resource_id, deleted_at_ms),
            resource_id: resource.id.clone(),
            name: resource.name.clone(),
            kind: resource.kind,
            description: resource.description.clone(),
            directory: resource.directory.clone(),
            source: resource.source.clone(),
            installed_at: resource.installed_at,
            usage_count: resource.usage_count,
            last_used_at: resource.last_used_at,
            deleted_at,
            payload_path: Some(payload_path),
            enabled_targets,
            group_ids,
            mcp_configs: HashMap::new(),
            disabled_backup: None,
        };
        self.db.insert_trash_entry(&entry)?;
        Ok(entry)
    }

    pub fn uninstall(&self, resource_id: &str) -> Result<()> {
        let _ = self.trash_resource(resource_id)?;
        Ok(())
    }

    pub fn restore_from_trash(&self, trash_id: &str) -> Result<()> {
        let entry = self
            .db
            .get_trash_entry(trash_id)?
            .ok_or_else(|| anyhow::anyhow!("trash entry not found: {trash_id}"))?;

        match entry.kind {
            ResourceKind::Skill => {
                let payload_path = entry
                    .payload_path
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("trash payload missing for {}", entry.name))?;
                if !payload_path.exists() {
                    bail!("trash payload missing: {}", payload_path.display());
                }
                if entry.directory.exists() || self.db.get_resource(&entry.resource_id)?.is_some() {
                    bail!("resource already exists: {}", entry.name);
                }
                if let Some(parent) = entry.directory.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Linker::move_dir(&payload_path, &entry.directory)?;

                let resource = Resource {
                    id: entry.resource_id.clone(),
                    name: entry.name.clone(),
                    kind: entry.kind,
                    description: entry.description.clone(),
                    directory: entry.directory.clone(),
                    source: entry.source.clone(),
                    installed_at: entry.installed_at,
                    enabled: HashMap::new(),
                    usage_count: entry.usage_count,
                    last_used_at: entry.last_used_at,
                };
                self.db.insert_resource(&resource)?;
                for group_id in &entry.group_ids {
                    self.db.add_group_member(group_id, &entry.resource_id)?;
                }
                for target in &entry.enabled_targets {
                    self.enable_resource(&entry.resource_id, *target, None)?;
                }
            }
            ResourceKind::Mcp => {
                let mcp_status = Self::read_mcp_status_from_configs();
                for target in entry.mcp_configs.keys() {
                    if mcp_status
                        .get(&entry.name)
                        .and_then(|targets| targets.get(target))
                        .copied()
                        .unwrap_or(false)
                    {
                        bail!("MCP already exists for {} on {}", entry.name, target.name());
                    }
                }

                if entry.disabled_backup.is_some()
                    && self
                        .paths
                        .mcps_dir()
                        .join(format!("{}.json", entry.name))
                        .exists()
                {
                    bail!("disabled MCP backup already exists: {}", entry.name);
                }

                for (target, mcp_entry) in &entry.mcp_configs {
                    self.write_mcp_entry_to_target(&entry.name, *target, mcp_entry)?;
                }
                if let Some(ref disabled_backup) = entry.disabled_backup {
                    self.write_mcp_backup(&entry.name, disabled_backup)?;
                }
                for group_id in &entry.group_ids {
                    self.db.add_group_member(group_id, &entry.resource_id)?;
                }
            }
        }

        self.db.delete_trash_entry(trash_id)?;
        Ok(())
    }

    pub fn purge_trash(&self, trash_id: &str) -> Result<()> {
        let entry = self
            .db
            .get_trash_entry(trash_id)?
            .ok_or_else(|| anyhow::anyhow!("trash entry not found: {trash_id}"))?;

        if let Some(payload_path) = entry.payload_path
            && payload_path.exists()
        {
            if payload_path.is_dir() {
                std::fs::remove_dir_all(&payload_path)?;
            } else {
                std::fs::remove_file(&payload_path)?;
            }
        }

        self.db.delete_trash_entry(trash_id)?;
        Ok(())
    }

    pub fn empty_trash(&self) -> Result<usize> {
        let entries = self.list_trash()?;
        for entry in &entries {
            self.purge_trash(&entry.id)?;
        }
        Ok(entries.len())
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
            // Dedupe by name first — same skill may have multiple DB rows from
            // historical adopts. Without this, a duplicated skill counts twice
            // toward `enabled` while the list-view (which dedupes via
            // list_resources) shows it once, and the header/list disagree.
            let mut seen = std::collections::HashSet::new();
            for skill in &skills {
                if !seen.insert(skill.name.clone()) {
                    continue;
                }
                // Check both skills/ and .agents/skills/. Use `symlink_metadata`
                // (via `Linker::is_symlink`) so DANGLING symlinks still count
                // as enabled — `path.exists()` follows symlinks and returns
                // false for those, which is what made the header undercount.
                // The on-disk symlink IS the source of truth for "enabled";
                // whether it's currently dangling is a separate concern that
                // `runai doctor` reports.
                let primary = target.skills_dir().join(&skill.name);
                let agents = target.agents_skills_dir().join(&skill.name);
                if Linker::is_symlink(&primary)
                    || Linker::is_symlink(&agents)
                    || primary.exists()
                    || agents.exists()
                {
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
                Some(id) => match self.trash_resource(&id) {
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

    /// Get usage stats from Claude Code transcripts, sorted by count DESC.
    ///
    /// Sources truth from `~/.claude/projects/**/*.jsonl` — the `record_usage`
    /// DB path is kept for compatibility but no longer feeds this call.
    pub fn usage_stats(&self) -> Result<Vec<crate::core::resource::UsageStat>> {
        use crate::core::resource::UsageStat;
        use crate::core::transcript_stats::{self, StatKind};

        let stats = transcript_stats::scan_default()?;
        let out = stats
            .entries
            .into_iter()
            .map(|e| UsageStat {
                id: match e.kind {
                    StatKind::Skill => format!("skill:{}", e.name),
                    StatKind::Mcp => format!("mcp:{}", e.name),
                },
                name: e.name,
                count: e.count,
                last_used_at: e.last_used_at,
            })
            .collect();
        Ok(out)
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
        if mcps_dir.exists()
            && let Ok(entries) = std::fs::read_dir(&mcps_dir)
        {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("json")
                    && let Some(name) = entry.path().file_stem().and_then(|s| s.to_str())
                {
                    total_mcp_names.insert(name.to_string());
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
        if self.paths.mcps_dir().join(format!("{name}.json")).exists() {
            return Some(format!("mcp:{name}"));
        }
        None
    }
}

// Tests rely on `with_home` to redirect `dirs::home_dir()` via the HOME env
// var. That works on unix, but on Windows the `dirs` 6.x crate resolves home
// through the Win32 `SHGetKnownFolderPath` API and ignores env vars — there's
// no way to mock home in-process. Skip the whole module on Windows rather than
// introduce a production-only escape hatch just for tests. Generic coverage
// still runs on unix; runtime Windows usage hits the real user home, which is
// the intended behavior anyway.
#[cfg(all(test, not(target_os = "windows")))]
mod tests {
    use super::*;
    use crate::test_support::HOME_LOCK;

    /// Helper: temporarily set HOME, run a closure, restore.
    fn with_home<F: FnOnce()>(tmp: &Path, f: F) {
        let _guard = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let original = std::env::var("HOME").ok();
        // SAFETY: HOME_LOCK prevents other test threads from racing on HOME.
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
    fn trash_and_restore_skill_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        let skill_dir = sm_data.join("skills").join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# Test\n").unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            mgr.register_local_skill("test-skill").unwrap();
            let resource_id = mgr.find_resource_id("test-skill").unwrap();
            mgr.db().add_group_member("grp", &resource_id).unwrap();
            mgr.enable_resource(&resource_id, CliTarget::Claude, None)
                .unwrap();

            let trash = mgr.trash_resource(&resource_id).unwrap();
            assert!(mgr.find_resource_id("test-skill").is_none());
            assert!(trash.payload_path.as_ref().unwrap().exists());
            assert!(!skill_dir.exists(), "skill dir should move into trash");
            assert!(
                !CliTarget::Claude.skills_dir().join("test-skill").exists(),
                "enabled symlink should be removed"
            );
            assert!(
                mgr.db()
                    .get_groups_for_resource(&resource_id)
                    .unwrap()
                    .is_empty()
            );

            mgr.restore_from_trash(&trash.id).unwrap();

            assert!(skill_dir.exists(), "skill dir should be restored");
            assert!(mgr.find_resource_id("test-skill").is_some());
            assert!(
                CliTarget::Claude.skills_dir().join("test-skill").exists(),
                "enabled symlink should be restored"
            );
            assert_eq!(
                mgr.db().get_groups_for_resource(&resource_id).unwrap(),
                vec!["grp".to_string()]
            );
            assert!(mgr.list_trash().unwrap().is_empty());
        });
    }

    #[test]
    fn trash_and_restore_mcp_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            let claude_config = tmp.path().join(".claude.json");
            std::fs::write(
                &claude_config,
                serde_json::json!({
                    "mcpServers": {
                        "test-mcp": {
                            "command": "node",
                            "args": ["server.js"]
                        }
                    }
                })
                .to_string(),
            )
            .unwrap();
            mgr.db().add_group_member("grp", "mcp:test-mcp").unwrap();

            let resource_id = mgr.find_resource_id("test-mcp").unwrap();
            let trash = mgr.trash_resource(&resource_id).unwrap();

            let config_after_delete = std::fs::read_to_string(&claude_config).unwrap();
            assert!(!config_after_delete.contains("test-mcp"));
            assert_eq!(
                mgr.db().get_groups_for_resource("mcp:test-mcp").unwrap(),
                Vec::<String>::new()
            );

            mgr.restore_from_trash(&trash.id).unwrap();

            let config_after_restore = std::fs::read_to_string(&claude_config).unwrap();
            assert!(config_after_restore.contains("test-mcp"));
            assert_eq!(
                mgr.db().get_groups_for_resource("mcp:test-mcp").unwrap(),
                vec!["grp".to_string()]
            );
            assert!(mgr.list_trash().unwrap().is_empty());
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
    fn usage_stats_aggregates_claude_transcripts() {
        // Serialized at process level — the env var is global.
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("some-proj");
        std::fs::create_dir_all(&proj).unwrap();
        let skill = r#"{"type":"assistant","timestamp":"2026-04-17T01:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Skill","input":{"skill":"polish"}}]}}"#;
        let mcp = r#"{"type":"assistant","timestamp":"2026-04-17T02:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"mcp__runai__sm_list","input":{}}]}}"#;
        std::fs::write(proj.join("s.jsonl"), format!("{skill}\n{mcp}\n{skill}\n")).unwrap();

        // SAFETY: serialized via ENV_LOCK; no concurrent reader of this var.
        unsafe { std::env::set_var("RUNAI_TRANSCRIPTS_DIR", tmp.path()) };

        let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
        let stats = mgr.usage_stats().unwrap();

        unsafe { std::env::remove_var("RUNAI_TRANSCRIPTS_DIR") };

        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].name, "polish");
        assert_eq!(stats[0].count, 2);
        assert!(stats[0].id.starts_with("skill:"));
        assert_eq!(stats[1].name, "runai");
        assert_eq!(stats[1].count, 1);
        assert!(stats[1].id.starts_with("mcp:"));
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
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(
            "C:\\nonexistent\\path\\test-skill",
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
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(
            "C:\\some\\other\\path\\test-skill",
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
                claude_skills.join("test-skill").symlink_metadata().is_err(),
                "symlink should be removed"
            );
        });
    }

    // ── Cross-CLI MCP registration tests ──

    /// When an MCP exists only in Claude's config and the user tries to enable it
    /// for Codex, runai should discover the definition from Claude and register it
    /// in Codex's config.toml — instead of failing with "No saved config".
    #[test]
    fn enable_mcp_for_codex_when_only_in_claude_cross_registers() {
        let tmp = tempfile::tempdir().unwrap();

        // design-gateway is only in Claude's config
        let claude_config = serde_json::json!({
            "mcpServers": {
                "design-gateway": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/design-gateway"],
                    "description": "Design MCP"
                }
            }
        });
        std::fs::write(
            tmp.path().join(".claude.json"),
            serde_json::to_string_pretty(&claude_config).unwrap(),
        )
        .unwrap();

        // Codex config exists but doesn't have design-gateway
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(codex_dir.join("config.toml"), "model = \"o4\"\n").unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            // Should succeed: discover from Claude and cross-register to Codex
            let result = mgr.enable_resource("mcp:design-gateway", CliTarget::Codex, None);
            assert!(
                result.is_ok(),
                "enabling for new CLI should succeed, got: {result:?}"
            );

            // design-gateway should now appear in Codex's config.toml
            let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
            assert!(
                content.contains("design-gateway"),
                "design-gateway should be added to Codex config"
            );
            assert!(
                content.contains("npx"),
                "command should be preserved in Codex config"
            );

            // Non-MCP config should be preserved
            assert!(content.contains("model"), "existing Codex config preserved");
        });
    }

    /// When an MCP exists only in Claude's config and the user disables it for Codex,
    /// the operation should be a no-op (not an error) since there's nothing to remove.
    #[test]
    fn disable_mcp_for_codex_when_only_in_claude_is_noop() {
        let tmp = tempfile::tempdir().unwrap();

        let claude_config = serde_json::json!({
            "mcpServers": {
                "design-gateway": { "command": "npx", "args": ["-y", "@mcp/design"] }
            }
        });
        std::fs::write(
            tmp.path().join(".claude.json"),
            serde_json::to_string_pretty(&claude_config).unwrap(),
        )
        .unwrap();

        // Codex has its own MCPs but not design-gateway
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            "[mcp_servers.other]\ntype=\"stdio\"\ncommand=\"other\"\n",
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();

            // Should not error — just a no-op
            let result = mgr.disable_resource("mcp:design-gateway", CliTarget::Codex, None);
            assert!(
                result.is_ok(),
                "disabling non-existent MCP for target CLI should be no-op"
            );

            // Codex config should be unchanged (other MCP still there)
            let content = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
            assert!(content.contains("other"), "existing Codex MCPs preserved");
            // No design-gateway was added (it wasn't there to begin with)
            assert!(
                !content.contains("design-gateway"),
                "design-gateway should not appear in Codex config"
            );
        });
    }

    // ── 2026-04-27 incident regression tests ──────────────────────────────
    // Bugs fixed in this commit:
    //   3. status() undercounted skills whose ~/.claude/skills/ symlink was
    //      dangling — `path.exists()` follows symlinks.
    //   1+2. enable_resource silently failed when a stale (dangling) symlink
    //      sat at the link path — `fs::symlink` returned EEXIST but the
    //      caller wrapped it in `if !exists()` so the error never surfaced.
    //   4. resource_count vs list_resources diverged when DB carried two
    //      rows with the same skill name from successive adopts.
    //   5. scanner.adopt_entry would `std::fs::rename` real ~/.runai/skills/
    //      data into a non-default RUNE_DATA_DIR target (the actual cause of
    //      the 5 permanently-deleted skills).

    #[test]
    fn status_counts_dangling_symlink_as_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            let skill_dir = sm_data.join("skills/my-skill");
            std::fs::create_dir_all(&skill_dir).unwrap();
            mgr.db()
                .insert_resource(&Resource {
                    id: "local:my-skill".into(),
                    name: "my-skill".into(),
                    kind: ResourceKind::Skill,
                    description: "x".into(),
                    directory: skill_dir.clone(),
                    source: Source::Local {
                        path: skill_dir.clone(),
                    },
                    installed_at: 0,
                    enabled: HashMap::new(),
                    usage_count: 0,
                    last_used_at: None,
                })
                .unwrap();

            // Create the ~/.claude/skills/my-skill symlink pointing at a
            // path that does NOT exist (dangling). path.exists() returns
            // false here; the OLD status() code skipped this skill.
            let claude_skills = tmp.path().join(".claude/skills");
            std::fs::create_dir_all(&claude_skills).unwrap();
            let link = claude_skills.join("my-skill");
            std::os::unix::fs::symlink(tmp.path().join("nope"), &link).unwrap();
            assert!(!link.exists(), "link must be dangling");

            let (skill_enabled, _) = mgr.status(CliTarget::Claude).unwrap();
            assert_eq!(
                skill_enabled, 1,
                "dangling symlink IS the source of truth for enabled — status() must count it"
            );
        });
    }

    #[test]
    fn enable_resource_clobbers_dangling_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();
            let skill_dir = sm_data.join("skills/my-skill");
            std::fs::create_dir_all(&skill_dir).unwrap();
            mgr.db()
                .insert_resource(&Resource {
                    id: "local:my-skill".into(),
                    name: "my-skill".into(),
                    kind: ResourceKind::Skill,
                    description: "x".into(),
                    directory: skill_dir.clone(),
                    source: Source::Local {
                        path: skill_dir.clone(),
                    },
                    installed_at: 0,
                    enabled: HashMap::new(),
                    usage_count: 0,
                    last_used_at: None,
                })
                .unwrap();

            // Pre-existing dangling symlink at the link path simulates a
            // prior failed scan / smoke-test pollution.
            let claude_skills = tmp.path().join(".claude/skills");
            std::fs::create_dir_all(&claude_skills).unwrap();
            let link = claude_skills.join("my-skill");
            std::os::unix::fs::symlink(tmp.path().join("nope"), &link).unwrap();

            mgr.enable_resource("local:my-skill", CliTarget::Claude, None)
                .expect("enable must succeed even if a dangling symlink already occupies the path");

            // Symlink must now point at the real managed skill dir.
            let resolved = std::fs::read_link(&link).unwrap();
            assert_eq!(resolved, skill_dir);
            assert!(link.exists(), "link must resolve after enable");
        });
    }

    #[test]
    fn dedupe_skills_by_name_keeps_newest_and_redirects_groups() {
        let tmp = tempfile::tempdir().unwrap();
        let sm_data = tmp.path().join("sm-data");
        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(sm_data.clone()).unwrap();

            let dir = sm_data.join("skills/dup");
            std::fs::create_dir_all(&dir).unwrap();
            // Two rows for the same skill name, different ids and installed_at.
            for (id, ts) in [("local:dup", 100), ("adopted:dup", 200)] {
                mgr.db()
                    .insert_resource(&Resource {
                        id: id.into(),
                        name: "dup".into(),
                        kind: ResourceKind::Skill,
                        description: id.into(),
                        directory: dir.clone(),
                        source: Source::Local { path: dir.clone() },
                        installed_at: ts,
                        enabled: HashMap::new(),
                        usage_count: 0,
                        last_used_at: None,
                    })
                    .unwrap();
            }

            // Put the loser (older row) in a group so we can verify
            // membership migrates to the keeper.
            let g = crate::core::group::Group {
                name: "g".into(),
                description: "".into(),
                kind: crate::core::group::GroupKind::Custom,
                auto_enable: false,
                members: vec![],
            };
            mgr.create_group("g", &g).unwrap();
            mgr.db().add_group_member("g", "local:dup").unwrap();

            let removed = mgr.db().dedupe_skills_by_name().unwrap();
            assert_eq!(removed, 1, "exactly the older row must be deleted");

            // Newest row (adopted:dup, ts=200) survived.
            assert!(mgr.db().get_resource("adopted:dup").unwrap().is_some());
            assert!(mgr.db().get_resource("local:dup").unwrap().is_none());

            // Group membership migrated to keeper.
            let members = mgr.db().get_group_members("g").unwrap();
            assert_eq!(members.len(), 1);
            assert_eq!(members[0].id, "adopted:dup");
        });
    }

    #[test]
    fn scan_refuses_when_actual_source_in_default_data_dir_but_active_dir_differs() {
        // This is the 2026-04-27 incident root-cause guard. We can't easily
        // simulate the full default_data_dir path on a CI temp dir without
        // also remapping HOME — but we CAN verify the predicate by setting
        // up the default skill, the foreign symlink, and a custom data dir,
        // all rooted under a shared HOME.
        let tmp = tempfile::tempdir().unwrap();
        with_home(tmp.path(), || {
            let real_skill = tmp.path().join(".runai/skills/protected");
            std::fs::create_dir_all(&real_skill).unwrap();
            std::fs::write(real_skill.join("SKILL.md"), "---\nname: protected\n---\n").unwrap();

            // Foreign symlink: ~/.claude/skills/protected -> ~/.runai/skills/protected
            let claude_skills = tmp.path().join(".claude/skills");
            std::fs::create_dir_all(&claude_skills).unwrap();
            let foreign_link = claude_skills.join("protected");
            std::os::unix::fs::symlink(&real_skill, &foreign_link).unwrap();

            // Custom data dir != default (~/.runai). This is the dangerous combo.
            let custom = tmp.path().join("custom-data-dir");
            let mgr = SkillManager::with_base(custom.clone()).unwrap();

            let result = crate::core::scanner::Scanner::scan_cli_dir(
                &claude_skills,
                mgr.paths(),
                mgr.db(),
                CliTarget::Claude,
            )
            .expect("scan_cli_dir should not itself error — guard surfaces inside ScanResult");

            // The scan should NOT have moved real_skill out of ~/.runai/skills.
            assert!(
                real_skill.exists(),
                "the guard must prevent rename — real ~/.runai data must stay put"
            );
            // And the result should report the guard error (not a successful adopt).
            assert!(
                !result.errors.is_empty(),
                "scan_cli_dir must surface the guard's bail as an error: {result:?}"
            );
            assert!(
                result
                    .errors
                    .iter()
                    .any(|e: &String| e.contains("default data dir")),
                "error message must reference the default-data-dir guard: {:?}",
                result.errors
            );
        });
    }

    // --- migrate_mcp_backups: regression for the cross-CLI schema bug ---

    #[test]
    fn migrate_mcp_backups_normalizes_opencode_shaped_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        std::fs::create_dir_all(paths.mcps_dir()).unwrap();
        let backup = paths.mcps_dir().join("foo.json");
        std::fs::write(
            &backup,
            r#"{"command":["/bin/foo","arg1"],"enabled":true,"type":"local"}"#,
        )
        .unwrap();

        let (rewritten, quarantined) = SkillManager::migrate_mcp_backups(&paths);
        assert_eq!(rewritten, 1);
        assert_eq!(quarantined, 0);

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
        assert_eq!(after["command"], serde_json::json!("/bin/foo"));
        assert_eq!(after["args"], serde_json::json!(["arg1"]));
        assert!(after.get("enabled").is_none(), "OpenCode enabled stripped");
        assert!(after.get("type").is_none(), "OpenCode type stripped");
    }

    #[test]
    fn migrate_mcp_backups_quarantines_corrupt_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        std::fs::create_dir_all(paths.mcps_dir()).unwrap();
        let backup = paths.mcps_dir().join("broken.json");
        std::fs::write(&backup, r#"{"command":[""],"enabled":true,"type":"local"}"#).unwrap();

        let (rewritten, quarantined) = SkillManager::migrate_mcp_backups(&paths);
        assert_eq!(rewritten, 0);
        assert_eq!(quarantined, 1);

        assert!(!backup.exists(), "corrupt backup moved out of mcps/");
        let corrupt = paths.mcps_dir().join(".corrupt").join("broken.json");
        assert!(corrupt.exists(), "corrupt backup landed in mcps/.corrupt/");
    }

    #[test]
    fn migrate_mcp_backups_is_idempotent_on_canonical_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().to_path_buf());
        std::fs::create_dir_all(paths.mcps_dir()).unwrap();
        let backup = paths.mcps_dir().join("clean.json");
        let original = r#"{
  "command": "/bin/foo",
  "args": ["x"]
}"#;
        std::fs::write(&backup, original).unwrap();

        let (rewritten, quarantined) = SkillManager::migrate_mcp_backups(&paths);
        assert_eq!(rewritten, 0);
        assert_eq!(quarantined, 0);
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
    }

    #[test]
    fn write_mcp_entry_refuses_corrupt_canonical() {
        let tmp = tempfile::tempdir().unwrap();
        write_realistic_claude_json(tmp.path());

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            // Pretend a backup already canonical but with empty command
            let canonical = serde_json::json!({ "command": "", "args": [] });
            let res = mgr.write_mcp_entry_to_target("bad", CliTarget::Claude, &canonical);
            assert!(
                res.is_err(),
                "corrupt canonical entries must not be written"
            );
        });
    }

    #[test]
    fn cross_cli_disable_opencode_then_enable_claude_writes_canonical_to_claude() {
        let tmp = tempfile::tempdir().unwrap();

        // Pre-existing OpenCode config with `crosery-search` registered natively
        let oc_dir = tmp.path().join(".config").join("opencode");
        std::fs::create_dir_all(&oc_dir).unwrap();
        std::fs::write(
            oc_dir.join("opencode.json"),
            r#"{
                "mcp": {
                    "crosery-search": {
                        "command": ["/bin/crosery-search", "--port", "9999"],
                        "enabled": true,
                        "type": "local"
                    }
                }
            }"#,
        )
        .unwrap();

        // Empty Claude config
        std::fs::write(tmp.path().join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            // Disable from OpenCode → backup stored canonical
            mgr.disable_resource("mcp:crosery-search", CliTarget::OpenCode, None)
                .unwrap();

            let backup_path = mgr.paths.mcps_dir().join("crosery-search.json");
            let backup: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&backup_path).unwrap()).unwrap();
            assert_eq!(
                backup["command"],
                serde_json::json!("/bin/crosery-search"),
                "backup stores canonical command (string, not array)"
            );
            assert_eq!(backup["args"], serde_json::json!(["--port", "9999"]));

            // Enable for Claude → must emit Claude-shaped entry
            mgr.enable_resource("mcp:crosery-search", CliTarget::Claude, None)
                .unwrap();

            let claude: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap(),
            )
            .unwrap();
            let entry = &claude["mcpServers"]["crosery-search"];
            assert_eq!(
                entry["command"],
                serde_json::json!("/bin/crosery-search"),
                "Claude entry has command as string"
            );
            assert_eq!(entry["args"], serde_json::json!(["--port", "9999"]));
            assert!(
                entry.get("enabled").is_none(),
                "Claude does not get OpenCode-only `enabled` field"
            );
            assert!(
                entry.get("type").is_none()
                    || entry.get("type").and_then(|v| v.as_str()) != Some("local"),
                "Claude does not get OpenCode `type:local`"
            );
        });
    }

    #[test]
    fn cross_cli_disable_claude_then_enable_opencode_emits_command_array() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".claude.json"),
            r#"{"mcpServers":{"foo":{"command":"/bin/foo","args":["x","y"]}}}"#,
        )
        .unwrap();
        let oc_dir = tmp.path().join(".config").join("opencode");
        std::fs::create_dir_all(&oc_dir).unwrap();
        std::fs::write(oc_dir.join("opencode.json"), r#"{"mcp":{}}"#).unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            mgr.disable_resource("mcp:foo", CliTarget::Claude, None)
                .unwrap();
            mgr.enable_resource("mcp:foo", CliTarget::OpenCode, None)
                .unwrap();

            let oc: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(oc_dir.join("opencode.json")).unwrap(),
            )
            .unwrap();
            let entry = &oc["mcp"]["foo"];
            assert_eq!(
                entry["command"],
                serde_json::json!(["/bin/foo", "x", "y"]),
                "OpenCode entry has command as array (cmd + args merged)"
            );
            assert_eq!(entry["enabled"], serde_json::json!(true));
            assert_eq!(entry["type"], serde_json::json!("local"));
        });
    }

    #[test]
    fn codex_disable_then_enable_preserves_tools_subtable() {
        let tmp = tempfile::tempdir().unwrap();
        let codex_dir = tmp.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("config.toml"),
            r#"[mcp_servers.design-gateway]
type = "stdio"
command = "/bin/dg"
args = ["server.js"]

[mcp_servers.design-gateway.env]
DG_KEY = "secret"

[mcp_servers.design-gateway.tools.cdp_navigate]
approval_mode = "approve"

[mcp_servers.design-gateway.tools.export_node_as_image]
approval_mode = "approve"
"#,
        )
        .unwrap();

        with_home(tmp.path(), || {
            let mgr = SkillManager::with_base(tmp.path().join("sm-data")).unwrap();
            mgr.disable_resource("mcp:design-gateway", CliTarget::Codex, None)
                .unwrap();
            mgr.enable_resource("mcp:design-gateway", CliTarget::Codex, None)
                .unwrap();

            let after = std::fs::read_to_string(codex_dir.join("config.toml")).unwrap();
            assert!(
                after.contains("approval_mode = \"approve\""),
                "Codex tools.* approval_mode preserved across disable/enable"
            );
            assert!(
                after.contains("DG_KEY = \"secret\""),
                "Codex env subtable preserved"
            );
            assert!(after.contains("cdp_navigate"), "tool 1 preserved");
            assert!(after.contains("export_node_as_image"), "tool 2 preserved");
        });
    }
}

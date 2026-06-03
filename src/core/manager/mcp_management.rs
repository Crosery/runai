use super::SkillManager;
use crate::core::cli_target::CliTarget;
use crate::core::mcp_canonical::{
    canonical_to_codex_toml, codex_toml_to_canonical, from_canonical_for_json_target, is_corrupt,
    to_canonical,
};
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::PathBuf;

impl SkillManager {
    pub(super) fn read_mcp_backup(&self, mcp_name: &str) -> Result<Option<serde_json::Value>> {
        let backup_path = self.paths.mcps_dir().join(format!("{mcp_name}.json"));
        if !backup_path.exists() {
            return Ok(None);
        }
        let backup_content = std::fs::read_to_string(&backup_path)?;
        Ok(Some(serde_json::from_str(&backup_content)?))
    }

    pub(super) fn write_mcp_backup(&self, mcp_name: &str, entry: &serde_json::Value) -> Result<()> {
        let backup_dir = self.paths.mcps_dir();
        std::fs::create_dir_all(&backup_dir)?;
        let backup_path = backup_dir.join(format!("{mcp_name}.json"));
        std::fs::write(&backup_path, serde_json::to_string_pretty(entry)?)?;
        Ok(())
    }

    pub(super) fn remove_mcp_backup(&self, mcp_name: &str) -> Result<()> {
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
    pub(super) fn remove_mcp_entry_from_target(
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
    pub(super) fn write_mcp_entry_to_target(
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
    pub(super) fn remove_mcp(&self, mcp_name: &str, target: CliTarget) -> Result<()> {
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
    pub(super) fn restore_mcp(&self, mcp_name: &str, target: CliTarget) -> Result<()> {
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
}

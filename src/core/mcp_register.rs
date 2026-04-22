use anyhow::{Context, Result};
use std::path::Path;

/// Registers runai as an MCP server in all supported CLI configs.
pub struct McpRegister;

#[derive(Debug)]
pub struct RegisterResult {
    pub registered: Vec<String>, // CLI names successfully registered
    pub skipped: Vec<String>,    // already registered
    pub errors: Vec<String>,     // failed
}

impl McpRegister {
    /// Auto-detect the runai binary path and register to all CLIs.
    pub fn register_all(home: &Path) -> RegisterResult {
        let binary = Self::find_binary();
        let mut result = RegisterResult {
            registered: Vec::new(),
            skipped: Vec::new(),
            errors: Vec::new(),
        };

        // Claude: ~/.claude.json (mcpServers at root)
        match Self::register_claude(home, &binary) {
            Ok(true) => result.registered.push("claude".into()),
            Ok(false) => result.skipped.push("claude".into()),
            Err(e) => result.errors.push(format!("claude: {e}")),
        }

        // Gemini: ~/.gemini/settings.json
        match Self::register_generic(home, ".gemini/settings.json", &binary) {
            Ok(true) => result.registered.push("gemini".into()),
            Ok(false) => result.skipped.push("gemini".into()),
            Err(e) => result.errors.push(format!("gemini: {e}")),
        }

        // Codex: ~/.codex/config.toml (TOML format)
        match Self::register_codex(home, &binary) {
            Ok(true) => result.registered.push("codex".into()),
            Ok(false) => result.skipped.push("codex".into()),
            Err(e) => result.errors.push(format!("codex: {e}")),
        }

        // OpenCode: ~/.config/opencode/opencode.json (custom format: "mcp" key, command=array)
        match Self::register_opencode(home, &binary) {
            Ok(true) => result.registered.push("opencode".into()),
            Ok(false) => result.skipped.push("opencode".into()),
            Err(e) => result.errors.push(format!("opencode: {e}")),
        }

        result
    }

    /// Find the runai binary — prefer PATH, fallback to current exe.
    fn find_binary() -> String {
        // Try current executable path
        if let Ok(exe) = std::env::current_exe() {
            return exe.to_string_lossy().to_string();
        }
        // Fallback
        "runai".to_string()
    }

    /// Register in ~/.claude.json (mcpServers at root level).
    fn register_claude(home: &Path, binary: &str) -> Result<bool> {
        let path = home.join(".claude.json");
        let mut config: serde_json::Value = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str(&content)?
        } else {
            serde_json::json!({})
        };

        let servers = config
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("config is not an object"))?
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));

        let servers_map = servers
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcpServers is not an object"))?;

        if let Some(existing) = servers_map.get("runai") {
            // Already registered — check if binary path is still correct
            let current_cmd = existing
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if current_cmd == binary {
                return Ok(false); // already registered with correct path
            }
            // Path changed — update it
            servers_map.insert("runai".into(), Self::mcp_entry(binary));
            let content = serde_json::to_string_pretty(&config)?;
            std::fs::write(&path, content)?;
            return Ok(true);
        }

        servers_map.insert("runai".into(), Self::mcp_entry(binary));
        let content = serde_json::to_string_pretty(&config)?;
        std::fs::write(&path, content)?;
        Ok(true)
    }

    /// Register in a generic settings.json (create dirs/file if needed).
    fn register_generic(home: &Path, rel_path: &str, binary: &str) -> Result<bool> {
        let path = home.join(rel_path);

        let mut config: serde_json::Value = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str(&content)?
        } else {
            // Create parent directory
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            serde_json::json!({})
        };

        let servers = config
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("config is not an object"))?
            .entry("mcpServers")
            .or_insert_with(|| serde_json::json!({}));

        let servers_map = servers
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcpServers is not an object"))?;

        if let Some(existing) = servers_map.get("runai") {
            let current_cmd = existing
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if current_cmd == binary {
                return Ok(false);
            }
            servers_map.insert("runai".into(), Self::mcp_entry(binary));
            let content = serde_json::to_string_pretty(&config)?;
            std::fs::write(&path, content)?;
            return Ok(true);
        }

        servers_map.insert("runai".into(), Self::mcp_entry(binary));
        let content = serde_json::to_string_pretty(&config)?;
        std::fs::write(&path, content)?;
        Ok(true)
    }

    /// Register in ~/.codex/config.toml (TOML format).
    fn register_codex(home: &Path, binary: &str) -> Result<bool> {
        let path = home.join(".codex").join("config.toml");

        let mut table: toml::Table = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            content.parse()?
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            toml::Table::new()
        };

        // Check if already registered — update if path changed
        if let Some(toml::Value::Table(servers)) = table.get("mcp_servers") {
            if let Some(toml::Value::Table(entry)) = servers.get("runai") {
                let current_cmd = entry
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if current_cmd == binary {
                    return Ok(false);
                }
                // Path changed — fall through to re-register
            } else if servers.contains_key("runai") {
                return Ok(false);
            }
        }

        // Add runai entry
        let servers = table
            .entry("mcp_servers")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let toml::Value::Table(s) = servers {
            let mut entry = toml::Table::new();
            entry.insert("type".into(), toml::Value::String("stdio".into()));
            entry.insert("command".into(), toml::Value::String(binary.into()));
            entry.insert(
                "args".into(),
                toml::Value::Array(vec![toml::Value::String("mcp-serve".into())]),
            );
            s.insert("runai".into(), toml::Value::Table(entry));
        }

        std::fs::write(&path, toml::to_string_pretty(&table)?)?;
        Ok(true)
    }

    /// Register in ~/.config/opencode/opencode.json (OpenCode custom format).
    fn register_opencode(home: &Path, binary: &str) -> Result<bool> {
        let path = home.join(".config").join("opencode").join("opencode.json");

        let mut config: serde_json::Value = if path.exists() {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            serde_json::from_str(&content)?
        } else {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            serde_json::json!({})
        };

        // Check if already registered — update if path changed
        if let Some(existing) = config.get("mcp").and_then(|s| s.get("runai")) {
            let current_cmd = existing
                .get("command")
                .and_then(|v| v.as_array())
                .and_then(|a| a.first())
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if current_cmd == binary {
                return Ok(false);
            }
            // Path changed — fall through to re-register
        }

        let servers = config
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("config is not an object"))?
            .entry("mcp")
            .or_insert_with(|| serde_json::json!({}));

        servers
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcp is not an object"))?
            .insert(
                "runai".into(),
                serde_json::json!({
                    "command": [binary, "mcp-serve"],
                    "enabled": true,
                    "type": "local",
                }),
            );

        let content = serde_json::to_string_pretty(&config)?;
        std::fs::write(&path, content)?;
        Ok(true)
    }

    /// The MCP server entry to inject.
    fn mcp_entry(binary: &str) -> serde_json::Value {
        serde_json::json!({
            "command": binary,
            "args": ["mcp-serve"],
            "description": "Runai — AI skill manager for skills, MCPs, and groups"
        })
    }

    /// Check if already registered in a given config file.
    pub fn is_registered(home: &Path, rel_path: &str) -> bool {
        let path = home.join(rel_path);
        if !path.exists() {
            return false;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let config: serde_json::Value = match serde_json::from_str(&content) {
            Ok(c) => c,
            Err(_) => return false,
        };
        config
            .get("mcpServers")
            .and_then(|s| s.get("runai"))
            .is_some()
    }

    /// Migrate old "skill-manager" MCP entries to "runai" across all CLIs.
    /// Returns the number of CLIs that were migrated.
    /// - Renames the entry key from "skill-manager" to "runai"
    /// - Preserves all fields (args, env, timeout, etc.)
    /// - If "runai" already exists, only removes the old entry (no overwrite)
    /// - Idempotent: safe to call multiple times
    pub fn migrate_all(home: &Path) -> usize {
        let mut count = 0;

        // Claude: ~/.claude.json (JSON, key="mcpServers")
        if Self::migrate_json(&home.join(".claude.json"), "mcpServers") {
            count += 1;
        }

        // Gemini: ~/.gemini/settings.json (JSON, key="mcpServers")
        if Self::migrate_json(&home.join(".gemini").join("settings.json"), "mcpServers") {
            count += 1;
        }

        // Codex: ~/.codex/config.toml (TOML, key="mcp_servers")
        if Self::migrate_codex_toml(&home.join(".codex").join("config.toml")) {
            count += 1;
        }

        // OpenCode: ~/.config/opencode/opencode.json (JSON, key="mcp")
        if Self::migrate_json(
            &home.join(".config").join("opencode").join("opencode.json"),
            "mcp",
        ) {
            count += 1;
        }

        count
    }

    /// Migrate "skill-manager" → "runai" in a JSON config file.
    /// Returns true if migration occurred.
    fn migrate_json(path: &Path, servers_key: &str) -> bool {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let mut config: serde_json::Value = match serde_json::from_str(&content) {
            Ok(c) => c,
            Err(_) => return false,
        };

        let servers = match config.get_mut(servers_key).and_then(|s| s.as_object_mut()) {
            Some(s) => s,
            None => return false,
        };

        // Nothing to migrate
        if !servers.contains_key("skill-manager") {
            return false;
        }

        let old_entry = servers.remove("skill-manager").unwrap();

        // Only insert if "runai" doesn't already exist
        if !servers.contains_key("runai") {
            servers.insert("runai".into(), old_entry);
        }

        // Write back
        if let Ok(out) = serde_json::to_string_pretty(&config) {
            let _ = std::fs::write(path, out);
        }

        true
    }

    /// Migrate "skill-manager" → "runai" in Codex TOML config.
    /// Returns true if migration occurred.
    fn migrate_codex_toml(path: &Path) -> bool {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let mut table: toml::Table = match content.parse() {
            Ok(t) => t,
            Err(_) => return false,
        };

        let servers = match table.get_mut("mcp_servers") {
            Some(toml::Value::Table(s)) => s,
            _ => return false,
        };

        if !servers.contains_key("skill-manager") {
            return false;
        }

        let old_entry = servers.remove("skill-manager").unwrap();

        if !servers.contains_key("runai") {
            servers.insert("runai".into(), old_entry);
        }

        if let Ok(out) = toml::to_string_pretty(&table) {
            let _ = std::fs::write(path, out);
        }

        true
    }

    /// Unregister from all CLIs.
    pub fn unregister_all(home: &Path) -> Result<()> {
        // JSON CLIs with mcpServers key
        for rel_path in &[".claude.json", ".gemini/settings.json"] {
            let path = home.join(rel_path);
            if !path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&path)?;
            let mut config: serde_json::Value = serde_json::from_str(&content)?;
            if let Some(servers) = config.get_mut("mcpServers").and_then(|s| s.as_object_mut()) {
                servers.remove("runai");
            }
            std::fs::write(&path, serde_json::to_string_pretty(&config)?)?;
        }

        // Codex TOML
        let codex_path = home.join(".codex").join("config.toml");
        if codex_path.exists() {
            let content = std::fs::read_to_string(&codex_path)?;
            let mut table: toml::Table = content.parse()?;
            if let Some(toml::Value::Table(servers)) = table.get_mut("mcp_servers") {
                servers.remove("runai");
            }
            std::fs::write(&codex_path, toml::to_string_pretty(&table)?)?;
        }

        // OpenCode: ~/.config/opencode/opencode.json (key="mcp")
        let oc_path = home.join(".config").join("opencode").join("opencode.json");
        if oc_path.exists() {
            let content = std::fs::read_to_string(&oc_path)?;
            let mut config: serde_json::Value = serde_json::from_str(&content)?;
            if let Some(servers) = config.get_mut("mcp").and_then(|s| s.as_object_mut()) {
                servers.remove("runai");
            }
            std::fs::write(&oc_path, serde_json::to_string_pretty(&config)?)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, content).unwrap();
    }

    #[test]
    fn register_claude_creates_entry() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(tmp.path(), ".claude.json", r#"{"mcpServers":{}}"#);

        let result = McpRegister::register_all(tmp.path());
        assert!(result.registered.contains(&"claude".to_string()));

        // Verify written
        let content = std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v["mcpServers"]["runai"]["command"].is_string());
        assert_eq!(v["mcpServers"]["runai"]["args"][0], "mcp-serve");
    }

    #[test]
    fn register_skips_if_path_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let binary = McpRegister::find_binary();
        // Build the JSON via serde so Windows paths (containing `\`) get
        // escaped correctly — plain format! would produce invalid JSON.
        let config = serde_json::json!({
            "mcpServers": {
                "runai": { "command": binary }
            }
        });
        write_file(tmp.path(), ".claude.json", &config.to_string());

        let result = McpRegister::register_all(tmp.path());
        assert!(result.skipped.contains(&"claude".to_string()));
    }

    #[test]
    fn register_updates_if_path_changed() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".claude.json",
            r#"{"mcpServers":{"runai":{"command":"old"}}}"#,
        );

        let result = McpRegister::register_all(tmp.path());
        assert!(result.registered.contains(&"claude".to_string()));

        // Should update to current binary path
        let content = std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap();
        assert!(!content.contains("\"old\""));
    }

    #[test]
    fn register_creates_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        // No .codex dir exists

        let result = McpRegister::register_all(tmp.path());
        assert!(result.registered.contains(&"codex".to_string()));
        // Codex uses config.toml, not settings.json
        assert!(tmp.path().join(".codex/config.toml").exists());
    }

    #[test]
    fn register_preserves_existing_config() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".gemini/settings.json",
            r#"{"general":{"key":"val"},"mcpServers":{"other":{"command":"x"}}}"#,
        );

        let result = McpRegister::register_all(tmp.path());
        assert!(result.registered.contains(&"gemini".to_string()));

        let content = std::fs::read_to_string(tmp.path().join(".gemini/settings.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        // Preserved existing
        assert_eq!(v["general"]["key"], "val");
        assert!(v["mcpServers"]["other"].is_object());
        // Added new
        assert!(v["mcpServers"]["runai"].is_object());
    }

    #[test]
    fn is_registered_works() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".claude.json",
            r#"{"mcpServers":{"runai":{"command":"sm"}}}"#,
        );
        assert!(McpRegister::is_registered(tmp.path(), ".claude.json"));
        assert!(!McpRegister::is_registered(
            tmp.path(),
            ".codex/settings.json"
        ));
    }

    // --- Migration tests ---

    #[test]
    fn migrate_claude_renames_old_entry_preserving_fields() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".claude.json",
            r#"{
                "numStartups": 42,
                "mcpServers": {
                    "skill-manager": {
                        "command": "/old/path/skill-manager",
                        "args": ["mcp-serve"],
                        "description": "old description",
                        "timeout": 30000
                    },
                    "other-mcp": { "command": "x" }
                }
            }"#,
        );

        let migrated = McpRegister::migrate_all(tmp.path());
        assert!(migrated > 0, "should migrate at least one CLI");

        let content = std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();

        // Old entry removed
        assert!(
            v["mcpServers"].get("skill-manager").is_none(),
            "old entry should be removed"
        );
        // New entry exists with preserved fields
        let runai = &v["mcpServers"]["runai"];
        assert!(runai.is_object(), "runai entry should exist");
        assert_eq!(runai["args"][0], "mcp-serve", "args preserved");
        assert_eq!(runai["timeout"], 30000, "timeout preserved");
        // Other MCPs untouched
        assert!(
            v["mcpServers"]["other-mcp"].is_object(),
            "other MCPs preserved"
        );
        // Non-MCP config untouched
        assert_eq!(v["numStartups"], 42, "non-MCP config preserved");
    }

    #[test]
    fn migrate_codex_toml_renames_old_entry() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".codex/config.toml",
            r#"
model = "gpt-5"

[mcp_servers.skill-manager]
type = "stdio"
command = "/old/skill-manager"
args = ["mcp-serve"]

[mcp_servers.other]
type = "stdio"
command = "other-cmd"
"#,
        );

        let migrated = McpRegister::migrate_all(tmp.path());
        assert!(migrated > 0);

        let content = std::fs::read_to_string(tmp.path().join(".codex/config.toml")).unwrap();
        assert!(
            !content.contains("[mcp_servers.skill-manager]"),
            "old entry removed"
        );
        assert!(content.contains("[mcp_servers.runai]"), "new entry created");
        assert!(
            content.contains("[mcp_servers.other]"),
            "other MCPs preserved"
        );
        assert!(content.contains("model"), "non-MCP config preserved");
    }

    #[test]
    fn migrate_opencode_renames_old_entry() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".config/opencode/opencode.json",
            r#"{
                "mcp": {
                    "skill-manager": {
                        "command": ["/old/skill-manager", "mcp-serve"],
                        "enabled": true,
                        "type": "local"
                    },
                    "pencil": {
                        "command": ["npx", "-y", "pencil"],
                        "enabled": true,
                        "type": "local"
                    }
                },
                "provider": {}
            }"#,
        );

        let migrated = McpRegister::migrate_all(tmp.path());
        assert!(migrated > 0);

        let content: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join(".config/opencode/opencode.json")).unwrap(),
        )
        .unwrap();

        assert!(
            content["mcp"].get("skill-manager").is_none(),
            "old entry removed"
        );
        let runai = &content["mcp"]["runai"];
        assert!(runai.is_object(), "runai entry exists");
        assert!(runai["command"].is_array(), "command preserved as array");
        assert_eq!(runai["enabled"], true, "enabled preserved");
        // Other MCPs untouched
        assert!(content["mcp"]["pencil"].is_object(), "other MCPs preserved");
        assert!(content["provider"].is_object(), "non-MCP config preserved");
    }

    #[test]
    fn migrate_skips_when_runai_already_exists() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".claude.json",
            r#"{
                "mcpServers": {
                    "skill-manager": { "command": "/old/skill-manager", "args": ["mcp-serve"] },
                    "runai": { "command": "/new/runai", "args": ["mcp-serve"] }
                }
            }"#,
        );

        let migrated = McpRegister::migrate_all(tmp.path());
        assert_eq!(
            migrated, 1,
            "cleaning up the old entry should still count as a migration"
        );

        let content = std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();

        // runai should keep its existing config, NOT be overwritten by old skill-manager
        assert_eq!(
            v["mcpServers"]["runai"]["command"], "/new/runai",
            "existing runai should NOT be overwritten"
        );
        // old entry should still be cleaned up
        assert!(
            v["mcpServers"].get("skill-manager").is_none(),
            "old entry should be removed even when runai exists"
        );
    }

    #[test]
    fn migrate_noop_when_no_old_entry() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".claude.json",
            r#"{"mcpServers":{"runai":{"command":"x"}}}"#,
        );

        let migrated = McpRegister::migrate_all(tmp.path());
        assert_eq!(migrated, 0, "nothing to migrate");
    }

    #[test]
    fn unregister_removes_entry() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".claude.json",
            r#"{"mcpServers":{"runai":{"command":"sm"},"other":{"command":"x"}}}"#,
        );

        McpRegister::unregister_all(tmp.path()).unwrap();

        let content = std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(v["mcpServers"]["runai"].is_null());
        assert!(v["mcpServers"]["other"].is_object()); // preserved
    }
}

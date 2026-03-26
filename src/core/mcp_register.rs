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

        if servers.get("runai").is_some() {
            return Ok(false); // already registered
        }

        servers
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcpServers is not an object"))?
            .insert("runai".into(), Self::mcp_entry(binary));

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

        if servers.get("runai").is_some() {
            return Ok(false);
        }

        servers
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("mcpServers is not an object"))?
            .insert("runai".into(), Self::mcp_entry(binary));

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

        // Check if already registered
        if let Some(toml::Value::Table(servers)) = table.get("mcp_servers") {
            if servers.contains_key("runai") {
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

        // Check if already registered
        if config.get("mcp").and_then(|s| s.get("runai")).is_some() {
            return Ok(false);
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
    fn register_skips_if_already_present() {
        let tmp = tempfile::tempdir().unwrap();
        write_file(
            tmp.path(),
            ".claude.json",
            r#"{"mcpServers":{"runai":{"command":"old"}}}"#,
        );

        let result = McpRegister::register_all(tmp.path());
        assert!(result.skipped.contains(&"claude".to_string()));

        // Should NOT overwrite
        let content = std::fs::read_to_string(tmp.path().join(".claude.json")).unwrap();
        assert!(content.contains("\"old\""));
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

use anyhow::Result;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A discovered MCP server configuration
#[derive(Debug, Clone)]
pub struct McpEntry {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub description: String,
    pub source_file: PathBuf,
    pub mcp_type: McpType,
    pub disabled: bool,
    pub source_cli: String, // which CLI config this came from
}

#[derive(Debug, Clone, PartialEq)]
pub enum McpType {
    Stdio,
    Http { url: String },
}

#[derive(Deserialize)]
struct McpServerDef {
    command: Option<String>,
    args: Option<Vec<String>>,
    description: Option<String>,
    #[serde(rename = "type")]
    server_type: Option<String>,
    url: Option<String>,
    disabled: Option<bool>,
}

#[derive(Deserialize)]
struct McpConfigFile {
    #[serde(rename = "mcpServers")]
    mcp_servers: Option<HashMap<String, McpServerDef>>,
}

/// Top-level .claude.json which has mcpServers at root
#[derive(Deserialize)]
struct ClaudeJson {
    #[serde(rename = "mcpServers")]
    mcp_servers: Option<HashMap<String, McpServerDef>>,
}

pub struct McpDiscovery;

impl McpDiscovery {
    /// Discover MCPs from all known config locations for a given home directory.
    pub fn discover_all(home: &Path) -> Vec<McpEntry> {
        let mut results = Vec::new();

        // 1. ~/.claude.json (active MCPs)
        let claude_json = home.join(".claude.json");
        if let Ok(entries) = Self::parse_claude_json(&claude_json, "claude") {
            results.extend(entries);
        }

        // 2. ~/.claude/mcp-configs/*.json
        let mcp_configs_dir = home.join(".claude").join("mcp-configs");
        if mcp_configs_dir.is_dir() {
            if let Ok(dir) = std::fs::read_dir(&mcp_configs_dir) {
                for entry in dir.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("json") {
                        if let Ok(entries) = Self::parse_mcp_config_file(&path, "claude") {
                            for e in entries {
                                if !results.iter().any(|r| r.name == e.name) {
                                    results.push(e);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. Codex: config.toml (TOML format)
        let codex_toml = home.join(".codex").join("config.toml");
        if let Ok(entries) = Self::parse_codex_toml(&codex_toml) {
            for e in entries {
                if !results.iter().any(|r| r.name == e.name) {
                    results.push(e);
                }
            }
        }

        // 4. OpenCode: ~/.config/opencode/opencode.json (custom format)
        let oc_path = home.join(".config").join("opencode").join("opencode.json");
        if let Ok(entries) = Self::parse_opencode_json(&oc_path) {
            for e in entries {
                if !results.iter().any(|r| r.name == e.name) {
                    results.push(e);
                }
            }
        }

        // 5. Other JSON CLIs (Gemini)
        for (cli_dir, cli_name) in &[(".gemini", "gemini")] {
            let settings = home.join(cli_dir).join("settings.json");
            if let Ok(entries) = Self::parse_mcp_config_file(&settings, cli_name) {
                for e in entries {
                    if !results.iter().any(|r| r.name == e.name) {
                        results.push(e);
                    }
                }
            }
        }

        results
    }

    /// Parse ~/.claude.json format (mcpServers at root level)
    fn parse_claude_json(path: &Path, cli: &str) -> Result<Vec<McpEntry>> {
        let content = std::fs::read_to_string(path)?;
        let parsed: ClaudeJson = serde_json::from_str(&content)?;
        Ok(Self::convert_map(parsed.mcp_servers, path, cli))
    }

    fn parse_mcp_config_file(path: &Path, cli: &str) -> Result<Vec<McpEntry>> {
        let content = std::fs::read_to_string(path)?;
        let parsed: McpConfigFile = serde_json::from_str(&content)?;
        Ok(Self::convert_map(parsed.mcp_servers, path, cli))
    }

    /// Parse OpenCode's opencode.json: { "mcp": { "name": { "command": [...], "enabled": true } } }
    fn parse_opencode_json(path: &Path) -> Result<Vec<McpEntry>> {
        let content = std::fs::read_to_string(path)?;
        let config: serde_json::Value = serde_json::from_str(&content)?;
        let mut entries = Vec::new();

        if let Some(servers) = config.get("mcp").and_then(|s| s.as_object()) {
            for (name, def) in servers {
                if name.starts_with('_') {
                    continue;
                }
                // command is an array: ["npx", "-y", "pkg"]
                let cmd_arr: Vec<String> = def
                    .get("command")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                if cmd_arr.is_empty() {
                    continue;
                }
                let command = cmd_arr[0].clone();
                let args = cmd_arr[1..].to_vec();
                let disabled = def
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .map(|e| !e)
                    .unwrap_or(false);

                entries.push(McpEntry {
                    name: name.clone(),
                    command,
                    args,
                    description: String::new(),
                    source_file: path.to_path_buf(),
                    mcp_type: McpType::Stdio,
                    disabled,
                    source_cli: "opencode".to_string(),
                });
            }
        }
        Ok(entries)
    }

    /// Parse Codex's config.toml format: [mcp_servers.name]
    fn parse_codex_toml(path: &Path) -> Result<Vec<McpEntry>> {
        let content = std::fs::read_to_string(path)?;
        let table: toml::Table = content.parse()?;
        let mut entries = Vec::new();

        if let Some(toml::Value::Table(servers)) = table.get("mcp_servers") {
            for (name, val) in servers {
                if name.starts_with('_') {
                    continue;
                }
                if let toml::Value::Table(def) = val {
                    let command = def
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if command.is_empty() {
                        continue;
                    }
                    let args: Vec<String> = def
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let mcp_type = if def.get("type").and_then(|v| v.as_str()) == Some("http") {
                        let url = def
                            .get("url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        McpType::Http { url }
                    } else {
                        McpType::Stdio
                    };

                    entries.push(McpEntry {
                        name: name.clone(),
                        command,
                        args,
                        description: String::new(),
                        source_file: path.to_path_buf(),
                        mcp_type,
                        disabled: false,
                        source_cli: "codex".to_string(),
                    });
                }
            }
        }
        Ok(entries)
    }

    fn convert_map(
        servers: Option<HashMap<String, McpServerDef>>,
        source: &Path,
        cli: &str,
    ) -> Vec<McpEntry> {
        let Some(servers) = servers else {
            return Vec::new();
        };
        servers
            .into_iter()
            .filter_map(|(name, def)| {
                // Skip meta keys like _comments
                if name.starts_with('_') {
                    return None;
                }

                let mcp_type = if def.server_type.as_deref() == Some("http") {
                    McpType::Http {
                        url: def.url.unwrap_or_default(),
                    }
                } else {
                    McpType::Stdio
                };

                let command = match &mcp_type {
                    McpType::Http { url } => url.clone(),
                    McpType::Stdio => def.command.unwrap_or_default(),
                };

                if command.is_empty() {
                    return None;
                }

                Some(McpEntry {
                    name,
                    command,
                    args: def.args.unwrap_or_default(),
                    description: def.description.unwrap_or_default(),
                    source_file: source.to_path_buf(),
                    mcp_type,
                    disabled: def.disabled.unwrap_or(false),
                    source_cli: cli.to_string(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_json(dir: &Path, rel_path: &str, content: &str) {
        let path = dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
    }

    #[test]
    fn parse_claude_json_finds_mcps() {
        let tmp = tempfile::tempdir().unwrap();
        write_json(
            tmp.path(),
            ".claude.json",
            r#"{
            "mcpServers": {
                "github": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "description": "GitHub operations"
                },
                "vercel": {
                    "type": "http",
                    "url": "https://mcp.vercel.com",
                    "description": "Vercel"
                }
            }
        }"#,
        );

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 2);

        let github = results.iter().find(|e| e.name == "github").unwrap();
        assert_eq!(github.command, "npx");
        assert_eq!(
            github.args,
            vec!["-y", "@modelcontextprotocol/server-github"]
        );
        assert_eq!(github.mcp_type, McpType::Stdio);

        let vercel = results.iter().find(|e| e.name == "vercel").unwrap();
        assert_eq!(
            vercel.mcp_type,
            McpType::Http {
                url: "https://mcp.vercel.com".into()
            }
        );
    }

    #[test]
    fn parse_mcp_configs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        write_json(
            tmp.path(),
            ".claude/mcp-configs/servers.json",
            r#"{
            "mcpServers": {
                "memory": {
                    "command": "npx",
                    "args": ["-y", "@mcp/server-memory"],
                    "description": "Memory server"
                },
                "_comments": { "note": "skip this" }
            }
        }"#,
        );

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "memory");
    }

    #[test]
    fn deduplicates_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_json(
            tmp.path(),
            ".claude.json",
            r#"{
            "mcpServers": {
                "github": { "command": "npx", "args": ["-y", "gh-active"] }
            }
        }"#,
        );
        write_json(
            tmp.path(),
            ".claude/mcp-configs/all.json",
            r#"{
            "mcpServers": {
                "github": { "command": "npx", "args": ["-y", "gh-template"] },
                "memory": { "command": "npx", "args": ["-y", "mem"] }
            }
        }"#,
        );

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 2);
        // .claude.json takes priority (loaded first)
        let gh = results.iter().find(|e| e.name == "github").unwrap();
        assert!(gh.args.iter().any(|a| a.contains("gh-active")));
    }

    #[test]
    fn skips_meta_keys() {
        let tmp = tempfile::tempdir().unwrap();
        write_json(
            tmp.path(),
            ".claude.json",
            r#"{
            "mcpServers": {
                "_comments": { "usage": "ignore" },
                "real": { "command": "node", "args": ["server.js"] }
            }
        }"#,
        );

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "real");
    }

    #[test]
    fn empty_home_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let results = McpDiscovery::discover_all(tmp.path());
        assert!(results.is_empty());
    }

    #[test]
    fn discovers_other_cli_mcps() {
        let tmp = tempfile::tempdir().unwrap();
        // Codex uses TOML config.toml
        write_json(
            tmp.path(),
            ".codex/config.toml",
            r#"
[mcp_servers.codex-tool]
type = "stdio"
command = "codex-mcp"
args = []
"#,
        );

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "codex-tool");
    }
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::Result;
use serde::Deserialize;

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
    pub source_cli: String,  // which CLI config this came from
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

        // 3. Other CLIs
        for (cli_dir, cli_name) in &[(".codex", "codex"), (".gemini", "gemini"), (".opencode", "opencode")] {
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

    fn convert_map(servers: Option<HashMap<String, McpServerDef>>, source: &Path, cli: &str) -> Vec<McpEntry> {
        let Some(servers) = servers else { return Vec::new() };
        servers.into_iter().filter_map(|(name, def)| {
            // Skip meta keys like _comments
            if name.starts_with('_') { return None; }

            let mcp_type = if def.server_type.as_deref() == Some("http") {
                McpType::Http { url: def.url.unwrap_or_default() }
            } else {
                McpType::Stdio
            };

            let command = match &mcp_type {
                McpType::Http { url } => url.clone(),
                McpType::Stdio => def.command.unwrap_or_default(),
            };

            if command.is_empty() { return None; }

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
        }).collect()
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
        write_json(tmp.path(), ".claude.json", r#"{
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
        }"#);

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 2);

        let github = results.iter().find(|e| e.name == "github").unwrap();
        assert_eq!(github.command, "npx");
        assert_eq!(github.args, vec!["-y", "@modelcontextprotocol/server-github"]);
        assert_eq!(github.mcp_type, McpType::Stdio);

        let vercel = results.iter().find(|e| e.name == "vercel").unwrap();
        assert_eq!(vercel.mcp_type, McpType::Http { url: "https://mcp.vercel.com".into() });
    }

    #[test]
    fn parse_mcp_configs_dir() {
        let tmp = tempfile::tempdir().unwrap();
        write_json(tmp.path(), ".claude/mcp-configs/servers.json", r#"{
            "mcpServers": {
                "memory": {
                    "command": "npx",
                    "args": ["-y", "@mcp/server-memory"],
                    "description": "Memory server"
                },
                "_comments": { "note": "skip this" }
            }
        }"#);

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "memory");
    }

    #[test]
    fn deduplicates_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        write_json(tmp.path(), ".claude.json", r#"{
            "mcpServers": {
                "github": { "command": "npx", "args": ["-y", "gh-active"] }
            }
        }"#);
        write_json(tmp.path(), ".claude/mcp-configs/all.json", r#"{
            "mcpServers": {
                "github": { "command": "npx", "args": ["-y", "gh-template"] },
                "memory": { "command": "npx", "args": ["-y", "mem"] }
            }
        }"#);

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 2);
        // .claude.json takes priority (loaded first)
        let gh = results.iter().find(|e| e.name == "github").unwrap();
        assert!(gh.args.iter().any(|a| a.contains("gh-active")));
    }

    #[test]
    fn skips_meta_keys() {
        let tmp = tempfile::tempdir().unwrap();
        write_json(tmp.path(), ".claude.json", r#"{
            "mcpServers": {
                "_comments": { "usage": "ignore" },
                "real": { "command": "node", "args": ["server.js"] }
            }
        }"#);

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
        write_json(tmp.path(), ".codex/settings.json", r#"{
            "mcpServers": {
                "codex-tool": { "command": "codex-mcp", "args": [] }
            }
        }"#);

        let results = McpDiscovery::discover_all(tmp.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "codex-tool");
    }
}

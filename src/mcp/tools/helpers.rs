use anyhow::Result;
use rmcp::serde_json;

use crate::core::cli_target::CliTarget;

/// Merge single name + names list into one vec.
pub(super) fn collect_names(name: Option<String>, names: Option<Vec<String>>) -> Vec<String> {
    let mut all = Vec::new();
    if let Some(n) = name {
        all.push(n);
    }
    if let Some(ns) = names {
        all.extend(ns);
    }
    all
}

/// Resolve group name fuzzily, returning the group_id or an error message.
pub(super) fn resolve_group(
    mgr: &crate::core::manager::SkillManager,
    name: &str,
) -> Result<String, String> {
    if let Some(id) = mgr.find_group_id(name) {
        Ok(id)
    } else {
        Err(format!(
            "Group not found: '{name}'. Use sm_groups to list available groups."
        ))
    }
}

/// Validate a string is safe for shell command usage (no injection).
pub(super) fn is_safe_shell_arg(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || "-_/.@".contains(c))
}

pub(super) fn parse_target(s: Option<&str>) -> CliTarget {
    s.unwrap_or("claude").parse().unwrap_or(CliTarget::Claude)
}

/// Notify Claude Code to reload a specific MCP server after config changes.
/// Uses `claude mcp remove + add-json` to force re-read.
/// Only relevant when target is Claude — other CLIs don't support hot-reload.
pub(super) fn sync_claude_mcp(mcp_name: &str) {
    // Read current config to get the MCP entry
    let home = dirs::home_dir().unwrap_or_default();
    let config_path = home.join(".claude.json");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let config: serde_json::Value = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(_) => return,
    };

    let entry = config.get("mcpServers").and_then(|s| s.get(mcp_name));

    match entry {
        Some(entry) => {
            // MCP exists in config → remove then re-add to force Claude Code to reconnect
            let json_str = serde_json::to_string(entry).unwrap_or_default();
            let _ = std::process::Command::new("claude")
                .args(["mcp", "remove", mcp_name, "-s", "user"])
                .output();
            let _ = std::process::Command::new("claude")
                .args(["mcp", "add-json", "-s", "user", mcp_name, &json_str])
                .output();
        }
        None => {
            // MCP was removed from config → tell Claude Code to disconnect
            let _ = std::process::Command::new("claude")
                .args(["mcp", "remove", mcp_name, "-s", "user"])
                .output();
        }
    }
}

/// Sync all MCP changes for Claude target after enable/disable operations.
pub(super) fn maybe_sync_claude(target: CliTarget, mcp_name: &str) {
    if target == CliTarget::Claude {
        sync_claude_mcp(mcp_name);
    }
}

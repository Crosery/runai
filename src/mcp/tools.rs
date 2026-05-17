use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::schemars;
use rmcp::serde_json;
use rmcp::{ServerHandler, model::ServerInfo, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use crate::core::cli_target::CliTarget;
use crate::core::manager::SkillManager;

pub struct SmServer {
    manager: Mutex<SkillManager>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl SmServer {
    pub fn new() -> Result<Self> {
        let manager = SkillManager::new()?;
        Ok(Self {
            manager: Mutex::new(manager),
            tool_router: Self::tool_router(),
        })
    }
}

// --- Parameter structs ---

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct ListResourcesParams {
    /// Filter by kind: 'skill' or 'mcp'
    pub kind: Option<String>,
    /// Filter by group name or ID
    pub group: Option<String>,
    /// CLI target for status display: claude, codex, gemini, opencode
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NameTargetParams {
    /// Resource name or group ID
    pub name: String,
    /// CLI target: claude, codex, gemini, opencode (default: claude)
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct UnifiedEnableParams {
    /// Single resource or group name
    pub name: Option<String>,
    /// Multiple resource/group names
    pub names: Option<Vec<String>>,
    /// CLI target: claude, codex, gemini, opencode (default: claude)
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NameParams {
    /// Resource or group name
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct UnifiedDeleteParams {
    /// Single resource name
    pub name: Option<String>,
    /// Multiple resource names
    pub names: Option<Vec<String>>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct TrashQueryParams {
    /// Trash entry ID or resource name
    pub query: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct StatusParams {
    /// CLI target: claude, codex, gemini, opencode
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct CreateGroupParams {
    /// Group ID (used as filename)
    pub id: String,
    /// Display name
    pub name: String,
    /// Description
    pub description: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct GroupMembersActionParams {
    /// Action: "add", "remove", or "update"
    pub action: String,
    /// Group ID
    pub group: String,
    /// Single resource name (for add/remove)
    pub name: Option<String>,
    /// Multiple resource names (for add/remove)
    pub names: Option<Vec<String>>,
    /// New display name (for update action only)
    pub display_name: Option<String>,
    /// New description (for update action only)
    pub description: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct MarketListParams {
    /// Source label or repo (e.g. "Anthropic Official" or "anthropics/claude-plugins-official")
    pub source: Option<String>,
    /// Search filter
    pub search: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct UnifiedMarketInstallParams {
    /// Single skill name to install
    pub name: Option<String>,
    /// Multiple skill names to install
    pub names: Option<Vec<String>>,
    /// Source repo (owner/repo), required if ambiguous
    pub source: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct InstallGitHubParams {
    /// GitHub repo in "owner/repo" or "owner/repo@branch" format, or full URL
    pub repo: String,
    /// CLI target to enable for: claude, codex, gemini, opencode (default: claude)
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct UsageStatsParams {
    /// Max entries to return (default: all)
    pub top: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct RecommendStatsParams {
    /// Only count events in the last N hours (omit for all-time)
    pub hours: Option<i64>,
    /// Also include the N most recent individual calls
    pub recent: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct RestoreParams {
    /// Backup timestamp (omit to use latest)
    pub timestamp: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct TextResult {
    pub result: String,
}

/// Merge single name + names list into one vec.
fn collect_names(name: Option<String>, names: Option<Vec<String>>) -> Vec<String> {
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
fn resolve_group(mgr: &crate::core::manager::SkillManager, name: &str) -> Result<String, String> {
    if let Some(id) = mgr.find_group_id(name) {
        Ok(id)
    } else {
        Err(format!(
            "Group not found: '{name}'. Use sm_groups to list available groups."
        ))
    }
}

/// Validate a string is safe for shell command usage (no injection).
fn is_safe_shell_arg(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || "-_/.@".contains(c))
}

fn parse_target(s: Option<&str>) -> CliTarget {
    s.unwrap_or("claude").parse().unwrap_or(CliTarget::Claude)
}

/// Notify Claude Code to reload a specific MCP server after config changes.
/// Uses `claude mcp remove + add-json` to force re-read.
/// Only relevant when target is Claude — other CLIs don't support hot-reload.
fn sync_claude_mcp(mcp_name: &str) {
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
fn maybe_sync_claude(target: CliTarget, mcp_name: &str) {
    if target == CliTarget::Claude {
        sync_claude_mcp(mcp_name);
    }
}

// --- Tool router ---

#[tool_router]
impl SmServer {
    // ── Query tools ──

    #[tool(
        description = "List skills/MCPs with status. Filter: kind='skill'|'mcp', group=ID, target=CLI. Shows usage count."
    )]
    fn sm_list(&self, Parameters(p): Parameters<ListResourcesParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let target = parse_target(p.target.as_deref());
        let resources = if let Some(ref group_id) = p.group {
            let gid = match mgr.find_group_id(group_id) {
                Some(id) => id,
                None => {
                    return Json(TextResult {
                        result: format!("Group not found: '{group_id}'"),
                    });
                }
            };
            mgr.get_group_members(&gid).unwrap_or_default()
        } else {
            let kind_filter = p.kind.as_deref().and_then(|kind| kind.parse().ok());
            mgr.list_resources(kind_filter, None).unwrap_or_default()
        };

        // Compact format: "● kind name [Nx]" one per line
        let mut lines = Vec::new();
        let mut enabled_count = 0;
        for r in &resources {
            let on = r.is_enabled_for(target);
            if on {
                enabled_count += 1;
            }
            let icon = if on { "●" } else { "○" };
            let usage = if r.usage_count > 0 {
                format!(" [{}x]", r.usage_count)
            } else {
                String::new()
            };
            lines.push(format!("{icon} {:<5} {}{usage}", r.kind.as_str(), r.name));
        }
        lines.insert(
            0,
            format!(
                "{} resources ({enabled_count} enabled for {})",
                resources.len(),
                target.name()
            ),
        );

        Json(TextResult {
            result: lines.join("\n"),
        })
    }

    #[tool(
        description = "List groups with member counts and description preview. Description is truncated to 200 chars; for full detail use the runai CLI `runai group show <id>`."
    )]
    fn sm_groups(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let groups = mgr.list_groups().unwrap_or_default();

        if groups.is_empty() {
            return Json(TextResult {
                result: "No groups. Use sm_create_group to create one.".into(),
            });
        }

        // One line per group with description preview (indented on next line if non-empty)
        let mut lines = vec![format!("{} groups:", groups.len())];
        for (id, g) in &groups {
            let members = mgr.get_group_members(id).unwrap_or_default();
            lines.push(format!("  {} ({}) — {} members", id, g.name, members.len()));
            if !g.description.is_empty() {
                let preview: String = g.description.chars().take(200).collect();
                let ellipsis = if g.description.chars().count() > 200 {
                    "…"
                } else {
                    ""
                };
                lines.push(format!("      {preview}{ellipsis}"));
            }
        }

        Json(TextResult {
            result: lines.join("\n"),
        })
    }

    #[tool(description = "Enabled/total counts per CLI target. Returns JSON.")]
    fn sm_status(&self, Parameters(p): Parameters<StatusParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let (skills, mcps) = mgr.status(target).unwrap_or((0, 0));
        let (ts, tm) = mgr.resource_count();

        let runai_version = env!("CARGO_PKG_VERSION");
        let pending = crate::core::updater::pending_update_version(mgr.paths().data_dir());

        let mut obj = serde_json::json!({
            "target": target.name(),
            "skills_enabled": skills, "skills_total": ts,
            "mcps_enabled": mcps, "mcps_total": tm,
            "runai_version": runai_version,
            "update_available": pending.is_some(),
        });
        if let Some(latest) = pending {
            obj["update_latest"] = serde_json::Value::String(latest);
            obj["update_hint"] = serde_json::Value::String(
                "Tell the user a newer runai is available and suggest `runai update`.".into(),
            );
        }
        Json(TextResult {
            result: obj.to_string(),
        })
    }

    // ── Enable/Disable ──

    #[tool(
        description = "Enable skill(s)/MCP(s)/group(s) for a CLI target. Pass 'name' for single or 'names' for multiple. Auto-detects groups vs items."
    )]
    fn sm_enable(&self, Parameters(p): Parameters<UnifiedEnableParams>) -> Json<TextResult> {
        let all_names = collect_names(p.name, p.names);
        if all_names.is_empty() {
            return Json(TextResult {
                result: "Provide 'name' or 'names' parameter.".into(),
            });
        }
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let groups = mgr.list_groups().unwrap_or_default();

        let mut results = Vec::new();
        for name in &all_names {
            let msg = if groups.iter().any(|(id, _)| id == name) {
                let r = mgr
                    .enable_group(name, target, None)
                    .map(|_| format!("Group '{name}' enabled for {}", target.name()))
                    .unwrap_or_else(|e| format!("'{name}': {e}"));
                if let Ok(members) = mgr.get_group_members(name) {
                    for m in &members {
                        if m.id.starts_with("mcp:") {
                            maybe_sync_claude(target, &m.name);
                        }
                    }
                }
                r
            } else {
                match mgr.find_resource_id(name) {
                    Some(id) => {
                        let is_mcp = id.starts_with("mcp:");
                        let r = mgr
                            .enable_resource(&id, target, None)
                            .map(|_| format!("'{name}' enabled for {}", target.name()))
                            .unwrap_or_else(|e| format!("'{name}': {e}"));
                        if is_mcp {
                            maybe_sync_claude(target, name);
                        }
                        r
                    }
                    None => format!(
                        "Not found: '{name}'. Try sm_scan first, or sm_market(search='{name}') to find it."
                    ),
                }
            };
            results.push(msg);
        }
        Json(TextResult {
            result: results.join("\n"),
        })
    }

    #[tool(
        description = "Disable skill(s)/MCP(s)/group(s) for a CLI target. Pass 'name' for single or 'names' for multiple. Auto-detects groups vs items."
    )]
    fn sm_disable(&self, Parameters(p): Parameters<UnifiedEnableParams>) -> Json<TextResult> {
        let all_names = collect_names(p.name, p.names);
        if all_names.is_empty() {
            return Json(TextResult {
                result: "Provide 'name' or 'names' parameter.".into(),
            });
        }
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let groups = mgr.list_groups().unwrap_or_default();

        let mut results = Vec::new();
        for name in &all_names {
            let msg = if groups.iter().any(|(id, _)| id == name) {
                let mcp_names: Vec<String> = mgr
                    .get_group_members(name)
                    .unwrap_or_default()
                    .iter()
                    .filter(|m| m.id.starts_with("mcp:"))
                    .map(|m| m.name.clone())
                    .collect();
                let r = mgr
                    .disable_group(name, target, None)
                    .map(|_| format!("Group '{name}' disabled for {}", target.name()))
                    .unwrap_or_else(|e| format!("'{name}': {e}"));
                for mcp_name in &mcp_names {
                    maybe_sync_claude(target, mcp_name);
                }
                r
            } else {
                match mgr.find_resource_id(name) {
                    Some(id) => {
                        let is_mcp = id.starts_with("mcp:");
                        let r = mgr
                            .disable_resource(&id, target, None)
                            .map(|_| format!("'{name}' disabled for {}", target.name()))
                            .unwrap_or_else(|e| format!("'{name}': {e}"));
                        if is_mcp {
                            maybe_sync_claude(target, name);
                        }
                        r
                    }
                    None => format!("Not found: '{name}'. Run sm_list to see available resources."),
                }
            };
            results.push(msg);
        }
        Json(TextResult {
            result: results.join("\n"),
        })
    }

    // ── Mutating tools ──

    #[tool(
        description = "Scan CLI dirs and adopt new skills. Run after install or manual file changes."
    )]
    fn sm_scan(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.scan() {
            Ok(r) => {
                let mut msg = format!("Scan: {} adopted, {} skipped", r.adopted, r.skipped);
                if !r.errors.is_empty() {
                    msg.push_str(&format!("\nErrors:\n  {}", r.errors.join("\n  ")));
                }
                msg
            }
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(
        description = "Move skill(s)/MCP(s) into trash. Pass 'name' for single or 'names' for multiple."
    )]
    fn sm_delete(&self, Parameters(p): Parameters<UnifiedDeleteParams>) -> Json<TextResult> {
        let all_names = collect_names(p.name, p.names);
        if all_names.is_empty() {
            return Json(TextResult {
                result: "Provide 'name' or 'names' parameter.".into(),
            });
        }
        let mgr = self.manager.lock().unwrap();
        let mut results = Vec::new();
        for name in &all_names {
            let msg = match mgr.find_resource_id(name) {
                Some(id) => match mgr.trash_resource(&id) {
                    Ok(_) => {
                        if id.starts_with("mcp:") {
                            sync_claude_mcp(name);
                        }
                        format!("Moved '{name}' to trash")
                    }
                    Err(e) => format!("'{name}': {e}"),
                },
                None => format!("Not found: '{name}'. Run sm_list to see available resources."),
            };
            results.push(msg);
        }
        Json(TextResult {
            result: results.join("\n"),
        })
    }

    #[tool(description = "List trash entries managed globally by runai.")]
    fn sm_trash(&self) -> Json<TextResult> {
        use crate::core::resource::format_time_ago;

        let mgr = self.manager.lock().unwrap();
        match mgr.list_trash() {
            Ok(entries) => {
                if entries.is_empty() {
                    Json(TextResult {
                        result: "Trash is empty.".into(),
                    })
                } else {
                    let mut lines = vec![format!("{} trashed resources:", entries.len())];
                    for entry in &entries {
                        lines.push(format!(
                            "  [{}] {} — {} ({})",
                            entry.kind.as_str(),
                            entry.id,
                            entry.name,
                            format_time_ago(Some(entry.deleted_at))
                        ));
                    }
                    Json(TextResult {
                        result: lines.join("\n"),
                    })
                }
            }
            Err(e) => Json(TextResult {
                result: format!("Error: {e}"),
            }),
        }
    }

    #[tool(description = "Restore a trashed resource by trash entry ID or resource name.")]
    fn sm_trash_restore(&self, Parameters(p): Parameters<TrashQueryParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let entry = match mgr
            .list_trash()
            .unwrap_or_default()
            .into_iter()
            .find(|entry| entry.id == p.query || entry.name == p.query)
        {
            Some(entry) => entry,
            None => {
                return Json(TextResult {
                    result: format!("Trash entry not found: '{}'", p.query),
                });
            }
        };

        let result = match mgr.restore_from_trash(&entry.id) {
            Ok(_) => {
                if entry.kind == crate::core::resource::ResourceKind::Mcp {
                    sync_claude_mcp(&entry.name);
                }
                format!("Restored '{}'", entry.name)
            }
            Err(e) => format!("'{}': {e}", entry.name),
        };
        Json(TextResult { result })
    }

    #[tool(
        description = "Permanently delete a trashed resource by trash entry ID or resource name."
    )]
    fn sm_trash_purge(&self, Parameters(p): Parameters<TrashQueryParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let entry = match mgr
            .list_trash()
            .unwrap_or_default()
            .into_iter()
            .find(|entry| entry.id == p.query || entry.name == p.query)
        {
            Some(entry) => entry,
            None => {
                return Json(TextResult {
                    result: format!("Trash entry not found: '{}'", p.query),
                });
            }
        };

        let result = match mgr.purge_trash(&entry.id) {
            Ok(_) => format!("Permanently deleted '{}'", entry.name),
            Err(e) => format!("'{}': {e}", entry.name),
        };
        Json(TextResult { result })
    }

    // ── Group management ──

    #[tool(description = "Create a new group")]
    fn sm_create_group(&self, Parameters(p): Parameters<CreateGroupParams>) -> Json<TextResult> {
        use crate::core::group::{Group, GroupKind};
        let group = Group {
            name: p.name,
            description: p.description.unwrap_or_default(),
            kind: GroupKind::Custom,
            auto_enable: false,
            members: vec![],
        };
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.create_group(&p.id, &group) {
            Ok(_) => format!("Group '{}' created", p.id),
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(description = "Delete a group (does not delete its members)")]
    fn sm_delete_group(&self, Parameters(p): Parameters<NameParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let path = mgr.paths().groups_dir().join(format!("{}.toml", p.name));
        if path.exists() {
            let _ = std::fs::remove_file(&path);
            Json(TextResult {
                result: format!("Group '{}' deleted", p.name),
            })
        } else {
            Json(TextResult {
                result: format!("Group not found: {}", p.name),
            })
        }
    }

    #[tool(
        description = "Manage group members. action: 'add' (add resources), 'remove' (remove resources), 'update' (rename/redescribe). Pass 'name'/'names' for add/remove, 'display_name'/'description' for update."
    )]
    fn sm_group_members(
        &self,
        Parameters(p): Parameters<GroupMembersActionParams>,
    ) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let gid = match resolve_group(&mgr, &p.group) {
            Ok(id) => id,
            Err(e) => return Json(TextResult { result: e }),
        };

        let result = match p.action.as_str() {
            "add" => {
                let all_names = collect_names(p.name, p.names);
                if all_names.is_empty() {
                    return Json(TextResult {
                        result: "Provide 'name' or 'names' parameter.".into(),
                    });
                }
                let mut added = 0;
                let mut errors = Vec::new();
                for name in &all_names {
                    match mgr.find_resource_id(name) {
                        Some(rid) => match mgr.db().add_group_member(&gid, &rid) {
                            Ok(_) => added += 1,
                            Err(e) => errors.push(format!("{name}: {e}")),
                        },
                        None => errors.push(format!("{name}: not found")),
                    }
                }
                let mut msg = format!("Added {added}/{} to group '{gid}'", all_names.len());
                if !errors.is_empty() {
                    msg.push_str(&format!("\nErrors: {}", errors.join(", ")));
                }
                msg
            }
            "remove" => {
                let all_names = collect_names(p.name, p.names);
                if all_names.is_empty() {
                    return Json(TextResult {
                        result: "Provide 'name' or 'names' parameter.".into(),
                    });
                }
                let mut removed = 0;
                let mut errors = Vec::new();
                for name in &all_names {
                    match mgr.find_resource_id(name) {
                        Some(rid) => match mgr.db().remove_group_member(&gid, &rid) {
                            Ok(_) => removed += 1,
                            Err(e) => errors.push(format!("{name}: {e}")),
                        },
                        None => errors.push(format!("{name}: not found")),
                    }
                }
                let mut msg = format!("Removed {removed}/{} from group '{gid}'", all_names.len());
                if !errors.is_empty() {
                    msg.push_str(&format!("\nErrors: {}", errors.join(", ")));
                }
                msg
            }
            "update" => {
                match mgr.update_group(&gid, p.display_name.as_deref(), p.description.as_deref()) {
                    Ok(_) => {
                        let mut changes = Vec::new();
                        if let Some(n) = &p.display_name {
                            changes.push(format!("name='{n}'"));
                        }
                        if let Some(d) = &p.description {
                            changes.push(format!("desc='{d}'"));
                        }
                        format!("Group '{gid}' updated: {}", changes.join(", "))
                    }
                    Err(e) => format!("Error: {e}"),
                }
            }
            _ => "Invalid action. Use: add, remove, or update".into(),
        };

        Json(TextResult { result })
    }

    // ── Market ──

    #[tool(
        description = "Search market for skills. Use search='keyword' to filter. Returns installable skill names."
    )]
    fn sm_market(&self, Parameters(p): Parameters<MarketListParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let sources = crate::core::market::load_sources(&data_dir);

        let installed: Vec<String> = mgr
            .list_resources(None, None)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.name)
            .collect();

        let mut all_skills = Vec::new();
        for src in &sources {
            if !src.enabled {
                continue;
            }
            if let Some(ref filter) = p.source {
                let f = filter.to_lowercase();
                if !src.label.to_lowercase().contains(&f)
                    && !src.repo_id().to_lowercase().contains(&f)
                {
                    continue;
                }
            }
            if let Some(cached) = crate::core::market::load_cache(&data_dir, src) {
                let mut matcher = crate::core::search::new_matcher();
                for mut skill in cached {
                    skill.installed = installed.contains(&skill.name);
                    if let Some(ref search) = p.search {
                        let matched = crate::core::search::fuzzy_score_any(
                            &mut matcher,
                            search,
                            &[&skill.name, &skill.repo_path, &skill.source_label],
                        )
                        .is_some();
                        if !matched {
                            continue;
                        }
                    }
                    all_skills.push(serde_json::json!({
                        "name": skill.name,
                        "source": skill.source_label,
                        "installed": skill.installed,
                    }));
                }
            }
        }

        if all_skills.is_empty() {
            // Check if any matched source is a plugin (not a skill collection)
            for src in &sources {
                if !src.enabled {
                    continue;
                }
                if let Some(ref filter) = p.source {
                    let f = filter.to_lowercase();
                    if !src.label.to_lowercase().contains(&f)
                        && !src.repo_id().to_lowercase().contains(&f)
                    {
                        continue;
                    }
                }
                if crate::core::market::is_plugin_source(&data_dir, src) {
                    return Json(TextResult {
                        result: format!(
                            "This is a Claude Code plugin, not a skill collection. Install with:\n  /plugin install {}@<marketplace>\n\nOr check the repo README for install instructions.",
                            src.repo
                        ),
                    });
                }
            }
            if let Some(ref search) = p.search {
                return Json(TextResult {
                    result: format!(
                        "No skills matching '{}'. Check available sources in TUI Market tab.",
                        search
                    ),
                });
            }
        }

        Json(TextResult {
            result: serde_json::to_string_pretty(&all_skills).unwrap_or_default(),
        })
    }

    #[tool(
        description = "Install market skill(s). Returns Bash command(s) — run them, don't wait for MCP. Pass 'name' for single or 'names' for multiple."
    )]
    fn sm_market_install(
        &self,
        Parameters(p): Parameters<UnifiedMarketInstallParams>,
    ) -> Json<TextResult> {
        let all_names = collect_names(p.name, p.names);
        if all_names.is_empty() {
            return Json(TextResult {
                result: "Provide 'name' or 'names' parameter.".into(),
            });
        }
        // Validate all names and source before generating commands
        for name in &all_names {
            if !is_safe_shell_arg(name) {
                return Json(TextResult {
                    result: format!(
                        "Invalid name: '{name}'. Only alphanumeric, -, _, ., / allowed."
                    ),
                });
            }
        }
        if let Some(ref src) = p.source
            && !is_safe_shell_arg(src)
        {
            return Json(TextResult {
                result: format!(
                    "Invalid source: '{src}'. Only alphanumeric, -, _, ., /, @ allowed."
                ),
            });
        }
        let cmds: Vec<String> = all_names
            .iter()
            .map(|name| {
                let mut cmd = format!("runai market-install {name}");
                if let Some(ref src) = p.source {
                    cmd.push_str(&format!(" --source '{src}'"));
                }
                cmd
            })
            .collect();

        if cmds.len() == 1 {
            Json(TextResult {
                result: format!(
                    "Run this command via Bash tool:\n\n{}\n\nDo NOT wait for MCP — CLI is much faster.",
                    cmds[0]
                ),
            })
        } else {
            Json(TextResult {
                result: format!(
                    "Run these commands via Bash tool (one by one or with &&):\n\n{}\n\nThen run: runai scan",
                    cmds.join("\n")
                ),
            })
        }
    }

    #[tool(
        description = "Install skills from a GitHub repo. Returns a CLI command for the agent to execute via Bash tool. Do NOT call this and wait — run the returned command instead."
    )]
    fn sm_install(&self, Parameters(p): Parameters<InstallGitHubParams>) -> Json<TextResult> {
        let input = p
            .repo
            .trim()
            .trim_start_matches("https://github.com/")
            .trim_end_matches('/');

        if !is_safe_shell_arg(input) {
            return Json(TextResult {
                result: format!("Invalid repo format: '{}'. Use owner/repo.", input),
            });
        }

        Json(TextResult {
            result: format!(
                "Run this command via Bash tool:\n\nrune install {input}\n\nThis downloads skills concurrently and is much faster than running inside MCP."
            ),
        })
    }

    // ── Unified search ──

    #[tool(
        description = "Search across installed resources AND market. Returns local matches first, then market results. Use for finding skills/MCPs to enable or install."
    )]
    fn sm_search(&self, Parameters(p): Parameters<NameParams>) -> Json<TextResult> {
        use crate::core::search::{fuzzy_score_any, new_matcher};
        let mgr = self.manager.lock().unwrap();
        let q = p.name.clone();
        let mut matcher = new_matcher();
        let mut lines = Vec::new();

        // 1. Search installed resources (fuzzy on name + description)
        let resources = mgr.list_resources(None, None).unwrap_or_default();
        let mut local_scored: Vec<(&_, u32)> = resources
            .iter()
            .filter_map(|r| {
                fuzzy_score_any(&mut matcher, &q, &[&r.name, &r.description]).map(|s| (r, s))
            })
            .collect();
        // Higher score first; tiebreak by usage_count desc.
        local_scored.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.usage_count.cmp(&a.0.usage_count)));

        if !local_scored.is_empty() {
            lines.push(format!("── Installed ({}) ──", local_scored.len()));
            for (r, _) in &local_scored {
                let icon = if r.enabled.values().any(|&v| v) {
                    "●"
                } else {
                    "○"
                };
                let usage = if r.usage_count > 0 {
                    format!(" [{}x]", r.usage_count)
                } else {
                    String::new()
                };
                lines.push(format!("{icon} {:<5} {}{usage}", r.kind.as_str(), r.name));
            }
        }

        // 2. Search market (fuzzy on name + repo_path)
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let sources = crate::core::market::load_sources(&data_dir);
        let installed_names: Vec<String> = resources.iter().map(|r| r.name.clone()).collect();
        let mut market_scored: Vec<(String, u32)> = Vec::new();

        for src in &sources {
            if !src.enabled {
                continue;
            }
            if let Some(cached) = crate::core::market::load_cache(&data_dir, src) {
                for skill in cached {
                    if installed_names.contains(&skill.name) {
                        continue;
                    }
                    if let Some(score) =
                        fuzzy_score_any(&mut matcher, &q, &[&skill.name, &skill.repo_path])
                    {
                        market_scored
                            .push((format!("  {} ({})", skill.name, skill.source_label), score));
                    }
                }
            }
        }
        market_scored.sort_by(|a, b| b.1.cmp(&a.1));
        let market_matches: Vec<String> = market_scored.into_iter().map(|(s, _)| s).collect();

        if !market_matches.is_empty() {
            lines.push(format!("\n── Market ({}) ──", market_matches.len()));
            lines.extend(market_matches.into_iter().take(20));
            lines.push("Use sm_market_install(name='...') to install.".into());
        }

        if lines.is_empty() {
            Json(TextResult {
                result: format!(
                    "No results for '{q}' in installed or market.\n\n\
                     Try these fallbacks:\n\
                     1. npx skills find {q}  ← search skills.sh ecosystem\n\
                     2. Web search: '{q} claude code skill github'\n\
                     3. Check enabled market sources in TUI Market tab\n\n\
                     If you find a repo, install with: runai install owner/repo"
                ),
            })
        } else {
            Json(TextResult {
                result: lines.join("\n"),
            })
        }
    }

    // ── Usage tracking ──

    #[tool(
        description = "Show usage statistics for all skills and MCPs, sorted by most used. Helps identify unused resources."
    )]
    fn sm_usage_stats(&self, Parameters(p): Parameters<UsageStatsParams>) -> Json<TextResult> {
        use crate::core::resource::format_time_ago;
        let mgr = self.manager.lock().unwrap();
        match mgr.usage_stats() {
            Ok(stats) => {
                let limit = p.top.unwrap_or(usize::MAX);
                let mut lines = Vec::new();
                for (i, s) in stats.iter().enumerate() {
                    if i >= limit {
                        break;
                    }
                    let ago = format_time_ago(s.last_used_at);
                    let kind = if s.id.starts_with("mcp:") {
                        "mcp"
                    } else {
                        "skill"
                    };
                    lines.push(format!(
                        "{:>4}x  {:>8}  {:<5}  {}",
                        s.count, ago, kind, s.name
                    ));
                }
                if lines.is_empty() {
                    Json(TextResult {
                        result: "No usage data yet.".into(),
                    })
                } else {
                    lines.insert(
                        0,
                        format!("{:>4}   {:>8}  {:<5}  {}", "uses", "last", "type", "name"),
                    );
                    Json(TextResult {
                        result: lines.join("\n"),
                    })
                }
            }
            Err(e) => Json(TextResult {
                result: format!("Error: {e}"),
            }),
        }
    }

    // ── Backup ──

    #[tool(description = "Create a backup of all CLI skill directories and config files")]
    fn sm_backup(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match crate::core::backup::create_backup(mgr.paths()) {
            Ok(dir) => format!("Backup created: {}", dir.display()),
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(description = "Restore from backup. Omit timestamp to use latest.")]
    fn sm_restore(&self, Parameters(p): Parameters<RestoreParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let paths = mgr.paths();
        let ts = match p.timestamp {
            Some(t) => t,
            None => match crate::core::backup::list_backups(paths).into_iter().next() {
                Some(t) => t,
                None => {
                    return Json(TextResult {
                        result: "No backups found".into(),
                    });
                }
            },
        };
        let result = match crate::core::backup::restore_backup(paths, &ts) {
            Ok(n) => format!("Restored {n} items from backup {ts}"),
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(description = "List available backups (newest first)")]
    fn sm_backups(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let list = crate::core::backup::list_backups(mgr.paths());
        if list.is_empty() {
            Json(TextResult {
                result: "No backups found".into(),
            })
        } else {
            Json(TextResult {
                result: list.join("\n"),
            })
        }
    }

    #[tool(
        description = "Show runai recommend router LLM telemetry: total calls, token spend per model, average latency. Pass `hours` to limit the window (e.g. last 24 hours), omit for all-time. Pass `recent` to also include the N newest individual calls. Useful when the user asks 'how much have I spent on the router' / 'router 用量多少' / 'which model picked what'."
    )]
    fn sm_recommend_stats(
        &self,
        Parameters(p): Parameters<RecommendStatsParams>,
    ) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let since_ts = p.hours.map(|h| chrono::Utc::now().timestamp() - h * 3600);
        let summary = match mgr.db().router_stats_summary(since_ts) {
            Ok(s) => s,
            Err(e) => {
                return Json(TextResult {
                    result: format!("Error: {e}"),
                });
            }
        };
        let mut lines = Vec::new();
        let window_label = match p.hours {
            Some(h) => format!("last {h}h"),
            None => "all-time".into(),
        };
        lines.push(format!("Router LLM telemetry ({window_label})"));
        lines.push(format!("  total calls:        {}", summary.total_calls));
        lines.push(format!("  errors:             {}", summary.errors));
        if let Some(ms) = summary.avg_latency_ms {
            lines.push(format!("  avg latency:        {ms:.0} ms"));
        }
        lines.push(format!(
            "  prompt tokens:      {}",
            summary.total_prompt_tokens
        ));
        lines.push(format!(
            "  completion tokens:  {}",
            summary.total_completion_tokens
        ));
        lines.push(format!(
            "  reasoning tokens:   {}",
            summary.total_reasoning_tokens
        ));
        lines.push(format!("  total tokens:       {}", summary.total_tokens));
        if !summary.per_model.is_empty() {
            lines.push(String::new());
            lines.push("  per model:".into());
            for m in &summary.per_model {
                lines.push(format!(
                    "    {:<28} {:>5} calls  {:>9} tokens",
                    m.model, m.calls, m.total_tokens
                ));
            }
        }
        let recent_n = p.recent.unwrap_or(0);
        if recent_n > 0
            && let Ok(events) = mgr.db().router_recent_events(recent_n)
        {
            lines.push(String::new());
            lines.push("  recent calls (newest first):".into());
            for ev in &events {
                let when = chrono::DateTime::<chrono::Utc>::from_timestamp(ev.ts, 0)
                    .map(|d| {
                        d.with_timezone(&chrono::Local)
                            .format("%m-%d %H:%M:%S")
                            .to_string()
                    })
                    .unwrap_or_default();
                lines.push(format!(
                    "    {when}  {:<22}  {:>5}t  {:>5}ms  {}",
                    ev.model, ev.total_tokens, ev.latency_ms, ev.chosen_skills_json
                ));
            }
        }
        Json(TextResult {
            result: lines.join("\n"),
        })
    }
}

#[tool_handler]
impl ServerHandler for SmServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();

        // Check for a pending upgrade ONCE at server handshake. If a release
        // drops mid-session, `sm_status` will still expose it on each call.
        let update_line = {
            let mgr = self.manager.lock().unwrap();
            crate::core::updater::pending_update_version(mgr.paths().data_dir())
                .map(|latest| {
                    format!(
                        "\n\nUPDATE AVAILABLE: runai v{latest} (current v{}). \
                         Tell the user to run `runai update`.",
                        env!("CARGO_PKG_VERSION")
                    )
                })
                .unwrap_or_default()
        };

        info.instructions = Some(format!(
            "Runai v{} — AI skill/MCP manager.\n\
             \n\
             SKILL DISCOVERY (proactive):\n\
             1. sm_search → find skills (local + market)\n\
             2. sm_market_install → install (returns CLI command, run via Bash)\n\
             3. Fallback: Bash `npx skills find <keyword>` or `runai install owner/repo`\n\
             4. After install → sm_scan, sm_enable\n\
             \n\
             CORE: sm_list, sm_status, sm_enable, sm_disable, sm_search, sm_scan, sm_delete\n\
             INSTALL: sm_install(repo), sm_market_install\n\
             GROUPS: sm_groups, sm_create_group, sm_delete_group, sm_group_members\n\
             TRASH: sm_trash, sm_trash_restore, sm_trash_purge\n\
             STATS: sm_usage_stats\n\
             BACKUP: sm_backup, sm_backups, sm_restore\n\
             MARKET: sm_market{update_line}",
            env!("CARGO_PKG_VERSION"),
        ));
        info.capabilities = rmcp::model::ServerCapabilities::builder()
            .enable_tools()
            .build();
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::HOME_LOCK;
    use rmcp::handler::server::wrapper::Parameters;

    fn with_temp_home_server<F: FnOnce(&SmServer)>(f: F) {
        let _guard = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let server = SmServer::new().unwrap();
        f(&server);
        drop(server);

        unsafe {
            match original {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    #[test]
    fn tool_router_has_expected_tools() {
        with_temp_home_server(|server| {
            let tools = server.tool_router.list_all();
            let tool_names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
            eprintln!("Registered tools: {}", tools.len());
            for name in &tool_names {
                eprintln!("  - {name}");
            }

            // 21 core expected tools
            let expected_core = [
                "sm_list",
                "sm_status",
                "sm_enable",
                "sm_disable",
                "sm_search",
                "sm_scan",
                "sm_delete",
                "sm_trash",
                "sm_trash_restore",
                "sm_trash_purge",
                "sm_install",
                "sm_market",
                "sm_market_install",
                "sm_groups",
                "sm_create_group",
                "sm_delete_group",
                "sm_group_members",
                "sm_usage_stats",
                "sm_backup",
                "sm_backups",
                "sm_restore",
            ];
            for name in &expected_core {
                assert!(
                    tool_names.iter().any(|t| t == name),
                    "Expected core tool '{name}' not found"
                );
            }

            // 13 removed tools
            let removed = [
                "sm_batch_enable",
                "sm_batch_disable",
                "sm_batch_delete",
                "sm_batch_install",
                "sm_group_enable",
                "sm_group_disable",
                "sm_group_add",
                "sm_group_remove",
                "sm_update_group",
                "sm_register",
                "sm_record_usage",
                "sm_discover",
                "sm_sources",
            ];
            for name in &removed {
                assert!(
                    !tool_names.iter().any(|t| t == name),
                    "Removed tool '{name}' should not be present"
                );
            }

            assert_eq!(tools.len(), 22, "Expected 22 tools, got {}", tools.len());
        });
    }

    #[test]
    fn sm_status_returns_valid_json() {
        with_temp_home_server(|server| {
            let Json(result) = server.sm_status(Parameters(StatusParams { target: None }));
            let parsed: serde_json::Value =
                serde_json::from_str(&result.result).expect("sm_status should return valid JSON");

            assert!(parsed.get("target").is_some(), "missing 'target' field");
            assert!(
                parsed.get("skills_enabled").is_some(),
                "missing 'skills_enabled' field"
            );
            assert!(
                parsed.get("skills_total").is_some(),
                "missing 'skills_total' field"
            );
            assert!(
                parsed.get("mcps_enabled").is_some(),
                "missing 'mcps_enabled' field"
            );
            assert!(
                parsed.get("mcps_total").is_some(),
                "missing 'mcps_total' field"
            );
            assert_eq!(parsed["target"], "claude");
        });
    }

    #[test]
    fn sm_backups_returns_string() {
        with_temp_home_server(|server| {
            let Json(result) = server.sm_backups();
            // With no backups, should return "No backups found"
            // With backups, should return newline-separated timestamps
            assert!(
                !result.result.is_empty(),
                "sm_backups should return a non-empty string"
            );
        });
    }

    #[test]
    fn sm_groups_renders_description_preview() {
        with_temp_home_server(|server| {
            // Create a group with a long description, verify sm_groups surfaces it.
            let long_desc =
                "This describes what the group is for and which skills belong to it.".repeat(5);
            let _ = server.sm_create_group(Parameters(CreateGroupParams {
                id: "demo-group".into(),
                name: "Demo Group".into(),
                description: Some(long_desc.clone()),
            }));

            let Json(result) = server.sm_groups();
            assert!(
                result.result.contains("demo-group"),
                "group id missing from output: {}",
                result.result
            );
            assert!(
                result.result.contains("Demo Group"),
                "display name missing from output: {}",
                result.result
            );
            assert!(
                result.result.contains("This describes what"),
                "description preview missing from output: {}",
                result.result
            );
        });

        with_temp_home_server(|server| {
            // Empty description must not produce an empty preview line.
            let _ = server.sm_create_group(Parameters(CreateGroupParams {
                id: "no-desc".into(),
                name: "No Desc".into(),
                description: None,
            }));
            let Json(result) = server.sm_groups();
            // Find the line for "no-desc" and confirm the following line (if any) is another group, not an empty preview.
            let lines: Vec<&str> = result.result.lines().collect();
            let idx = lines
                .iter()
                .position(|l| l.contains("no-desc"))
                .expect("no-desc line should exist");
            if idx + 1 < lines.len() {
                let next = lines[idx + 1];
                assert!(
                    !next.trim_start().is_empty() || next.starts_with("  "),
                    "should not emit an indented preview line for empty description, got: {next:?}"
                );
                assert!(
                    !next.starts_with("      "),
                    "should not emit a 6-space description preview line for empty description, got: {next:?}"
                );
            }
        });
    }

    #[test]
    fn sm_search_no_results_suggests_npx_skills_find() {
        with_temp_home_server(|server| {
            let Json(result) = server.sm_search(Parameters(NameParams {
                name: "xyznonexistent99999".into(),
            }));
            assert!(
                result.result.contains("npx skills find"),
                "no-results message should suggest npx skills find, got: {}",
                result.result
            );
        });
    }
}

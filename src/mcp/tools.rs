use rmcp::{ServerHandler, tool, tool_router, tool_handler, model::ServerInfo};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Parameters, Json};
use rmcp::serde_json;
use rmcp::schemars;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

use crate::core::manager::SkillManager;
use crate::core::cli_target::CliTarget;

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
    /// Filter by group ID
    pub group: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NameTargetParams {
    /// Resource name or group ID
    pub name: String,
    /// CLI target: claude, codex, gemini, opencode (default: claude)
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct NameParams {
    /// Resource or group name
    pub name: String,
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
pub struct GroupMemberParams {
    /// Group ID
    pub group: String,
    /// Resource name to add/remove
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct MarketListParams {
    /// Source label or repo (e.g. "Anthropic Official" or "anthropics/claude-plugins-official")
    pub source: Option<String>,
    /// Search filter
    pub search: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct MarketInstallParams {
    /// Skill name to install
    pub name: String,
    /// Source repo (owner/repo), required if ambiguous
    pub source: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct MarketSourceParams {
    /// Action: "list", "add", "remove", "enable", "disable"
    pub action: String,
    /// Source repo (owner/repo) for add/remove/enable/disable
    pub repo: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct TextResult {
    pub result: String,
}

fn parse_target(s: Option<&str>) -> CliTarget {
    CliTarget::from_str(s.unwrap_or("claude")).unwrap_or(CliTarget::Claude)
}

// --- Tool router ---

#[tool_router]
impl SmServer {
    // ── Query tools ──

    #[tool(description = "List all managed skills and MCP servers. Filter by kind ('skill'/'mcp') or group ID.")]
    fn sm_list(&self, Parameters(p): Parameters<ListResourcesParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let resources = if let Some(group_id) = p.group {
            mgr.db().get_group_members(&group_id).unwrap_or_default()
        } else {
            let kind_filter = p.kind
                .as_deref()
                .and_then(crate::core::resource::ResourceKind::from_str);
            mgr.list_resources(kind_filter, None).unwrap_or_default()
        };

        let items: Vec<serde_json::Value> = resources.iter().map(|r| {
            serde_json::json!({
                "name": r.name,
                "kind": r.kind.as_str(),
                "description": r.description,
                "enabled": {
                    "claude": r.is_enabled_for(CliTarget::Claude),
                    "codex": r.is_enabled_for(CliTarget::Codex),
                    "gemini": r.is_enabled_for(CliTarget::Gemini),
                    "opencode": r.is_enabled_for(CliTarget::OpenCode),
                }
            })
        }).collect();

        Json(TextResult { result: serde_json::to_string_pretty(&items).unwrap_or_default() })
    }

    #[tool(description = "List all groups with member counts")]
    fn sm_groups(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let groups = mgr.list_groups().unwrap_or_default();

        let items: Vec<serde_json::Value> = groups.iter().map(|(id, g)| {
            let members = mgr.db().get_group_members(id).unwrap_or_default();
            serde_json::json!({
                "id": id,
                "name": g.name,
                "description": g.description,
                "members": members.len(),
            })
        }).collect();

        Json(TextResult { result: serde_json::to_string_pretty(&items).unwrap_or_default() })
    }

    #[tool(description = "Show enabled/total resource counts for a CLI target")]
    fn sm_status(&self, Parameters(p): Parameters<StatusParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let (skills, mcps) = mgr.status(target).unwrap_or((0, 0));
        let (ts, tm) = mgr.db().resource_count().unwrap_or((0, 0));
        let result = serde_json::json!({
            "target": target.name(),
            "skills_enabled": skills, "skills_total": ts,
            "mcps_enabled": mcps, "mcps_total": tm,
        }).to_string();
        Json(TextResult { result })
    }

    // ── Enable/Disable ──

    #[tool(description = "Enable a skill, MCP, or group for a CLI target")]
    fn sm_enable(&self, Parameters(p): Parameters<NameTargetParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();

        let groups = mgr.list_groups().unwrap_or_default();
        let result = if groups.iter().any(|(id, _)| id == &p.name) {
            mgr.enable_group(&p.name, target, None)
                .map(|_| format!("Group '{}' enabled for {}", p.name, target.name()))
                .unwrap_or_else(|e| format!("Error: {e}"))
        } else {
            match mgr.find_resource_id(&p.name) {
                Some(id) => mgr.enable_resource(&id, target, None)
                    .map(|_| format!("'{}' enabled for {}", p.name, target.name()))
                    .unwrap_or_else(|e| format!("Error: {e}")),
                None => format!("Not found: {}", p.name),
            }
        };
        Json(TextResult { result })
    }

    #[tool(description = "Disable a skill, MCP, or group for a CLI target")]
    fn sm_disable(&self, Parameters(p): Parameters<NameTargetParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();

        let groups = mgr.list_groups().unwrap_or_default();
        let result = if groups.iter().any(|(id, _)| id == &p.name) {
            mgr.disable_group(&p.name, target, None)
                .map(|_| format!("Group '{}' disabled for {}", p.name, target.name()))
                .unwrap_or_else(|e| format!("Error: {e}"))
        } else {
            match mgr.find_resource_id(&p.name) {
                Some(id) => mgr.disable_resource(&id, target, None)
                    .map(|_| format!("'{}' disabled for {}", p.name, target.name()))
                    .unwrap_or_else(|e| format!("Error: {e}")),
                None => format!("Not found: {}", p.name),
            }
        };
        Json(TextResult { result })
    }

    // ── Mutating tools ──

    #[tool(description = "Scan CLI directories for new skills and MCPs")]
    fn sm_scan(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.scan() {
            Ok(r) => format!("Scan: {} adopted, {} skipped, {} errors",
                r.adopted, r.skipped, r.errors.len()),
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(description = "Delete a skill or MCP (removes files, symlinks, and DB entry)")]
    fn sm_delete(&self, Parameters(p): Parameters<NameParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.find_resource_id(&p.name) {
            Some(id) => match mgr.uninstall(&id) {
                Ok(_) => format!("Deleted '{}'", p.name),
                Err(e) => format!("Error: {e}"),
            },
            None => format!("Not found: {}", p.name),
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
            Json(TextResult { result: format!("Group '{}' deleted", p.name) })
        } else {
            Json(TextResult { result: format!("Group not found: {}", p.name) })
        }
    }

    #[tool(description = "Add a skill or MCP to a group")]
    fn sm_group_add(&self, Parameters(p): Parameters<GroupMemberParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.find_resource_id(&p.name) {
            Some(rid) => match mgr.db().add_group_member(&p.group, &rid) {
                Ok(_) => format!("Added '{}' to group '{}'", p.name, p.group),
                Err(e) => format!("Error: {e}"),
            },
            None => format!("Resource not found: {}", p.name),
        };
        Json(TextResult { result })
    }

    #[tool(description = "Remove a skill or MCP from a group")]
    fn sm_group_remove(&self, Parameters(p): Parameters<GroupMemberParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.find_resource_id(&p.name) {
            Some(rid) => match mgr.db().remove_group_member(&p.group, &rid) {
                Ok(_) => format!("Removed '{}' from group '{}'", p.name, p.group),
                Err(e) => format!("Error: {e}"),
            },
            None => format!("Resource not found: {}", p.name),
        };
        Json(TextResult { result })
    }

    #[tool(description = "Enable all skills/MCPs in a group for a CLI target")]
    fn sm_group_enable(&self, Parameters(p): Parameters<NameTargetParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.enable_group(&p.name, target, None) {
            Ok(_) => format!("Group '{}' enabled for {}", p.name, target.name()),
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(description = "Disable all skills/MCPs in a group for a CLI target")]
    fn sm_group_disable(&self, Parameters(p): Parameters<NameTargetParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.disable_group(&p.name, target, None) {
            Ok(_) => format!("Group '{}' disabled for {}", p.name, target.name()),
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    // ── Market ──

    #[tool(description = "Browse market skills from cached sources. Returns skill names available for install.")]
    fn sm_market(&self, Parameters(p): Parameters<MarketListParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let sources = crate::core::market::load_sources(&data_dir);

        let installed: Vec<String> = mgr.list_resources(None, None)
            .unwrap_or_default().into_iter().map(|r| r.name).collect();

        let mut all_skills = Vec::new();
        for src in &sources {
            if !src.enabled { continue; }
            if let Some(ref filter) = p.source {
                let f = filter.to_lowercase();
                if !src.label.to_lowercase().contains(&f) && !src.repo_id().to_lowercase().contains(&f) {
                    continue;
                }
            }
            if let Some(cached) = crate::core::market::load_cache(&data_dir, src) {
                for mut skill in cached {
                    skill.installed = installed.contains(&skill.name);
                    if let Some(ref search) = p.search {
                        if !skill.name.to_lowercase().contains(&search.to_lowercase()) {
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

        Json(TextResult { result: serde_json::to_string_pretty(&all_skills).unwrap_or_default() })
    }

    #[tool(description = "Install a single skill from the market (downloads only SKILL.md)")]
    fn sm_market_install(&self, Parameters(p): Parameters<MarketInstallParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let sources = crate::core::market::load_sources(&data_dir);

        // Find the skill in cache
        let mut found = None;
        for src in &sources {
            if !src.enabled { continue; }
            if let Some(ref filter) = p.source {
                let f = filter.to_lowercase();
                if !src.repo_id().to_lowercase().contains(&f) { continue; }
            }
            if let Some(cached) = crate::core::market::load_cache(&data_dir, src) {
                if let Some(skill) = cached.into_iter().find(|s| s.name == p.name) {
                    found = Some(skill);
                    break;
                }
            }
        }

        let skill = match found {
            Some(s) => s,
            None => return Json(TextResult { result: format!("Skill '{}' not found in market cache", p.name) }),
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = match rt.block_on(crate::core::market::Market::install_single(&skill, mgr.paths())) {
            Ok(_) => {
                let _ = mgr.register_local_skill(&skill.name);
                format!("Installed '{}'", skill.name)
            }
            Err(e) => format!("Install failed: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(description = "Manage market sources. Actions: list, add (repo=owner/repo), remove (repo=owner/repo), enable (repo), disable (repo)")]
    fn sm_sources(&self, Parameters(p): Parameters<MarketSourceParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let mut sources = crate::core::market::load_sources(&data_dir);

        let result = match p.action.as_str() {
            "list" => {
                let items: Vec<serde_json::Value> = sources.iter().map(|s| {
                    serde_json::json!({
                        "label": s.label,
                        "repo": s.repo_id(),
                        "enabled": s.enabled,
                        "builtin": s.builtin,
                    })
                }).collect();
                serde_json::to_string_pretty(&items).unwrap_or_default()
            }
            "add" => {
                match p.repo {
                    Some(repo) => match crate::core::market::SourceEntry::from_input(&repo) {
                        Ok(src) => {
                            sources.push(src);
                            let _ = crate::core::market::save_sources(&data_dir, &sources);
                            format!("Added source: {repo}")
                        }
                        Err(e) => format!("Invalid: {e}"),
                    },
                    None => "Missing 'repo' parameter".into(),
                }
            }
            "remove" => {
                match p.repo {
                    Some(repo) => {
                        let before = sources.len();
                        sources.retain(|s| !s.builtin && s.repo_id() != repo);
                        if sources.len() < before {
                            let _ = crate::core::market::save_sources(&data_dir, &sources);
                            format!("Removed source: {repo}")
                        } else {
                            format!("Source not found or is built-in: {repo}")
                        }
                    }
                    None => "Missing 'repo' parameter".into(),
                }
            }
            "enable" | "disable" => {
                let enable = p.action == "enable";
                match p.repo {
                    Some(repo) => {
                        let mut found = false;
                        for s in &mut sources {
                            if s.repo_id() == repo {
                                s.enabled = enable;
                                found = true;
                            }
                        }
                        if found {
                            let _ = crate::core::market::save_sources(&data_dir, &sources);
                            format!("Source {} {}", repo, if enable { "enabled" } else { "disabled" })
                        } else {
                            format!("Source not found: {repo}")
                        }
                    }
                    None => "Missing 'repo' parameter".into(),
                }
            }
            _ => format!("Unknown action: {}. Use list/add/remove/enable/disable", p.action),
        };
        Json(TextResult { result })
    }

    // ── Utility ──

    #[tool(description = "Register skill-manager as MCP server in all CLI configs")]
    fn sm_register(&self) -> Json<TextResult> {
        let home = dirs::home_dir().unwrap_or_default();
        let r = crate::core::mcp_register::McpRegister::register_all(&home);
        let mut parts = Vec::new();
        for name in &r.registered { parts.push(format!("✓ {name}")); }
        for name in &r.skipped { parts.push(format!("· {name} (already)")); }
        for err in &r.errors { parts.push(format!("⚠ {err}")); }
        Json(TextResult { result: parts.join("\n") })
    }
}

#[tool_handler]
impl ServerHandler for SmServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Skill Manager — manage AI CLI skills, MCPs, groups, and market. \
             Tools: sm_list, sm_groups, sm_status, sm_enable, sm_disable, sm_scan, \
             sm_delete, sm_create_group, sm_delete_group, sm_group_add, sm_group_remove, \
             sm_market, sm_market_install, sm_sources, sm_register".into()
        );
        info.capabilities = rmcp::model::ServerCapabilities::builder()
            .enable_tools()
            .build();
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_router_has_tools() {
        let server = SmServer::new().unwrap();
        let tools = server.tool_router.list_all();
        eprintln!("Registered tools: {}", tools.len());
        for t in &tools {
            eprintln!("  - {}", t.name);
        }
        assert!(tools.len() > 0, "No tools registered in tool_router!");
    }
}

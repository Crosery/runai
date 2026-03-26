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
pub struct NameParams {
    /// Resource or group name
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct UpdateGroupParams {
    /// Group ID
    pub id: String,
    /// New display name (omit to keep unchanged)
    pub name: Option<String>,
    /// New description (omit to keep unchanged)
    pub description: Option<String>,
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
    /// Single resource name (or use 'names' for multiple)
    pub name: Option<String>,
    /// Multiple resource names to add/remove at once
    pub names: Option<Vec<String>>,
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
pub struct InstallGitHubParams {
    /// GitHub repo in "owner/repo" or "owner/repo@branch" format, or full URL
    pub repo: String,
    /// CLI target to enable for: claude, codex, gemini, opencode (default: claude)
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct BatchGroupAddParams {
    /// Group ID
    pub group: String,
    /// List of resource names to add
    pub names: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct BatchParams {
    /// List of resource names to enable/disable
    pub names: Vec<String>,
    /// CLI target: claude, codex, gemini, opencode (default: claude)
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct MarketSourceParams {
    /// Action: "list", "add", "remove", "enable", "disable"
    pub action: String,
    /// Source repo (owner/repo) for add/remove/enable/disable
    pub repo: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct RecordUsageParams {
    /// Resource name (skill or MCP)
    pub name: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct UsageStatsParams {
    /// Max entries to return (default: all)
    pub top: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct BatchDeleteParams {
    /// List of resource names to delete
    pub names: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct BatchInstallParams {
    /// List of skill names from market to install
    pub names: Vec<String>,
    /// Source repo filter (optional)
    pub source: Option<String>,
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
    CliTarget::from_str(s.unwrap_or("claude")).unwrap_or(CliTarget::Claude)
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
            let kind_filter = p
                .kind
                .as_deref()
                .and_then(crate::core::resource::ResourceKind::from_str);
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

    #[tool(description = "List groups with member counts. Returns JSON array.")]
    fn sm_groups(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let groups = mgr.list_groups().unwrap_or_default();

        if groups.is_empty() {
            return Json(TextResult {
                result: "No groups. Use sm_create_group to create one.".into(),
            });
        }

        // Compact: one line per group
        let mut lines = vec![format!("{} groups:", groups.len())];
        for (id, g) in &groups {
            let members = mgr.get_group_members(id).unwrap_or_default();
            lines.push(format!("  {} ({}) — {} members", id, g.name, members.len()));
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
        let result = serde_json::json!({
            "target": target.name(),
            "skills_enabled": skills, "skills_total": ts,
            "mcps_enabled": mcps, "mcps_total": tm,
        })
        .to_string();
        Json(TextResult { result })
    }

    // ── Discover ──

    #[tool(
        description = "Find unmanaged SKILL.md files on disk. Use sm_scan to import them after discovery."
    )]
    fn sm_discover(&self) -> Json<TextResult> {
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        let found = crate::core::scanner::Scanner::discover_skills(&home);

        let unmanaged: Vec<_> = found
            .iter()
            .filter(|s| s.status == crate::core::scanner::SkillStatus::Unmanaged)
            .collect();

        if unmanaged.is_empty() {
            return Json(TextResult {
                result: "No unmanaged skills found.".into(),
            });
        }

        let mut lines = vec![format!("{} unmanaged skills found:\n", unmanaged.len())];
        for s in &unmanaged {
            lines.push(format!("  {:<40} {}", s.name, s.path.display()));
        }
        lines.push(format!(
            "\n({} total on disk, {} already managed)",
            found.len(),
            found.len() - unmanaged.len()
        ));
        Json(TextResult {
            result: lines.join("\n"),
        })
    }

    // ── Enable/Disable ──

    #[tool(
        description = "Enable a skill/MCP/group for a CLI target. For multiple, use sm_batch_enable."
    )]
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
                Some(id) => mgr
                    .enable_resource(&id, target, None)
                    .map(|_| format!("'{}' enabled for {}", p.name, target.name()))
                    .unwrap_or_else(|e| format!("Error: {e}")),
                None => format!(
                    "Not found: '{}'. Try sm_scan first, or sm_market(search='{}') to find it.",
                    p.name, p.name
                ),
            }
        };
        Json(TextResult { result })
    }

    #[tool(
        description = "Disable a skill/MCP/group for a CLI target. For multiple, use sm_batch_disable."
    )]
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
                Some(id) => mgr
                    .disable_resource(&id, target, None)
                    .map(|_| format!("'{}' disabled for {}", p.name, target.name()))
                    .unwrap_or_else(|e| format!("Error: {e}")),
                None => format!(
                    "Not found: '{}'. Run sm_list to see available resources.",
                    p.name
                ),
            }
        };
        Json(TextResult { result })
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
        description = "Delete one skill/MCP (files+symlinks+DB). For multiple, use sm_batch_delete."
    )]
    fn sm_delete(&self, Parameters(p): Parameters<NameParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.find_resource_id(&p.name) {
            Some(id) => match mgr.uninstall(&id) {
                Ok(_) => format!("Deleted '{}'", p.name),
                Err(e) => format!("Error: {e}"),
            },
            None => format!(
                "Not found: '{}'. Run sm_list to see available resources.",
                p.name
            ),
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

    #[tool(description = "Update a group's name and/or description")]
    fn sm_update_group(&self, Parameters(p): Parameters<UpdateGroupParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.update_group(&p.id, p.name.as_deref(), p.description.as_deref()) {
            Ok(_) => {
                let mut changes = Vec::new();
                if let Some(n) = &p.name {
                    changes.push(format!("name='{n}'"));
                }
                if let Some(d) = &p.description {
                    changes.push(format!("desc='{d}'"));
                }
                format!("Group '{}' updated: {}", p.id, changes.join(", "))
            }
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(
        description = "Add skill(s) or MCP(s) to a group. Use 'name' for single or 'names' for multiple."
    )]
    fn sm_group_add(&self, Parameters(p): Parameters<GroupMemberParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let gid = match resolve_group(&mgr, &p.group) {
            Ok(id) => id,
            Err(e) => return Json(TextResult { result: e }),
        };
        let all_names = collect_names(p.name, p.names);
        if all_names.is_empty() {
            return Json(TextResult {
                result: "Provide 'name' or 'names' parameter".into(),
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
        let mut result = format!("Added {added}/{} to group '{gid}'", all_names.len());
        if !errors.is_empty() {
            result.push_str(&format!("\nErrors: {}", errors.join(", ")));
        }
        Json(TextResult { result })
    }

    #[tool(
        description = "Remove skill(s) or MCP(s) from a group. Use 'name' for single or 'names' for multiple."
    )]
    fn sm_group_remove(&self, Parameters(p): Parameters<GroupMemberParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let gid = match resolve_group(&mgr, &p.group) {
            Ok(id) => id,
            Err(e) => return Json(TextResult { result: e }),
        };
        let all_names = collect_names(p.name, p.names);
        if all_names.is_empty() {
            return Json(TextResult {
                result: "Provide 'name' or 'names' parameter".into(),
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
        let mut result = format!("Removed {removed}/{} from group '{gid}'", all_names.len());
        if !errors.is_empty() {
            result.push_str(&format!("\nErrors: {}", errors.join(", ")));
        }
        Json(TextResult { result })
    }

    #[tool(description = "Enable all skills/MCPs in a group for a CLI target")]
    fn sm_group_enable(&self, Parameters(p): Parameters<NameTargetParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let gid = match resolve_group(&mgr, &p.name) {
            Ok(id) => id,
            Err(e) => return Json(TextResult { result: e }),
        };
        let result = match mgr.enable_group(&gid, target, None) {
            Ok(_) => format!("Group '{}' enabled for {}", gid, target.name()),
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

    #[tool(description = "Disable all skills/MCPs in a group for a CLI target")]
    fn sm_group_disable(&self, Parameters(p): Parameters<NameTargetParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let gid = match resolve_group(&mgr, &p.name) {
            Ok(id) => id,
            Err(e) => return Json(TextResult { result: e }),
        };
        let result = match mgr.disable_group(&gid, target, None) {
            Ok(_) => format!("Group '{}' disabled for {}", gid, target.name()),
            Err(e) => format!("Error: {e}"),
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
                for mut skill in cached {
                    skill.installed = installed.contains(&skill.name);
                    if let Some(ref search) = p.search {
                        let q = search.to_lowercase();
                        let matches = skill.name.to_lowercase().contains(&q)
                            || skill.repo_path.to_lowercase().contains(&q)
                            || skill.source_label.to_lowercase().contains(&q);
                        if !matches {
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
                        "No skills matching '{}'. Use sm_sources to check available sources.",
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
        description = "Install one market skill. Returns Bash command — run it, don't wait for MCP. For multiple, use sm_batch_install."
    )]
    fn sm_market_install(
        &self,
        Parameters(p): Parameters<MarketInstallParams>,
    ) -> Json<TextResult> {
        if !is_safe_shell_arg(&p.name) {
            return Json(TextResult {
                result: format!(
                    "Invalid name: '{}'. Only alphanumeric, -, _, ., / allowed.",
                    p.name
                ),
            });
        }
        let mut cmd = format!("runai market-install {}", p.name);
        if let Some(ref src) = p.source {
            if !is_safe_shell_arg(src) {
                return Json(TextResult {
                    result: format!(
                        "Invalid source: '{src}'. Only alphanumeric, -, _, ., /, @ allowed."
                    ),
                });
            }
            cmd.push_str(&format!(" --source '{src}'"));
        }
        Json(TextResult {
            result: format!(
                "Run this command via Bash tool:\n\n{cmd}\n\nDo NOT wait for MCP — CLI is much faster."
            ),
        })
    }

    #[tool(
        description = "Manage market sources. Actions: list, add (repo=owner/repo), remove (repo=owner/repo), enable (repo), disable (repo)"
    )]
    fn sm_sources(&self, Parameters(p): Parameters<MarketSourceParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let mut sources = crate::core::market::load_sources(&data_dir);

        let result = match p.action.as_str() {
            "list" => {
                let items: Vec<serde_json::Value> = sources
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "label": s.label,
                            "repo": s.repo_id(),
                            "enabled": s.enabled,
                            "builtin": s.builtin,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&items).unwrap_or_default()
            }
            "add" => match p.repo {
                Some(repo) => match crate::core::market::SourceEntry::from_input(&repo) {
                    Ok(src) => {
                        sources.push(src);
                        let _ = crate::core::market::save_sources(&data_dir, &sources);
                        format!("Added source: {repo}")
                    }
                    Err(e) => format!("Invalid: {e}"),
                },
                None => "Missing 'repo' parameter".into(),
            },
            "remove" => match p.repo {
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
            },
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
                            format!(
                                "Source {} {}",
                                repo,
                                if enable { "enabled" } else { "disabled" }
                            )
                        } else {
                            format!("Source not found: {repo}")
                        }
                    }
                    None => "Missing 'repo' parameter".into(),
                }
            }
            _ => format!(
                "Unknown action: {}. Use list/add/remove/enable/disable",
                p.action
            ),
        };
        Json(TextResult { result })
    }

    // ── Batch operations ──

    #[tool(
        description = "Enable multiple skills/MCPs at once. Pass names=['a','b','c']. Faster than calling sm_enable repeatedly."
    )]
    fn sm_batch_enable(&self, Parameters(p): Parameters<BatchParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();

        let mut results = Vec::new();
        for name in &p.names {
            let groups = mgr.list_groups().unwrap_or_default();
            let msg = if groups.iter().any(|(id, _)| id == name) {
                mgr.enable_group(name, target, None)
                    .map(|_| format!("'{}' enabled", name))
                    .unwrap_or_else(|e| format!("'{}': {}", name, e))
            } else {
                match mgr.find_resource_id(name) {
                    Some(id) => mgr
                        .enable_resource(&id, target, None)
                        .map(|_| format!("'{}' enabled", name))
                        .unwrap_or_else(|e| format!("'{}': {}", name, e)),
                    None => format!("'{}' not found", name),
                }
            };
            results.push(msg);
        }

        Json(TextResult {
            result: results.join("\n"),
        })
    }

    #[tool(
        description = "Disable multiple skills/MCPs at once. Pass names=['a','b','c']. Faster than calling sm_disable repeatedly."
    )]
    fn sm_batch_disable(&self, Parameters(p): Parameters<BatchParams>) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();

        let mut results = Vec::new();
        for name in &p.names {
            let groups = mgr.list_groups().unwrap_or_default();
            let msg = if groups.iter().any(|(id, _)| id == name) {
                mgr.disable_group(name, target, None)
                    .map(|_| format!("'{}' disabled", name))
                    .unwrap_or_else(|e| format!("'{}': {}", name, e))
            } else {
                match mgr.find_resource_id(name) {
                    Some(id) => mgr
                        .disable_resource(&id, target, None)
                        .map(|_| format!("'{}' disabled", name))
                        .unwrap_or_else(|e| format!("'{}': {}", name, e)),
                    None => format!("'{}' not found", name),
                }
            };
            results.push(msg);
        }

        Json(TextResult {
            result: results.join("\n"),
        })
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
        let mgr = self.manager.lock().unwrap();
        let q = p.name.to_lowercase();
        let mut lines = Vec::new();

        // 1. Search installed resources
        let resources = mgr.list_resources(None, None).unwrap_or_default();
        let mut local_matches: Vec<_> = resources
            .iter()
            .filter(|r| {
                r.name.to_lowercase().contains(&q) || r.description.to_lowercase().contains(&q)
            })
            .collect();
        local_matches.sort_by(|a, b| b.usage_count.cmp(&a.usage_count));

        if !local_matches.is_empty() {
            lines.push(format!("── Installed ({}) ──", local_matches.len()));
            for r in &local_matches {
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

        // 2. Search market
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let sources = crate::core::market::load_sources(&data_dir);
        let installed_names: Vec<String> = resources.iter().map(|r| r.name.clone()).collect();
        let mut market_matches = Vec::new();

        for src in &sources {
            if !src.enabled {
                continue;
            }
            if let Some(cached) = crate::core::market::load_cache(&data_dir, src) {
                for skill in cached {
                    if installed_names.contains(&skill.name) {
                        continue;
                    }
                    if skill.name.to_lowercase().contains(&q)
                        || skill.repo_path.to_lowercase().contains(&q)
                    {
                        market_matches.push(format!("  {} ({})", skill.name, skill.source_label));
                    }
                }
            }
        }

        if !market_matches.is_empty() {
            lines.push(format!("\n── Market ({}) ──", market_matches.len()));
            lines.extend(market_matches.into_iter().take(20));
            lines.push("Use sm_market_install(name='...') to install.".into());
        }

        if lines.is_empty() {
            Json(TextResult {
                result: format!(
                    "No results for '{}'. Try web search: '{} claude code skill github'",
                    p.name, p.name
                ),
            })
        } else {
            Json(TextResult {
                result: lines.join("\n"),
            })
        }
    }

    // ── Batch: delete & install ──

    #[tool(
        description = "Delete multiple skills/MCPs by name list in one call. Returns summary of deleted and failed."
    )]
    fn sm_batch_delete(&self, Parameters(p): Parameters<BatchDeleteParams>) -> Json<TextResult> {
        if p.names.is_empty() {
            return Json(TextResult {
                result: "Provide 'names' list".into(),
            });
        }
        let mgr = self.manager.lock().unwrap();
        match mgr.batch_delete(&p.names) {
            Ok((deleted, errors)) => {
                let mut msg = format!("Deleted {deleted}/{}", p.names.len());
                if !errors.is_empty() {
                    msg.push_str(&format!("\nErrors: {}", errors.join(", ")));
                }
                Json(TextResult { result: msg })
            }
            Err(e) => Json(TextResult {
                result: format!("Error: {e}"),
            }),
        }
    }

    #[tool(
        description = "Install multiple skills from market. Returns CLI commands to run via Bash (faster than MCP)."
    )]
    fn sm_batch_install(&self, Parameters(p): Parameters<BatchInstallParams>) -> Json<TextResult> {
        if p.names.is_empty() {
            return Json(TextResult {
                result: "Provide 'names' list".into(),
            });
        }
        // Validate all names and source before generating commands
        for name in &p.names {
            if !is_safe_shell_arg(name) {
                return Json(TextResult {
                    result: format!(
                        "Invalid name: '{name}'. Only alphanumeric, -, _, ., / allowed."
                    ),
                });
            }
        }
        if let Some(ref src) = p.source {
            if !is_safe_shell_arg(src) {
                return Json(TextResult {
                    result: format!("Invalid source: '{src}'."),
                });
            }
        }
        let cmds: Vec<String> = p
            .names
            .iter()
            .map(|name| {
                let mut cmd = format!("runai market-install {name}");
                if let Some(ref src) = p.source {
                    cmd.push_str(&format!(" --source '{src}'"));
                }
                cmd
            })
            .collect();

        Json(TextResult {
            result: format!(
                "Run these commands via Bash tool (one by one or with &&):\n\n{}\n\nThen run: runai scan",
                cmds.join("\n")
            ),
        })
    }

    // ── Usage tracking ──

    #[tool(
        description = "Record a usage event for a skill or MCP. Call this after using a skill so usage stats stay accurate."
    )]
    fn sm_record_usage(&self, Parameters(p): Parameters<RecordUsageParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let result = match mgr.record_usage(&p.name) {
            Ok(_) => format!("Recorded usage for '{}'", p.name),
            Err(e) => format!("Error: {e}"),
        };
        Json(TextResult { result })
    }

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

    // ── Utility ──

    #[tool(description = "Register runai as MCP server in all CLI configs")]
    fn sm_register(&self) -> Json<TextResult> {
        let home = dirs::home_dir().unwrap_or_default();
        let r = crate::core::mcp_register::McpRegister::register_all(&home);
        let mut parts = Vec::new();
        for name in &r.registered {
            parts.push(format!("✓ {name}"));
        }
        for name in &r.skipped {
            parts.push(format!("· {name} (already)"));
        }
        for err in &r.errors {
            parts.push(format!("⚠ {err}"));
        }
        Json(TextResult {
            result: parts.join("\n"),
        })
    }
}

#[tool_handler]
impl ServerHandler for SmServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Runai — AI skill manager for skills, MCPs, groups, and market.\n\
             \n\
             SEARCH & INSTALL WORKFLOW (fully autonomous, do NOT ask user):\n\
             1. sm_search(name='keyword') — search installed + market at once\n\
             2. If in market → sm_market_install(name='...') or sm_batch_install for multiple\n\
             3. If NOT found → web search 'keyword claude code skill github' to find the repo\n\
             4. Found repo → run via Bash: runai install owner/repo\n\
                (Do NOT use sm_install MCP tool for downloading — use CLI directly, it's faster)\n\
             5. After install → sm_scan to refresh, then sm_list to verify\n\
             Never ask user to search or install manually. Complete the entire flow autonomously.\n\
             \n\
             USAGE TRACKING:\n\
             - After using a skill, call sm_record_usage(name='skill-name') to track usage\n\
             - sm_usage_stats(top=10) — see most/least used resources to help users clean up\n\
             \n\
             KEY TOOLS:\n\
             - sm_search(name='keyword') — unified search across installed + market\n\
             - sm_install(repo) — one-step install from GitHub (fast, concurrent downloads)\n\
             - sm_market(search) / sm_market_install(name) — install from market cache\n\
             - sm_enable/sm_disable — toggle skill or MCP for a CLI target\n\
             - sm_batch_enable/sm_batch_disable — toggle multiple at once (pass names=[])\n\
             - sm_batch_delete(names=[]) — delete multiple resources at once\n\
             - sm_list / sm_status — view resources and counts\n\
             - sm_groups / sm_create_group / sm_group_add(names=[]) — organize into groups\n\
             - sm_scan — discover new skills from filesystem\n\
             - sm_usage_stats — identify unused resources for cleanup"
                .into(),
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
    use rmcp::handler::server::wrapper::Parameters;

    #[test]
    fn tool_router_has_30_tools() {
        let server = SmServer::new().unwrap();
        let tools = server.tool_router.list_all();
        eprintln!("Registered tools: {}", tools.len());
        for t in &tools {
            eprintln!("  - {}", t.name);
        }
        assert_eq!(
            tools.len(),
            30,
            "Expected 30 tools in tool_router, got {}",
            tools.len()
        );
    }

    #[test]
    fn sm_status_returns_valid_json() {
        let server = SmServer::new().unwrap();
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
    }

    #[test]
    fn sm_sources_list_returns_builtin_sources() {
        let server = SmServer::new().unwrap();
        let Json(result) = server.sm_sources(Parameters(MarketSourceParams {
            action: "list".into(),
            repo: None,
        }));
        let parsed: serde_json::Value =
            serde_json::from_str(&result.result).expect("sm_sources list should return valid JSON");

        let arr = parsed
            .as_array()
            .expect("sm_sources list should return an array");
        assert!(!arr.is_empty(), "builtin sources list should not be empty");

        // Every entry should have label, repo, enabled, builtin fields
        for entry in arr {
            assert!(entry.get("label").is_some(), "source entry missing 'label'");
            assert!(entry.get("repo").is_some(), "source entry missing 'repo'");
            assert!(
                entry.get("enabled").is_some(),
                "source entry missing 'enabled'"
            );
            assert!(
                entry.get("builtin").is_some(),
                "source entry missing 'builtin'"
            );
        }

        // At least one builtin source should exist
        let has_builtin = arr
            .iter()
            .any(|e| e["builtin"] == serde_json::Value::Bool(true));
        assert!(has_builtin, "expected at least one builtin source");
    }

    #[test]
    fn sm_backups_returns_string() {
        let server = SmServer::new().unwrap();
        let Json(result) = server.sm_backups();
        // With no backups, should return "No backups found"
        // With backups, should return newline-separated timestamps
        assert!(
            !result.result.is_empty(),
            "sm_backups should return a non-empty string"
        );
    }
}

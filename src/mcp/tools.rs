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
pub struct RestoreParams {
    /// Backup timestamp (omit to use latest)
    pub timestamp: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct DaziInstallParams {
    /// Skill or agent name to install
    pub name: String,
    /// 'skill' (default) or 'agent'
    pub kind: Option<String>,
    /// CLI target: claude, codex, gemini, opencode (default: claude)
    pub target: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct DaziListParams {
    /// Filter: 'all' (default), 'skills', 'agents', 'bundles'
    pub kind: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct DaziStatsParams {
    /// Filter: 'all' (default), 'skills', 'agents'
    pub kind: Option<String>,
    /// Max items to show (default: 10)
    pub top: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct DaziPublishParams {
    /// Skill name to publish (must be installed locally)
    pub name: String,
    /// Short description (auto-extracted from SKILL.md if omitted)
    pub description: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct DaziLoginParams {
    /// Session token. Omit to get guided instructions for obtaining one.
    pub session_token: Option<String>,
    /// Team ID (auto-detected if you have exactly one team)
    pub team_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct DaziPublishBundleParams {
    /// Agent IDs to include (get from sm_dazi_publishable)
    #[serde(default)]
    pub agent_ids: Vec<String>,
    /// Skill names to include
    #[serde(default)]
    pub skill_names: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
pub struct DaziPublishAgentParams {
    /// Agent name
    pub name: String,
    /// Display title (e.g. "性能测试专家")
    pub title: String,
    /// Short description
    pub description: String,
    /// Role identifier (e.g. "perf_engineer")
    pub role: String,
    /// Full prompt template content
    pub prompt_template: String,
    /// Tags for categorization
    #[serde(default)]
    pub tags: Vec<String>,
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

/// Open a URL in the user's default browser.
fn open_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", url])
        .spawn();
}

const LOGIN_PORT: u16 = 19836;

/// Start a local HTTP server, open browser to dazi, wait for token callback.
/// User logs in to dazi, then runs a one-liner in browser console that POSTs
/// the session token to our local server. Returns the token.
fn wait_for_dazi_token() -> anyhow::Result<String> {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    let listener = TcpListener::bind(format!("127.0.0.1:{LOGIN_PORT}"))
        .map_err(|e| anyhow::anyhow!("Failed to start local server on port {LOGIN_PORT}: {e}"))?;
    listener.set_nonblocking(true)?;

    // Open browser to dazi
    open_browser("http://dazi.ktvsky.com/app");

    let start = Instant::now();
    let timeout = Duration::from_secs(300); // 5 minutes

    loop {
        if start.elapsed() > timeout {
            anyhow::bail!(
                "Timed out waiting for login (5 min). Try again or provide session_token directly."
            );
        }

        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;

                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]).to_string();

                // Handle CORS preflight
                if request.starts_with("OPTIONS") {
                    let cors_response = "HTTP/1.1 204 No Content\r\n\
                        Access-Control-Allow-Origin: *\r\n\
                        Access-Control-Allow-Methods: POST, OPTIONS\r\n\
                        Access-Control-Allow-Headers: Content-Type\r\n\
                        \r\n";
                    let _ = stream.write_all(cors_response.as_bytes());
                    continue;
                }

                // Handle POST with token
                if request.starts_with("POST") {
                    // Extract JSON body after \r\n\r\n
                    if let Some(body_start) = request.find("\r\n\r\n") {
                        let body = &request[body_start + 4..];
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
                            if let Some(token) = json.get("token").and_then(|t| t.as_str()) {
                                // Send success response
                                let success_html = "<!DOCTYPE html><html><body style='font-family:sans-serif;text-align:center;padding:60px'>\
                                    <h2 style='color:#22c55e'>Login successful!</h2>\
                                    <p>You can close this tab.</p>\
                                    <script>setTimeout(()=>window.close(),2000)</script>\
                                    </body></html>";
                                let response = format!(
                                    "HTTP/1.1 200 OK\r\n\
                                    Content-Type: text/html; charset=utf-8\r\n\
                                    Access-Control-Allow-Origin: *\r\n\
                                    Content-Length: {}\r\n\
                                    Connection: close\r\n\
                                    \r\n{}",
                                    success_html.len(),
                                    success_html,
                                );
                                let _ = stream.write_all(response.as_bytes());
                                return Ok(token.to_string());
                            }
                        }
                    }

                    // Bad request
                    let err = "HTTP/1.1 400 Bad Request\r\n\
                        Access-Control-Allow-Origin: *\r\n\
                        Content-Length: 0\r\n\r\n";
                    let _ = stream.write_all(err.as_bytes());
                    continue;
                }

                // GET request — serve the guide page
                let guide_html = format!(
                    r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Runai - 搭子 Login</title></head>
<body style="font-family:system-ui,-apple-system,sans-serif;max-width:600px;margin:60px auto;padding:0 20px;color:#333">
<h2>Runai x 搭子 Login</h2>
<div id="status">
<p><b>Step 1:</b> Go to <a href="http://dazi.ktvsky.com/app" target="_blank">dazi.ktvsky.com</a> and login with 飞书</p>
<p><b>Step 2:</b> After login, press F12 to open console, paste this and press Enter:</p>
<pre style="background:#1a1a2e;color:#0f0;padding:12px;border-radius:8px;overflow-x:auto;font-size:13px;cursor:pointer" onclick="navigator.clipboard.writeText(this.textContent)" title="Click to copy">fetch('/api/auth/get-session').then(r=>r.json()).then(d=>fetch('http://127.0.0.1:{LOGIN_PORT}',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{token:d.session.token}})}})).then(()=>document.title='Done!')</pre>
<p style="color:#888;font-size:13px">Click the code block above to copy it.</p>
</div>
</body></html>"#
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\n\
                    Content-Type: text/html; charset=utf-8\r\n\
                    Content-Length: {}\r\n\
                    Connection: close\r\n\
                    \r\n{}",
                    guide_html.len(),
                    guide_html,
                );
                let _ = stream.write_all(response.as_bytes());
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                anyhow::bail!("Server error: {e}");
            }
        }
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
        description = "Delete skill(s)/MCP(s) (files+symlinks+DB). Pass 'name' for single or 'names' for multiple."
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
                Some(id) => match mgr.uninstall(&id) {
                    Ok(_) => format!("Deleted '{name}'"),
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
        if let Some(ref src) = p.source {
            if !is_safe_shell_arg(src) {
                return Json(TextResult {
                    result: format!(
                        "Invalid source: '{src}'. Only alphanumeric, -, _, ., /, @ allowed."
                    ),
                });
            }
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

    // ── Dazi marketplace ──

    #[tool(
        description = "Search 搭子(dazi) marketplace for skills, agents, and bundles. Returns matching items with download counts."
    )]
    fn sm_dazi_search(&self, Parameters(p): Parameters<NameParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let q = p.name.to_lowercase();

        let installed: Vec<String> = mgr
            .list_resources(None, None)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.name)
            .collect();

        let mut lines = Vec::new();

        // Search skills
        if let Some(skills) = crate::core::dazi::load_cache_skills(&data_dir) {
            let matches: Vec<_> = skills
                .iter()
                .filter(|s| {
                    s.name.to_lowercase().contains(&q)
                        || s.description.to_lowercase().contains(&q)
                        || s.tags.iter().any(|t| t.to_lowercase().contains(&q))
                })
                .collect();
            if !matches.is_empty() {
                lines.push(format!("── Skills ({}) ──", matches.len()));
                for s in matches.iter().take(20) {
                    let icon = if installed.contains(&s.name) {
                        "✓"
                    } else {
                        " "
                    };
                    let dl = if s.download_count > 0 {
                        format!(" ↓{}", s.download_count)
                    } else {
                        String::new()
                    };
                    lines.push(format!("  {icon} {}{dl}", s.name));
                }
            }
        }

        // Search agents
        if let Some(agents) = crate::core::dazi::load_cache_agents(&data_dir) {
            let matches: Vec<_> = agents
                .iter()
                .filter(|a| {
                    a.name.to_lowercase().contains(&q)
                        || a.title.to_lowercase().contains(&q)
                        || a.description.to_lowercase().contains(&q)
                        || a.tags.iter().any(|t| t.to_lowercase().contains(&q))
                })
                .collect();
            if !matches.is_empty() {
                lines.push(format!("\n── Agents ({}) ──", matches.len()));
                for a in matches.iter().take(20) {
                    let icon = if installed.contains(&a.name) {
                        "✓"
                    } else {
                        " "
                    };
                    let title = if a.title.is_empty() { "" } else { &a.title };
                    let dl = if a.download_count > 0 {
                        format!(" ↓{}", a.download_count)
                    } else {
                        String::new()
                    };
                    lines.push(format!("  {icon} {} {title}{dl}", a.name));
                }
            }
        }

        // Search bundles
        if let Some(bundles) = crate::core::dazi::load_cache_bundles(&data_dir) {
            let matches: Vec<_> = bundles
                .iter()
                .filter(|b| {
                    b.name.to_lowercase().contains(&q)
                        || b.source_team_name.to_lowercase().contains(&q)
                        || b.description.to_lowercase().contains(&q)
                })
                .collect();
            if !matches.is_empty() {
                lines.push(format!("\n── Bundles ({}) ──", matches.len()));
                for b in &matches {
                    let display = if b.source_team_name.is_empty() {
                        &b.name
                    } else {
                        &b.source_team_name
                    };
                    lines.push(format!(
                        "  📦 {} [{}A+{}S]",
                        display,
                        b.agent_refs.len(),
                        b.skill_refs.len()
                    ));
                }
            }
        }

        if lines.is_empty() {
            Json(TextResult {
                result: format!("No results for '{}' in 搭子 marketplace.", p.name),
            })
        } else {
            lines
                .push("\nUse sm_dazi_install(name='...', kind='skill'|'agent') to install.".into());
            Json(TextResult {
                result: lines.join("\n"),
            })
        }
    }

    #[tool(
        description = "Install a skill or agent from 搭子(dazi) marketplace. kind: 'skill' (default) or 'agent'. For bundles use sm_dazi_install_bundle."
    )]
    fn sm_dazi_install(&self, Parameters(p): Parameters<DaziInstallParams>) -> Json<TextResult> {
        if !is_safe_shell_arg(&p.name) {
            return Json(TextResult {
                result: format!("Invalid name: '{}'", p.name),
            });
        }
        let kind = p.kind.as_deref().unwrap_or("skill");
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let paths = mgr.paths().clone();
        drop(mgr);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::core::dazi::DaziClient::new();

        let result = match kind {
            "agent" => rt.block_on(client.install_agent(&p.name, &paths)),
            _ => rt.block_on(client.install_skill(&p.name, &paths)),
        };

        match result {
            Ok(name) => {
                let mgr = self.manager.lock().unwrap();
                let _ = mgr.register_local_skill(&name);
                if let Some(id) = mgr.find_resource_id(&name) {
                    let _ = mgr.enable_resource(&id, target, None);
                }
                Json(TextResult {
                    result: format!("Installed '{name}' from 搭子 as {kind}"),
                })
            }
            Err(e) => Json(TextResult {
                result: format!("Install failed: {e}"),
            }),
        }
    }

    #[tool(
        description = "Install a bundle (组合包) from 搭子 marketplace. Installs all skills and agents in the bundle."
    )]
    fn sm_dazi_install_bundle(
        &self,
        Parameters(p): Parameters<NameTargetParams>,
    ) -> Json<TextResult> {
        let target = parse_target(p.target.as_deref());
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        let paths = mgr.paths().clone();
        drop(mgr);

        let bundles = crate::core::dazi::load_cache_bundles(&data_dir).unwrap_or_default();
        let bundle = bundles
            .iter()
            .find(|b| b.name == p.name || b.source_team_name == p.name);

        let bundle = match bundle {
            Some(b) => b.clone(),
            None => {
                return Json(TextResult {
                    result: format!(
                        "Bundle '{}' not found. Use sm_dazi_search to find bundles.",
                        p.name
                    ),
                });
            }
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::core::dazi::DaziClient::new();
        match rt.block_on(client.install_bundle(&bundle, &paths)) {
            Ok(names) => {
                let mgr = self.manager.lock().unwrap();
                for name in &names {
                    let _ = mgr.register_local_skill(name);
                    if let Some(id) = mgr.find_resource_id(name) {
                        let _ = mgr.enable_resource(&id, target, None);
                    }
                }
                Json(TextResult {
                    result: format!(
                        "Installed bundle '{}': {} items ({})",
                        p.name,
                        names.len(),
                        names.join(", ")
                    ),
                })
            }
            Err(e) => Json(TextResult {
                result: format!("Bundle install failed: {e}"),
            }),
        }
    }

    #[tool(
        description = "List all skills, agents, and bundles available on 搭子(dazi) marketplace."
    )]
    fn sm_dazi_list(&self, Parameters(p): Parameters<DaziListParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();

        let installed: Vec<String> = mgr
            .list_resources(None, None)
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.name)
            .collect();

        let kind = p.kind.as_deref().unwrap_or("all");
        let mut lines = Vec::new();

        if kind == "all" || kind == "skill" || kind == "skills" {
            if let Some(skills) = crate::core::dazi::load_cache_skills(&data_dir) {
                lines.push(format!("── Skills ({}) ──", skills.len()));
                for s in &skills {
                    let icon = if installed.contains(&s.name) {
                        "✓"
                    } else {
                        " "
                    };
                    lines.push(format!("  {icon} {}", s.name));
                }
            }
        }

        if kind == "all" || kind == "agent" || kind == "agents" {
            if let Some(agents) = crate::core::dazi::load_cache_agents(&data_dir) {
                lines.push(format!("\n── Agents ({}) ──", agents.len()));
                for a in &agents {
                    let icon = if installed.contains(&a.name) {
                        "✓"
                    } else {
                        " "
                    };
                    let title = if a.title.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", a.title)
                    };
                    lines.push(format!("  {icon} {}{title}", a.name));
                }
            }
        }

        if kind == "all" || kind == "bundle" || kind == "bundles" {
            if let Some(bundles) = crate::core::dazi::load_cache_bundles(&data_dir) {
                lines.push(format!("\n── Bundles ({}) ──", bundles.len()));
                for b in &bundles {
                    let display = if b.source_team_name.is_empty() {
                        &b.name
                    } else {
                        &b.source_team_name
                    };
                    lines.push(format!(
                        "  📦 {} [{}A+{}S]",
                        display,
                        b.agent_refs.len(),
                        b.skill_refs.len()
                    ));
                }
            }
        }

        if lines.is_empty() {
            Json(TextResult {
                result: "No cached data. Dazi data loads in TUI on startup, or wait for background refresh.".into(),
            })
        } else {
            Json(TextResult {
                result: lines.join("\n"),
            })
        }
    }

    #[tool(
        description = "Show 搭子 marketplace hot rankings by download count. kind: 'all'(default), 'skills', 'agents'. top: max items (default 10)."
    )]
    fn sm_dazi_stats(&self, Parameters(p): Parameters<DaziStatsParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        drop(mgr);

        let kind = p.kind.as_deref().unwrap_or("all");
        let top = p.top.unwrap_or(10);
        let mut lines = Vec::new();

        if kind == "all" || kind == "skills" {
            if let Some(mut skills) = crate::core::dazi::load_cache_skills(&data_dir) {
                skills.sort_by(|a, b| b.download_count.cmp(&a.download_count));
                lines.push(format!("── Skills Hot ──"));
                for s in skills.iter().take(top) {
                    let official = if s.is_official { " ★" } else { "" };
                    lines.push(format!("  {:>4}↓ {}{official}", s.download_count, s.name));
                }
            }
        }

        if kind == "all" || kind == "agents" {
            if let Some(mut agents) = crate::core::dazi::load_cache_agents(&data_dir) {
                agents.sort_by(|a, b| b.download_count.cmp(&a.download_count));
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(format!("── Agents Hot ──"));
                for a in agents.iter().take(top) {
                    let title = if a.title.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", a.title)
                    };
                    lines.push(format!("  {:>4}↓ {}{title}", a.download_count, a.name));
                }
            }
        }

        if lines.is_empty() {
            Json(TextResult {
                result: "No cached data. Run sm_dazi_refresh first.".into(),
            })
        } else {
            Json(TextResult {
                result: lines.join("\n"),
            })
        }
    }

    #[tool(
        description = "Publish a local skill to 搭子 marketplace. Reads SKILL.md from the skill directory and publishes it."
    )]
    fn sm_dazi_publish(&self, Parameters(p): Parameters<DaziPublishParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let skill_dir = mgr.paths().skills_dir().join(&p.name);
        drop(mgr);

        let skill_md = skill_dir.join("SKILL.md");
        if !skill_md.exists() {
            return Json(TextResult {
                result: format!(
                    "Skill '{}' not found at {}. Make sure it's installed locally first.",
                    p.name,
                    skill_md.display()
                ),
            });
        }

        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(e) => {
                return Json(TextResult {
                    result: format!("Failed to read SKILL.md: {e}"),
                });
            }
        };

        let description = p.description.as_deref().unwrap_or_else(|| {
            // Extract first non-empty, non-heading line as description
            ""
        });
        let description = if description.is_empty() {
            crate::core::scanner::Scanner::extract_description(&skill_dir)
        } else {
            description.to_string()
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::core::dazi::DaziClient::new();
        match rt.block_on(client.publish_skill(&p.name, &content, &description)) {
            Ok(result) => Json(TextResult {
                result: format!(
                    "Published '{}' to 搭子 marketplace (v{})",
                    result.name, result.version
                ),
            }),
            Err(e) => Json(TextResult {
                result: format!("Publish failed: {e}"),
            }),
        }
    }

    #[tool(
        description = "Publish an agent definition to 搭子 marketplace. Requires name, title, description, role, and prompt_template."
    )]
    fn sm_dazi_publish_agent(
        &self,
        Parameters(p): Parameters<DaziPublishAgentParams>,
    ) -> Json<TextResult> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::core::dazi::DaziClient::new();
        match rt.block_on(client.publish_agent(
            &p.name,
            &p.title,
            &p.description,
            &p.role,
            &p.prompt_template,
            &p.tags,
        )) {
            Ok(result) => Json(TextResult {
                result: format!(
                    "Published agent '{}' to 搭子 marketplace (v{})",
                    result.name, result.version
                ),
            }),
            Err(e) => Json(TextResult {
                result: format!("Publish agent failed: {e}"),
            }),
        }
    }

    #[tool(
        description = "Refresh 搭子(dazi) marketplace cache and MCP token. Fetches latest skills, agents, bundles."
    )]
    fn sm_dazi_refresh(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        drop(mgr);

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::core::dazi::DaziClient::new();

        let mut parts = Vec::new();

        match rt.block_on(client.fetch_skills()) {
            Ok(skills) => {
                let n = skills.len();
                let _ = crate::core::dazi::save_cache_skills(&data_dir, &skills);
                parts.push(format!("✓ Skills: {n}"));
            }
            Err(e) => parts.push(format!("⚠ Skills: {e}")),
        }

        match rt.block_on(client.fetch_agents()) {
            Ok(agents) => {
                let n = agents.len();
                let _ = crate::core::dazi::save_cache_agents(&data_dir, &agents);
                parts.push(format!("✓ Agents: {n}"));
            }
            Err(e) => parts.push(format!("⚠ Agents: {e}")),
        }

        match rt.block_on(client.fetch_bundles()) {
            Ok(bundles) => {
                let n = bundles.len();
                let _ = crate::core::dazi::save_cache_bundles(&data_dir, &bundles);
                parts.push(format!("✓ Bundles: {n}"));
            }
            Err(e) => parts.push(format!("⚠ Bundles: {e}")),
        }

        // Refresh MCP token
        match rt.block_on(crate::core::dazi::refresh_token_if_needed(&data_dir)) {
            Ok(true) => parts.push("✓ MCP token refreshed".into()),
            Ok(false) => parts.push("· MCP token still valid".into()),
            Err(e) => parts.push(format!("⚠ Token: {e}")),
        }

        Json(TextResult {
            result: parts.join("\n"),
        })
    }

    #[tool(
        description = "Login to 搭子. Without session_token: opens browser, starts local server to receive token automatically. With session_token: saves directly."
    )]
    fn sm_dazi_login(&self, Parameters(p): Parameters<DaziLoginParams>) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        drop(mgr);

        // If no token provided, start local server + open browser
        let session_token = match p.session_token {
            Some(t) if !t.is_empty() => t,
            _ => match wait_for_dazi_token() {
                Ok(token) => token,
                Err(e) => {
                    return Json(TextResult {
                        result: format!("Login flow failed: {e}"),
                    });
                }
            },
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::core::dazi::DaziClient::new();

        // Verify the token
        let session_info = match rt.block_on(client.verify_session(&session_token)) {
            Ok(info) => info,
            Err(e) => {
                return Json(TextResult {
                    result: format!("Login failed: {e}"),
                });
            }
        };

        // If no team_id provided, list teams and use the first one
        let team_id = if let Some(tid) = p.team_id {
            tid
        } else {
            match rt.block_on(client.list_teams(&session_token)) {
                Ok(teams) => {
                    if teams.is_empty() {
                        return Json(TextResult {
                            result: "Login OK but no teams found. Create a team on dazi.ktvsky.com first.".into(),
                        });
                    }
                    if teams.len() > 1 {
                        let list: Vec<String> = teams
                            .iter()
                            .map(|t| format!("  {} ({})", t.name, t.id))
                            .collect();
                        return Json(TextResult {
                            result: format!(
                                "Multiple teams found. Re-run with team_id:\n{}\n\nExample: sm_dazi_login(session_token='...', team_id='{}')",
                                list.join("\n"),
                                teams[0].id,
                            ),
                        });
                    }
                    teams[0].id.clone()
                }
                Err(e) => {
                    return Json(TextResult {
                        result: format!("Failed to list teams: {e}"),
                    });
                }
            }
        };

        let session = crate::core::dazi::DaziSession {
            session_token,
            team_id: team_id.clone(),
            user_name: session_info.user.name.clone(),
            saved_at: chrono::Utc::now().timestamp(),
        };
        if let Err(e) = crate::core::dazi::save_session(&data_dir, &session) {
            return Json(TextResult {
                result: format!("Failed to save session: {e}"),
            });
        }

        Json(TextResult {
            result: format!(
                "Logged in as '{}', team '{}'. Session saved.\nYou can now use sm_dazi_publish_bundle.",
                session_info.user.name, team_id,
            ),
        })
    }

    #[tool(description = "Logout from 搭子. Removes saved session.")]
    fn sm_dazi_logout(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        drop(mgr);
        crate::core::dazi::clear_session(&data_dir);
        Json(TextResult {
            result: "Logged out from 搭子. Session removed.".into(),
        })
    }

    #[tool(
        description = "Publish a bundle (组合包) to 搭子 marketplace. Requires login (sm_dazi_login). Provide agent_ids and/or skill_names to include."
    )]
    fn sm_dazi_publish_bundle(
        &self,
        Parameters(p): Parameters<DaziPublishBundleParams>,
    ) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        drop(mgr);

        let session = match crate::core::dazi::load_session(&data_dir) {
            Some(s) => s,
            None => {
                return Json(TextResult {
                    result: "Not logged in. Run sm_dazi_login first.".into(),
                });
            }
        };

        if p.agent_ids.is_empty() && p.skill_names.is_empty() {
            return Json(TextResult {
                result: "Provide at least one agent_id or skill_name to bundle.".into(),
            });
        }

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::core::dazi::DaziClient::new();

        // Verify session still valid
        if let Err(e) = rt.block_on(client.verify_session(&session.session_token)) {
            crate::core::dazi::clear_session(&data_dir);
            return Json(TextResult {
                result: format!("Session expired: {e}\nRun sm_dazi_login to re-authenticate."),
            });
        }

        match rt.block_on(client.publish_bundle(
            &session.session_token,
            &session.team_id,
            &p.agent_ids,
            &p.skill_names,
        )) {
            Ok(result) => Json(TextResult {
                result: format!(
                    "Bundle published: {} agents, {} skills{}",
                    result.summary.agents,
                    result.summary.skills,
                    if result.summary.errors > 0 {
                        format!(", {} errors", result.summary.errors)
                    } else {
                        String::new()
                    },
                ),
            }),
            Err(e) => Json(TextResult {
                result: format!("Publish bundle failed: {e}"),
            }),
        }
    }

    #[tool(
        description = "List publishable items (agents + skills) in your 搭子 team. Requires login (sm_dazi_login). Use to find agent_ids for sm_dazi_publish_bundle."
    )]
    fn sm_dazi_publishable(&self) -> Json<TextResult> {
        let mgr = self.manager.lock().unwrap();
        let data_dir = mgr.paths().data_dir().to_path_buf();
        drop(mgr);

        let session = match crate::core::dazi::load_session(&data_dir) {
            Some(s) => s,
            None => {
                return Json(TextResult {
                    result: "Not logged in. Run sm_dazi_login first.".into(),
                });
            }
        };

        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::core::dazi::DaziClient::new();

        match rt.block_on(client.get_publishable(&session.session_token, &session.team_id)) {
            Ok(data) => {
                let mut lines = Vec::new();

                if let Some(agents) = data.get("agents").and_then(|a| a.as_array()) {
                    lines.push(format!("── Agents ({}) ──", agents.len()));
                    for a in agents {
                        let id = a.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        lines.push(format!("  {name} (id: {id})"));
                    }
                }

                if let Some(skills) = data.get("skills").and_then(|a| a.as_array()) {
                    lines.push(format!("\n── Skills ({}) ──", skills.len()));
                    for s in skills {
                        let name = s.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        lines.push(format!("  {name}"));
                    }
                }

                if lines.is_empty() {
                    Json(TextResult {
                        result: "No publishable items in your team.".into(),
                    })
                } else {
                    lines.push(
                        "\nUse agent ids and skill names with sm_dazi_publish_bundle.".into(),
                    );
                    Json(TextResult {
                        result: lines.join("\n"),
                    })
                }
            }
            Err(e) => {
                if e.to_string().contains("expired") {
                    crate::core::dazi::clear_session(&data_dir);
                }
                Json(TextResult {
                    result: format!("Failed: {e}"),
                })
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for SmServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Runai — AI skill/MCP manager.\n\
             \n\
             SKILL DISCOVERY (proactive):\n\
             1. sm_search → find skills (local + market)\n\
             2. sm_market_install → install (returns CLI command, run via Bash)\n\
             3. Fallback: Bash `npx skills find <keyword>` or `runai install owner/repo`\n\
             4. After install → sm_scan, sm_enable\n\
             \n\
             CORE: sm_list, sm_status, sm_enable, sm_disable, sm_search, sm_scan\n\
             INSTALL: sm_install(repo), sm_market_install\n\
             GROUPS: sm_groups, sm_create_group, sm_delete_group, sm_group_members\n\
             STATS: sm_usage_stats\n\
             BACKUP: sm_backup, sm_backups, sm_restore\n\
             MARKET: sm_market"
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
    fn tool_router_has_expected_tools() {
        let server = SmServer::new().unwrap();
        let tools = server.tool_router.list_all();
        let tool_names: Vec<String> = tools.iter().map(|t| t.name.to_string()).collect();
        eprintln!("Registered tools: {}", tools.len());
        for name in &tool_names {
            eprintln!("  - {name}");
        }

        // 18 core expected tools
        let expected_core = [
            "sm_list",
            "sm_status",
            "sm_enable",
            "sm_disable",
            "sm_search",
            "sm_scan",
            "sm_delete",
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

        // 12 dazi tools
        let expected_dazi = [
            "sm_dazi_search",
            "sm_dazi_install",
            "sm_dazi_install_bundle",
            "sm_dazi_list",
            "sm_dazi_stats",
            "sm_dazi_publish",
            "sm_dazi_publish_agent",
            "sm_dazi_refresh",
            "sm_dazi_login",
            "sm_dazi_logout",
            "sm_dazi_publish_bundle",
            "sm_dazi_publishable",
        ];
        for name in &expected_dazi {
            assert!(
                tool_names.iter().any(|t| t == name),
                "Expected dazi tool '{name}' not found"
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

        assert_eq!(
            tools.len(),
            30,
            "Expected 30 tools (18 core + 12 dazi), got {}",
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

    #[test]
    fn sm_search_no_results_suggests_npx_skills_find() {
        let server = SmServer::new().unwrap();
        let Json(result) = server.sm_search(Parameters(NameParams {
            name: "xyznonexistent99999".into(),
        }));
        assert!(
            result.result.contains("npx skills find"),
            "no-results message should suggest npx skills find, got: {}",
            result.result
        );
    }
}

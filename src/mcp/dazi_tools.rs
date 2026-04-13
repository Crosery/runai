use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::schemars;
use rmcp::serde_json;
use rmcp::{tool, tool_router};
use serde::Deserialize;

use super::{NameParams, NameTargetParams, SmServer, TextResult, is_safe_shell_arg, parse_target};

// ── Param structs ──

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

// ── Helper functions ──

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

// ── Dazi tool methods ──

#[tool_router(router = dazi_tool_router)]
impl SmServer {
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

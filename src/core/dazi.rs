//! Dazi marketplace client — fetches skills and agents from dazi.ktvsky.com.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

const DEFAULT_BASE_URL: &str = "http://dazi.ktvsky.com";
const CACHE_DIR: &str = "dazi-cache";
const CACHE_MAX_AGE_SECS: u64 = 3600; // 1 hour
const TOKEN_FILE: &str = "dazi-token.json";
const SESSION_FILE: &str = "dazi-session.json";
/// Refresh token when less than this many seconds remain before expiry.
const TOKEN_REFRESH_MARGIN_SECS: i64 = 300; // 5 minutes

// ── Data types ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaziSkill {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub entry: String,
    #[serde(default, rename = "publishedBy")]
    pub published_by: String,
    #[serde(default, rename = "isOfficial")]
    pub is_official: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "downloadCount")]
    pub download_count: u64,
    /// Local-only: marks whether this skill is already installed.
    #[serde(skip)]
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaziAgent {
    pub id: String,
    pub name: String,
    pub version: u32,
    #[serde(default)]
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub capabilities: String,
    #[serde(default, rename = "adapterType")]
    pub adapter_type: String,
    #[serde(default, rename = "promptTemplate")]
    pub prompt_template: String,
    #[serde(default, rename = "publishedBy")]
    pub published_by: String,
    #[serde(default, rename = "isOfficial")]
    pub is_official: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "downloadCount")]
    pub download_count: u64,
    /// Local-only: marks whether this agent is already installed.
    #[serde(skip)]
    pub installed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaziBundle {
    pub id: String,
    pub name: String,
    pub version: u32,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "sourceTeamName")]
    pub source_team_name: String,
    #[serde(default, rename = "publishedBy")]
    pub published_by: String,
    #[serde(default, rename = "isOfficial")]
    pub is_official: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default, rename = "agentRefs")]
    pub agent_refs: Vec<String>,
    #[serde(default, rename = "skillRefs")]
    pub skill_refs: Vec<String>,
    #[serde(default, rename = "downloadCount")]
    pub download_count: u64,
}

/// Which kind of resource to browse/install from dazi.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaziKind {
    Skills,
    Agents,
    Bundles,
}

impl DaziKind {
    pub fn label(&self) -> &'static str {
        match self {
            DaziKind::Skills => "Skills",
            DaziKind::Agents => "Agents",
            DaziKind::Bundles => "组合包",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            DaziKind::Skills => DaziKind::Agents,
            DaziKind::Agents => DaziKind::Bundles,
            DaziKind::Bundles => DaziKind::Skills,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            DaziKind::Skills => DaziKind::Bundles,
            DaziKind::Agents => DaziKind::Skills,
            DaziKind::Bundles => DaziKind::Agents,
        }
    }
}

// ── Cache ──

fn cache_path(data_dir: &Path, kind: DaziKind) -> std::path::PathBuf {
    let name = match kind {
        DaziKind::Skills => "skills.json",
        DaziKind::Agents => "agents.json",
        DaziKind::Bundles => "bundles.json",
    };
    data_dir.join(CACHE_DIR).join(name)
}

pub fn load_cache_skills(data_dir: &Path) -> Option<Vec<DaziSkill>> {
    load_cache_generic(&cache_path(data_dir, DaziKind::Skills))
}

pub fn load_cache_agents(data_dir: &Path) -> Option<Vec<DaziAgent>> {
    load_cache_generic(&cache_path(data_dir, DaziKind::Agents))
}

fn load_cache_generic<T: serde::de::DeserializeOwned>(path: &Path) -> Option<Vec<T>> {
    let meta = std::fs::metadata(path).ok()?;
    let age = meta.modified().ok()?.elapsed().ok()?.as_secs();
    if age > CACHE_MAX_AGE_SECS {
        return None;
    }
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save_cache_skills(data_dir: &Path, items: &[DaziSkill]) -> Result<()> {
    save_cache_generic(&cache_path(data_dir, DaziKind::Skills), items)
}

pub fn save_cache_agents(data_dir: &Path, items: &[DaziAgent]) -> Result<()> {
    save_cache_generic(&cache_path(data_dir, DaziKind::Agents), items)
}

pub fn load_cache_bundles(data_dir: &Path) -> Option<Vec<DaziBundle>> {
    load_cache_generic(&cache_path(data_dir, DaziKind::Bundles))
}

pub fn save_cache_bundles(data_dir: &Path, items: &[DaziBundle]) -> Result<()> {
    save_cache_generic(&cache_path(data_dir, DaziKind::Bundles), items)
}

fn save_cache_generic<T: Serialize>(path: &Path, items: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(items)?)?;
    Ok(())
}

// ── MCP Config & Token ──

/// Response from /api/marketplace/mcp-config
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaziMcpConfig {
    pub url: String,
    pub token: String,
    pub config: DaziMcpConfigInner,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaziMcpConfigInner {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: std::collections::HashMap<String, serde_json::Value>,
}

/// Cached token stored on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedToken {
    pub token: String,
    pub url: String,
    pub fetched_at: i64,
    /// Decoded `iat` from JWT, used to estimate expiry.
    pub iat: i64,
}

impl CachedToken {
    /// Check if the token is still valid (with margin).
    /// JWT from dazi has `iat` but no explicit `exp`. We assume 1 hour lifetime.
    pub fn is_valid(&self) -> bool {
        let now = chrono::Utc::now().timestamp();
        let assumed_exp = self.iat + 3600; // assume 1h lifetime
        now < (assumed_exp - TOKEN_REFRESH_MARGIN_SECS)
    }
}

/// Decode the `iat` field from a JWT without verifying the signature.
fn decode_jwt_iat(token: &str) -> Option<i64> {
    let parts: Vec<&str> = token.splitn(3, '.').collect();
    if parts.len() < 2 {
        return None;
    }
    // JWT base64url payload
    let payload = parts[1];
    // Pad to multiple of 4
    let padded = match payload.len() % 4 {
        2 => format!("{payload}=="),
        3 => format!("{payload}="),
        _ => payload.to_string(),
    };
    let decoded = base64_decode(&padded)?;
    let json: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    json.get("iat")?.as_i64()
}

/// Minimal base64url decode (no external crate needed).
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    // Convert base64url to standard base64
    let standard: String = input
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();

    // Simple base64 decode
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut buf = Vec::new();
    let mut bits: u32 = 0;
    let mut bit_count: u32 = 0;

    for ch in standard.bytes() {
        if ch == b'=' {
            break;
        }
        let val = alphabet.iter().position(|&b| b == ch)? as u32;
        bits = (bits << 6) | val;
        bit_count += 6;
        if bit_count >= 8 {
            bit_count -= 8;
            buf.push((bits >> bit_count) as u8);
            bits &= (1 << bit_count) - 1;
        }
    }
    Some(buf)
}

fn token_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(TOKEN_FILE)
}

/// Load cached token from disk.
pub fn load_token(data_dir: &Path) -> Option<CachedToken> {
    let path = token_path(data_dir);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Save token to disk cache.
pub fn save_token(data_dir: &Path, token: &CachedToken) -> Result<()> {
    let path = token_path(data_dir);
    std::fs::write(&path, serde_json::to_string_pretty(token)?)?;
    Ok(())
}

/// The name used for dazi MCP in CLI configs.
const DAZI_MCP_NAME: &str = "dazi-marketplace";

/// Write dazi MCP entry into a Claude-style JSON config.
/// Creates or updates the entry with fresh token.
fn write_mcp_to_claude_json(path: &Path, mcp_url: &str, token: &str) -> Result<()> {
    let mut config: serde_json::Value = if path.exists() {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };

    let servers = config
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("config is not an object"))?
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));

    let entry = serde_json::json!({
        "type": "url",
        "url": mcp_url,
        "headers": {
            "Authorization": format!("Bearer {token}")
        }
    });

    servers
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("mcpServers is not an object"))?
        .insert(DAZI_MCP_NAME.into(), entry);

    std::fs::write(path, serde_json::to_string_pretty(&config)?)?;
    Ok(())
}

/// Write dazi MCP entry into a Gemini-style JSON config.
fn write_mcp_to_generic_json(path: &Path, mcp_url: &str, token: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_mcp_to_claude_json(path, mcp_url, token)
}

/// Register (or update) dazi MCP in all supported CLI configs.
pub fn register_dazi_mcp(home: &Path, mcp_url: &str, token: &str) -> Vec<String> {
    let mut updated = Vec::new();

    // Claude: ~/.claude.json
    let claude_path = home.join(".claude.json");
    if write_mcp_to_claude_json(&claude_path, mcp_url, token).is_ok() {
        updated.push("claude".into());
    }

    // Gemini: ~/.gemini/settings.json
    let gemini_path = home.join(".gemini/settings.json");
    if write_mcp_to_generic_json(&gemini_path, mcp_url, token).is_ok() {
        updated.push("gemini".into());
    }

    updated
}

/// Remove dazi MCP entry from all CLI configs.
pub fn unregister_dazi_mcp(home: &Path) {
    let paths = [
        home.join(".claude.json"),
        home.join(".gemini/settings.json"),
    ];
    for path in &paths {
        if !path.exists() {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(servers) = config.get_mut("mcpServers").and_then(|s| s.as_object_mut())
                {
                    if servers.remove(DAZI_MCP_NAME).is_some() {
                        let _ = std::fs::write(
                            path,
                            serde_json::to_string_pretty(&config).unwrap_or_default(),
                        );
                    }
                }
            }
        }
    }
}

// ── API client ──

pub struct DaziClient {
    base_url: String,
}

impl DaziClient {
    pub fn new() -> Self {
        let base_url =
            std::env::var("DAZI_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self { base_url }
    }

    pub fn with_url(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }

    fn client() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder().user_agent("runai/0.5").build()?)
    }

    /// Fetch all published skills.
    pub async fn fetch_skills(&self) -> Result<Vec<DaziSkill>> {
        let url = format!("{}/api/marketplace/skills", self.base_url);
        let client = Self::client()?;
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("Dazi skills API returned HTTP {}", resp.status());
        }
        let skills: Vec<DaziSkill> = resp.json().await?;
        Ok(skills)
    }

    /// Fetch all published agents.
    pub async fn fetch_agents(&self) -> Result<Vec<DaziAgent>> {
        let url = format!("{}/api/marketplace/agents", self.base_url);
        let client = Self::client()?;
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("Dazi agents API returned HTTP {}", resp.status());
        }
        let agents: Vec<DaziAgent> = resp.json().await?;
        Ok(agents)
    }

    /// Download a skill as a ZIP and extract it into the skills directory.
    /// Returns the skill name on success.
    pub async fn install_skill(
        &self,
        skill_name: &str,
        paths: &crate::core::paths::AppPaths,
    ) -> Result<String> {
        let url = format!(
            "{}/api/marketplace/skills/{}/download",
            self.base_url,
            urlencoded(skill_name),
        );
        let client = Self::client()?;
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!(
                "Dazi skill download failed: HTTP {} for '{}'",
                resp.status(),
                skill_name,
            );
        }

        let bytes = resp.bytes().await?;
        let skill_dir = paths.skills_dir().join(skill_name);
        std::fs::create_dir_all(&skill_dir)?;
        extract_zip(&bytes, &skill_dir)?;
        Ok(skill_name.to_string())
    }

    /// Download an agent definition JSON and save it to the skills directory.
    /// Agent JSON contains the promptTemplate which becomes the SKILL.md equivalent.
    pub async fn install_agent(
        &self,
        agent_name: &str,
        paths: &crate::core::paths::AppPaths,
    ) -> Result<String> {
        let url = format!(
            "{}/api/marketplace/agents/{}/download",
            self.base_url,
            urlencoded(agent_name),
        );
        let client = Self::client()?;
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!(
                "Dazi agent download failed: HTTP {} for '{}'",
                resp.status(),
                agent_name,
            );
        }

        let agent: DaziAgent = resp.json().await?;
        let skill_dir = paths.skills_dir().join(agent_name);
        std::fs::create_dir_all(&skill_dir)?;

        // Generate SKILL.md from agent definition
        let skill_md = format_agent_as_skill(&agent);
        std::fs::write(skill_dir.join("SKILL.md"), skill_md)?;

        // Also save the raw agent JSON for reference
        std::fs::write(
            skill_dir.join("agent.json"),
            serde_json::to_string_pretty(&agent)?,
        )?;

        Ok(agent_name.to_string())
    }

    /// Fetch all published bundles.
    pub async fn fetch_bundles(&self) -> Result<Vec<DaziBundle>> {
        let url = format!("{}/api/marketplace/bundles", self.base_url);
        let client = Self::client()?;
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("Dazi bundles API returned HTTP {}", resp.status());
        }
        let bundles: Vec<DaziBundle> = resp.json().await?;
        Ok(bundles)
    }

    /// Install a bundle: download and install all its skills and agents.
    /// Returns a list of installed resource names.
    pub async fn install_bundle(
        &self,
        bundle: &DaziBundle,
        paths: &crate::core::paths::AppPaths,
    ) -> Result<Vec<String>> {
        let mut installed = Vec::new();

        for skill_name in &bundle.skill_refs {
            match self.install_skill(skill_name, paths).await {
                Ok(name) => installed.push(name),
                Err(e) => tracing::warn!("bundle: failed to install skill '{}': {}", skill_name, e),
            }
        }

        for agent_name in &bundle.agent_refs {
            match self.install_agent(agent_name, paths).await {
                Ok(name) => installed.push(name),
                Err(e) => tracing::warn!("bundle: failed to install agent '{}': {}", agent_name, e),
            }
        }

        Ok(installed)
    }

    /// Fetch a fresh MCP config (including new token) from the dazi API.
    pub async fn fetch_mcp_config(&self) -> Result<DaziMcpConfig> {
        let url = format!("{}/api/marketplace/mcp-config", self.base_url);
        let client = Self::client()?;
        let resp = client.get(&url).send().await?;
        if !resp.status().is_success() {
            bail!("Dazi mcp-config API returned HTTP {}", resp.status());
        }
        let config: DaziMcpConfig = resp.json().await?;
        Ok(config)
    }

    /// Ensure we have a valid token. Returns (mcp_url, token).
    /// Loads from cache if still valid, otherwise fetches fresh from API,
    /// saves to disk, and updates all CLI configs.
    pub async fn ensure_token(&self, data_dir: &Path) -> Result<(String, String)> {
        // Try cached token first
        if let Some(cached) = load_token(data_dir) {
            if cached.is_valid() {
                return Ok((cached.url, cached.token));
            }
        }

        // Fetch fresh
        let mcp_config = self.fetch_mcp_config().await?;
        let token = &mcp_config.token;
        let mcp_url = &mcp_config.url;

        let iat = decode_jwt_iat(token).unwrap_or_else(|| chrono::Utc::now().timestamp());
        let cached = CachedToken {
            token: token.clone(),
            url: mcp_url.clone(),
            fetched_at: chrono::Utc::now().timestamp(),
            iat,
        };
        let _ = save_token(data_dir, &cached);

        // Update CLI configs with new token
        if let Some(home) = dirs::home_dir() {
            register_dazi_mcp(&home, mcp_url, token);
            // Notify Claude Code to reload the dazi MCP entry
            sync_claude_mcp_entry(DAZI_MCP_NAME, mcp_url, token);
        }

        Ok((mcp_url.clone(), token.clone()))
    }

    /// Publish a skill to the dazi marketplace.
    /// `name`: skill name, `content`: SKILL.md content, `description`: short description.
    pub async fn publish_skill(
        &self,
        name: &str,
        content: &str,
        description: &str,
    ) -> Result<PublishResult> {
        let url = format!("{}/api/marketplace/skills", self.base_url);
        let client = Self::client()?;
        let body = serde_json::json!({
            "type": "skill",
            "name": name,
            "content": content,
            "description": description,
        });
        let resp = client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let err_text = resp.text().await.unwrap_or_default();
            bail!("Publish failed: {err_text}");
        }
        let result: PublishResult = resp.json().await?;
        Ok(result)
    }

    /// Publish an agent to the dazi marketplace.
    pub async fn publish_agent(
        &self,
        name: &str,
        title: &str,
        description: &str,
        role: &str,
        prompt_template: &str,
        tags: &[String],
    ) -> Result<PublishResult> {
        let url = format!("{}/api/marketplace/agents", self.base_url);
        let client = Self::client()?;
        let body = serde_json::json!({
            "type": "agent",
            "name": name,
            "title": title,
            "description": description,
            "role": role,
            "promptTemplate": prompt_template,
            "tags": tags,
        });
        let resp = client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let err_text = resp.text().await.unwrap_or_default();
            bail!("Publish agent failed: {err_text}");
        }
        let result: PublishResult = resp.json().await?;
        Ok(result)
    }

    // ── Authenticated team operations ──

    /// Verify a session token is still valid. Returns session info or error.
    pub async fn verify_session(&self, session_token: &str) -> Result<SessionInfo> {
        let url = format!("{}/api/auth/get-session", self.base_url);
        let client = Self::client()?;
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {session_token}"))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("Session expired. Run sm_dazi_login to re-authenticate.");
        }
        if !resp.status().is_success() {
            bail!("Session check failed: HTTP {}", resp.status());
        }
        let info: SessionInfo = resp.json().await?;
        Ok(info)
    }

    /// List teams the authenticated user belongs to.
    pub async fn list_teams(&self, session_token: &str) -> Result<Vec<TeamInfo>> {
        let url = format!("{}/api/teams", self.base_url);
        let client = Self::client()?;
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {session_token}"))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("Session expired. Run sm_dazi_login to re-authenticate.");
        }
        if !resp.status().is_success() {
            bail!("List teams failed: HTTP {}", resp.status());
        }
        let teams: Vec<TeamInfo> = resp.json().await?;
        Ok(teams)
    }

    /// Get publishable items (agents + skills) for a team.
    pub async fn get_publishable(
        &self,
        session_token: &str,
        team_id: &str,
    ) -> Result<serde_json::Value> {
        let url = format!(
            "{}/api/teams/{}/marketplace/publishable",
            self.base_url,
            urlencoded(team_id),
        );
        let client = Self::client()?;
        let resp = client
            .get(&url)
            .header("Authorization", format!("Bearer {session_token}"))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("Session expired. Run sm_dazi_login to re-authenticate.");
        }
        if !resp.status().is_success() {
            bail!("Get publishable failed: HTTP {}", resp.status());
        }
        let data: serde_json::Value = resp.json().await?;
        Ok(data)
    }

    /// Publish a bundle (组合包) to the marketplace via team API.
    pub async fn publish_bundle(
        &self,
        session_token: &str,
        team_id: &str,
        agent_ids: &[String],
        skill_names: &[String],
    ) -> Result<BundlePublishResult> {
        let url = format!(
            "{}/api/teams/{}/marketplace/publish-bundle",
            self.base_url,
            urlencoded(team_id),
        );
        let client = Self::client()?;
        let body = serde_json::json!({
            "agentIds": agent_ids,
            "skillNames": skill_names,
        });
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {session_token}"))
            .json(&body)
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("Session expired. Run sm_dazi_login to re-authenticate.");
        }
        if !resp.status().is_success() {
            let err_text = resp.text().await.unwrap_or_default();
            bail!("Publish bundle failed: {err_text}");
        }
        let result: BundlePublishResult = resp.json().await?;
        Ok(result)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishResult {
    pub published: bool,
    pub name: String,
    pub version: u32,
}

// ── Session (for team API: bundle publish) ──

/// Saved session for authenticated team operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaziSession {
    pub session_token: String,
    pub team_id: String,
    /// User display name (optional, for display)
    #[serde(default)]
    pub user_name: String,
    pub saved_at: i64,
}

/// Session info returned by /api/auth/get-session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session: SessionData,
    pub user: SessionUser,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub id: String,
    #[serde(default)]
    pub token: String,
    #[serde(default, rename = "expiresAt")]
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionUser {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub email: String,
}

/// Team info returned by /api/teams.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamInfo {
    pub id: String,
    pub name: String,
}

/// Bundle publish response summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundlePublishSummary {
    pub agents: u32,
    pub skills: u32,
    #[serde(default)]
    pub errors: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundlePublishResult {
    pub summary: BundlePublishSummary,
}

fn session_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join(SESSION_FILE)
}

pub fn load_session(data_dir: &Path) -> Option<DaziSession> {
    let path = session_path(data_dir);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save_session(data_dir: &Path, session: &DaziSession) -> Result<()> {
    let path = session_path(data_dir);
    std::fs::write(&path, serde_json::to_string_pretty(session)?)?;
    Ok(())
}

pub fn clear_session(data_dir: &Path) {
    let path = session_path(data_dir);
    let _ = std::fs::remove_file(path);
}

/// Notify Claude Code to reload a specific MCP entry via `claude mcp` CLI.
fn sync_claude_mcp_entry(name: &str, url: &str, token: &str) {
    let entry = serde_json::json!({
        "type": "url",
        "url": url,
        "headers": {
            "Authorization": format!("Bearer {token}")
        }
    });
    let json_str = serde_json::to_string(&entry).unwrap_or_default();
    // Remove then re-add to force reconnect
    let _ = std::process::Command::new("claude")
        .args(["mcp", "remove", name, "-s", "user"])
        .output();
    let _ = std::process::Command::new("claude")
        .args(["mcp", "add-json", "-s", "user", name, &json_str])
        .output();
}

/// Refresh the dazi MCP token if expired. Designed to be called periodically.
/// Returns Ok(true) if token was refreshed, Ok(false) if still valid.
pub async fn refresh_token_if_needed(data_dir: &Path) -> Result<bool> {
    if let Some(cached) = load_token(data_dir) {
        if cached.is_valid() {
            return Ok(false);
        }
    }

    let client = DaziClient::new();
    let (_url, _token) = client.ensure_token(data_dir).await?;
    Ok(true)
}

/// Blocking version of token refresh, for use in sync contexts.
pub fn refresh_token_blocking(data_dir: &Path) -> Result<bool> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(refresh_token_if_needed(data_dir))
}

/// Mark installed status on skill list.
pub fn mark_installed_skills(skills: &mut [DaziSkill], installed_names: &[String]) {
    for skill in skills.iter_mut() {
        skill.installed = installed_names.iter().any(|n| n == &skill.name);
    }
}

/// Mark installed status on agent list.
pub fn mark_installed_agents(agents: &mut [DaziAgent], installed_names: &[String]) {
    for agent in agents.iter_mut() {
        agent.installed = installed_names.iter().any(|n| n == &agent.name);
    }
}

// ── Helpers ──

fn urlencoded(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

/// Convert an agent definition into a SKILL.md file.
fn format_agent_as_skill(agent: &DaziAgent) -> String {
    let mut lines = Vec::new();

    // Title
    let title = if agent.title.is_empty() {
        &agent.name
    } else {
        &agent.title
    };
    lines.push(format!("# {title}"));
    lines.push(String::new());

    // Description
    if !agent.description.is_empty() {
        lines.push(format!("> {}", agent.description));
        lines.push(String::new());
    }

    // Metadata
    if !agent.role.is_empty() {
        lines.push(format!("**Role:** {}", agent.role));
    }
    if !agent.tags.is_empty() {
        lines.push(format!("**Tags:** {}", agent.tags.join(", ")));
    }
    if !agent.role.is_empty() || !agent.tags.is_empty() {
        lines.push(String::new());
    }

    // Prompt template (the actual skill content)
    if !agent.prompt_template.is_empty() {
        lines.push(agent.prompt_template.clone());
    }

    lines.join("\n")
}

/// Extract a ZIP archive from bytes into the given directory.
/// If all entries share a common top-level directory prefix (e.g. "docx/..."),
/// that prefix is stripped so files land directly in dest_dir.
fn extract_zip(bytes: &[u8], dest_dir: &Path) -> Result<()> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)?;

    // Detect common prefix: if all entries start with "name/" where name matches
    // the dest_dir's last component, strip it.
    let common_prefix = detect_common_prefix(&mut archive);

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let raw_name = file.name().to_string();

        // Strip common prefix if detected
        let name = if let Some(ref prefix) = common_prefix {
            raw_name
                .strip_prefix(prefix.as_str())
                .unwrap_or(&raw_name)
                .to_string()
        } else {
            raw_name
        };

        // Skip empty names (the prefix directory itself) and directory entries
        if name.is_empty() || name.ends_with('/') {
            if !name.is_empty() {
                let dir = dest_dir.join(&name);
                std::fs::create_dir_all(&dir)?;
            }
            continue;
        }

        // Security: reject paths with ".." or absolute paths
        if name.contains("..") || name.starts_with('/') {
            continue;
        }

        let dest_path = dest_dir.join(&name);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&dest_path)?;
        std::io::copy(&mut file, &mut out)?;
    }

    Ok(())
}

/// Detect if all ZIP entries share a single top-level directory prefix.
/// Returns Some("prefix/") if so, None otherwise.
fn detect_common_prefix(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>) -> Option<String> {
    if archive.len() == 0 {
        return None;
    }

    let mut prefix: Option<String> = None;
    for i in 0..archive.len() {
        let file = match archive.by_index_raw(i) {
            Ok(f) => f,
            Err(_) => return None,
        };
        let name = file.name();

        // Find the first "/" to get top-level dir
        let top = match name.find('/') {
            Some(pos) => &name[..=pos], // e.g. "docx/"
            None => return None,        // file at root level → no common prefix
        };

        match &prefix {
            None => prefix = Some(top.to_string()),
            Some(p) if p != top => return None, // different prefixes
            _ => {}
        }
    }

    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencoded_handles_special_chars() {
        assert_eq!(urlencoded("find-skills"), "find-skills");
        assert_eq!(urlencoded("hello world"), "hello%20world");
    }

    #[test]
    fn format_agent_as_skill_includes_all_fields() {
        let agent = DaziAgent {
            id: "123".into(),
            name: "test-agent".into(),
            version: 1,
            title: "Test Agent".into(),
            description: "A test agent".into(),
            tags: vec!["testing".into(), "demo".into()],
            role: "tester".into(),
            capabilities: "testing stuff".into(),
            adapter_type: "claude_local".into(),
            prompt_template: "You are a test agent.\n\n## Instructions\nDo testing.".into(),
            published_by: "test".into(),
            is_official: false,
            status: "published".into(),
            download_count: 0,
            installed: false,
        };

        let md = format_agent_as_skill(&agent);
        assert!(md.contains("# Test Agent"));
        assert!(md.contains("> A test agent"));
        assert!(md.contains("**Role:** tester"));
        assert!(md.contains("**Tags:** testing, demo"));
        assert!(md.contains("You are a test agent."));
    }

    #[test]
    fn cache_round_trip_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = vec![DaziSkill {
            id: "1".into(),
            name: "test".into(),
            version: 1,
            description: "desc".into(),
            tags: vec![],
            entry: "SKILL.md".into(),
            published_by: "me".into(),
            is_official: false,
            status: "published".into(),
            download_count: 0,
            installed: false,
        }];
        save_cache_skills(tmp.path(), &skills).unwrap();
        let loaded = load_cache_skills(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "test");
    }

    #[test]
    fn cache_round_trip_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let agents = vec![DaziAgent {
            id: "1".into(),
            name: "agent1".into(),
            version: 1,
            title: "Agent One".into(),
            description: "desc".into(),
            tags: vec![],
            role: "dev".into(),
            capabilities: "coding".into(),
            adapter_type: "claude_local".into(),
            prompt_template: "You are agent one.".into(),
            published_by: "me".into(),
            is_official: false,
            status: "published".into(),
            download_count: 0,
            installed: false,
        }];
        save_cache_agents(tmp.path(), &agents).unwrap();
        let loaded = load_cache_agents(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "agent1");
    }

    #[test]
    fn mark_installed_skills_sets_flag() {
        let mut skills = vec![
            DaziSkill {
                id: "1".into(),
                name: "foo".into(),
                version: 1,
                description: String::new(),
                tags: vec![],
                entry: "SKILL.md".into(),
                published_by: String::new(),
                is_official: false,
                status: String::new(),
                download_count: 0,
                installed: false,
            },
            DaziSkill {
                id: "2".into(),
                name: "bar".into(),
                version: 1,
                description: String::new(),
                tags: vec![],
                entry: "SKILL.md".into(),
                published_by: String::new(),
                is_official: false,
                status: String::new(),
                download_count: 0,
                installed: false,
            },
        ];
        mark_installed_skills(&mut skills, &["foo".to_string()]);
        assert!(skills[0].installed);
        assert!(!skills[1].installed);
    }

    #[test]
    fn extract_zip_creates_files() {
        // Create a minimal ZIP in memory (no common prefix)
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("SKILL.md", options).unwrap();
        std::io::Write::write_all(&mut writer, b"# Test Skill").unwrap();
        let cursor = writer.finish().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        extract_zip(cursor.get_ref(), tmp.path()).unwrap();

        let content = std::fs::read_to_string(tmp.path().join("SKILL.md")).unwrap();
        assert_eq!(content, "# Test Skill");
    }

    #[test]
    fn extract_zip_strips_common_prefix() {
        // Simulate dazi ZIP: all files under "docx/" prefix
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut writer = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default();

        writer.add_directory("docx/", options).unwrap();
        writer.start_file("docx/SKILL.md", options).unwrap();
        std::io::Write::write_all(&mut writer, b"# Docx Skill").unwrap();
        writer.start_file("docx/scripts/run.py", options).unwrap();
        std::io::Write::write_all(&mut writer, b"print('hello')").unwrap();
        let cursor = writer.finish().unwrap();

        let tmp = tempfile::tempdir().unwrap();
        extract_zip(cursor.get_ref(), tmp.path()).unwrap();

        // Should land directly in dest_dir, NOT in dest_dir/docx/
        assert!(
            tmp.path().join("SKILL.md").exists(),
            "SKILL.md should be at root, not nested"
        );
        assert!(
            !tmp.path().join("docx").exists(),
            "docx/ prefix should be stripped"
        );
        assert!(tmp.path().join("scripts/run.py").exists());

        let content = std::fs::read_to_string(tmp.path().join("SKILL.md")).unwrap();
        assert_eq!(content, "# Docx Skill");
    }

    #[test]
    fn decode_jwt_iat_parses_real_token() {
        // Real-ish JWT: header.payload.signature
        // payload = {"instanceId":"dazi-server","iat":1774837838}
        let token = "eyJhbGciOiJIUzI1NiJ9.eyJpbnN0YW5jZUlkIjoiZGF6aS1zZXJ2ZXIiLCJpYXQiOjE3NzQ4Mzc4Mzh9.signature";
        let iat = decode_jwt_iat(token);
        assert_eq!(iat, Some(1774837838));
    }

    #[test]
    fn decode_jwt_iat_returns_none_for_garbage() {
        assert_eq!(decode_jwt_iat("not-a-jwt"), None);
        assert_eq!(decode_jwt_iat("a.b"), None); // no valid base64
    }

    #[test]
    fn cached_token_validity() {
        let now = chrono::Utc::now().timestamp();

        // Fresh token (iat = now) → valid
        let fresh = CachedToken {
            token: "t".into(),
            url: "u".into(),
            fetched_at: now,
            iat: now,
        };
        assert!(fresh.is_valid());

        // Expired token (iat = 2 hours ago) → invalid
        let expired = CachedToken {
            token: "t".into(),
            url: "u".into(),
            fetched_at: now - 7200,
            iat: now - 7200,
        };
        assert!(!expired.is_valid());

        // About to expire (iat = 56 min ago, only 4 min left < 5 min margin) → invalid
        let almost = CachedToken {
            token: "t".into(),
            url: "u".into(),
            fetched_at: now - 3360,
            iat: now - 3360,
        };
        assert!(!almost.is_valid());
    }

    #[test]
    fn token_cache_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let token = CachedToken {
            token: "test-token".into(),
            url: "http://example.com/mcp".into(),
            fetched_at: 1700000000,
            iat: 1700000000,
        };
        save_token(tmp.path(), &token).unwrap();
        let loaded = load_token(tmp.path()).unwrap();
        assert_eq!(loaded.token, "test-token");
        assert_eq!(loaded.url, "http://example.com/mcp");
        assert_eq!(loaded.iat, 1700000000);
    }

    #[test]
    fn register_dazi_mcp_writes_to_claude_json() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Write initial config
        let claude_json = home.join(".claude.json");
        std::fs::write(&claude_json, r#"{"mcpServers":{}}"#).unwrap();

        let updated = register_dazi_mcp(home, "http://test.com/mcp", "tok123");
        assert!(updated.contains(&"claude".to_string()));

        // Verify config
        let content = std::fs::read_to_string(&claude_json).unwrap();
        let config: serde_json::Value = serde_json::from_str(&content).unwrap();
        let entry = &config["mcpServers"][DAZI_MCP_NAME];
        assert_eq!(entry["type"], "url");
        assert_eq!(entry["url"], "http://test.com/mcp");
        assert_eq!(entry["headers"]["Authorization"], "Bearer tok123");
    }

    #[test]
    fn unregister_dazi_mcp_removes_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Register first
        let claude_json = home.join(".claude.json");
        std::fs::write(&claude_json, r#"{"mcpServers":{"other":{}}}"#).unwrap();
        register_dazi_mcp(home, "http://test.com/mcp", "tok");

        // Verify it's there
        let content = std::fs::read_to_string(&claude_json).unwrap();
        assert!(content.contains(DAZI_MCP_NAME));

        // Unregister
        unregister_dazi_mcp(home);

        // Verify it's gone but "other" remains
        let content = std::fs::read_to_string(&claude_json).unwrap();
        assert!(!content.contains(DAZI_MCP_NAME));
        assert!(content.contains("other"));
    }

    #[test]
    fn base64_decode_standard_and_url_safe() {
        // "hello" in base64
        let decoded = base64_decode("aGVsbG8=").unwrap();
        assert_eq!(decoded, b"hello");

        // base64url variant (- instead of +, _ instead of /)
        let decoded = base64_decode("aGVsbG8").unwrap();
        assert_eq!(decoded, b"hello");
    }
}

use rmcp::schemars;
use serde::{Deserialize, Serialize};

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

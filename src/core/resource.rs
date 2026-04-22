use crate::core::cli_target::CliTarget;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResourceKind {
    Skill,
    Mcp,
}

impl ResourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResourceKind::Skill => "skill",
            ResourceKind::Mcp => "mcp",
        }
    }
}

impl FromStr for ResourceKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "skill" => Ok(ResourceKind::Skill),
            "mcp" => Ok(ResourceKind::Mcp),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Local {
        path: PathBuf,
    },
    GitHub {
        owner: String,
        repo: String,
        branch: String,
    },
    Adopted {
        original_cli: String,
    },
}

impl Source {
    pub fn source_type(&self) -> &'static str {
        match self {
            Source::Local { .. } => "local",
            Source::GitHub { .. } => "github",
            Source::Adopted { .. } => "adopted",
        }
    }

    pub fn to_meta_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn from_meta_json(source_type: &str, meta: &str) -> Option<Self> {
        match source_type {
            "local" => serde_json::from_str(meta).ok(),
            "github" => serde_json::from_str(meta).ok(),
            "adopted" => serde_json::from_str(meta).ok(),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Resource {
    pub id: String,
    pub name: String,
    pub kind: ResourceKind,
    pub description: String,
    pub directory: PathBuf,
    pub source: Source,
    pub installed_at: i64,
    pub enabled: HashMap<CliTarget, bool>,
    pub usage_count: u64,
    pub last_used_at: Option<i64>,
}

impl Resource {
    pub fn generate_id(source: &Source, name: &str) -> String {
        match source {
            Source::Local { .. } => format!("local:{name}"),
            Source::GitHub { owner, repo, .. } => format!("github:{owner}/{repo}:{name}"),
            Source::Adopted { .. } => format!("adopted:{name}"),
        }
    }

    pub fn is_enabled_for(&self, target: CliTarget) -> bool {
        self.enabled.get(&target).copied().unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashEntry {
    pub id: String,
    pub resource_id: String,
    pub name: String,
    pub kind: ResourceKind,
    pub description: String,
    pub directory: PathBuf,
    pub source: Source,
    pub installed_at: i64,
    pub usage_count: u64,
    pub last_used_at: Option<i64>,
    pub deleted_at: i64,
    pub payload_path: Option<PathBuf>,
    #[serde(default)]
    pub enabled_targets: Vec<CliTarget>,
    #[serde(default)]
    pub group_ids: Vec<String>,
    #[serde(default)]
    pub mcp_configs: HashMap<CliTarget, Value>,
    #[serde(default)]
    pub disabled_backup: Option<Value>,
}

/// Usage statistics for a resource.
#[derive(Debug, Clone)]
pub struct UsageStat {
    pub id: String,
    pub name: String,
    pub count: u64,
    pub last_used_at: Option<i64>,
}

/// Format a timestamp as a human-readable "X ago" string.
pub fn format_time_ago(ts: Option<i64>) -> String {
    match ts {
        Some(ts) => {
            let secs = (chrono::Utc::now().timestamp() - ts).max(0);
            if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86400 {
                format!("{}h ago", secs / 3600)
            } else {
                format!("{}d ago", secs / 86400)
            }
        }
        None => "never".into(),
    }
}

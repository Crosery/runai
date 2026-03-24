use std::collections::HashMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use crate::core::cli_target::CliTarget;

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

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "skill" => Some(ResourceKind::Skill),
            "mcp" => Some(ResourceKind::Mcp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Source {
    Local { path: PathBuf },
    GitHub { owner: String, repo: String, branch: String },
    Adopted { original_cli: String },
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

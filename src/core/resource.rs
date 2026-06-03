//! Domain types for "something runai can enable/disable": `Resource`,
//! `ResourceKind` (Skill / Mcp), `Source` (provenance), `TrashEntry`, `UsageStat`.
//! Constructed by `Database` / `SkillManager` / `scanner` / `mcp_discovery`;
//! consumed by `sm_list` / `sm_trash` output and CLI/TUI rendering.
//!
//! Public API:
//! - `enum ResourceKind { Skill, Mcp }` with `as_str()` + `FromStr`.
//! - `enum Source { Local{path}, GitHub{owner,repo,branch}, Adopted{original_cli} }`
//!   with `source_type()` + `to_meta_json` / `from_meta_json` for DB round-trip.
//! - `struct Resource { id, name, kind, description, directory, source,
//!   installed_at, enabled: HashMap<CliTarget,bool>, usage_count, last_used_at,
//!   owner_user_id }`.
//!   - `owner_user_id`: `None` = public-pool (at `<data>/skills/<name>/`),
//!     `Some(uid)` = private (at `<data>/users/<uid>/skills/<name>/`). v15+;
//!     pre-v15 rows default to `None`.
//!   - `Resource::generate_id(source, name, owner_user_id) -> String` —
//!     deterministic. Public ids keep the pre-v15 shape exactly (`local:foo`,
//!     `github:o/r:foo`, `adopted:foo`); private ids gain a `u:<uid>:` prefix so
//!     the same `(source, name)` can coexist across users without PK collision.
//!     Must stay stable across versions — DB rows and group membership depend on it.
//!   - `is_enabled_for(target) -> bool`.
//! - `struct TrashEntry { ... resource_id, payload_path, enabled_targets,
//!   group_ids, mcp_configs, disabled_backup, owner_user_id }` — serialized into
//!   the DB for global trash / restore / purge. `resource_id` must keep the
//!   ORIGINAL id so restore can reattach groups + re-enable targets without fuzzy
//!   lookup; `owner_user_id` lets restore recreate the row under the right owner
//!   (`serde(default)` keeps pre-v15 payloads decodable, restoring as public).
//! - `struct UsageStat { id, name, count, last_used_at }`.
//! - `fn format_time_ago(ts: Option<i64>) -> String` — `"3h ago"` / `"never"`
//!   (on `None` — do NOT pass `0` to mean never).
//!
//! Gotcha: `Resource::enabled` is runtime-derived/cosmetic — the source of truth
//! for enable state is the filesystem (symlink / config entry), NOT this field;
//! only populate it before presenting. When adding a `Source` variant, update
//! `source_type()`, both `to_meta_json` / `from_meta_json`, DB migrations, and the
//! group-suggestion classifier.

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
    /// Owner of a private resource. `None` = public-pool resource (visible
    /// to every user, physically at `<data>/skills/<name>/`). `Some(uid)` =
    /// private to that user (physically at `<data>/users/<uid>/skills/<name>/`).
    /// Introduced in v15 schema; auto-defaulted to `None` for pre-v15 rows.
    pub owner_user_id: Option<String>,
}

impl Resource {
    /// Build a stable DB id from `(source, name, owner_user_id)`.
    /// Public ids match the pre-v15 shape exactly (`local:foo`,
    /// `github:o/r:foo`, `adopted:foo`) so existing rows keep their PK.
    /// Private ids gain a `u:<uid>:` prefix so the same `(source, name)`
    /// can coexist across users without PK collision.
    pub fn generate_id(source: &Source, name: &str, owner_user_id: Option<&str>) -> String {
        let base = match source {
            Source::Local { .. } => format!("local:{name}"),
            Source::GitHub { owner, repo, .. } => format!("github:{owner}/{repo}:{name}"),
            Source::Adopted { .. } => format!("adopted:{name}"),
        };
        match owner_user_id {
            Some(uid) => format!("u:{uid}:{base}"),
            None => base,
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
    /// Carries the trashed resource's owner_user_id so restore can recreate
    /// the row with the correct owner. `serde(default)` keeps pre-v15 trash
    /// payloads decodable (those rows simply restore as public).
    #[serde(default)]
    pub owner_user_id: Option<String>,
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

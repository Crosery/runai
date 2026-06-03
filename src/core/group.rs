//! Group definition — a named bundle of skill/MCP members.
//!
//! Each group is a TOML file at `~/.runai/groups/<id>.toml` with a display name,
//! description, kind, an `auto_enable` flag, and a list of members (each a Skill
//! or an MCP, referenced by name).
//!
//! ## Public surface
//! - `enum GroupKind { Default, Ecosystem, Custom }` (serde lowercase),
//!   `enum MemberType { Skill, Mcp }`, `struct GroupMember { name, member_type }`.
//! - `struct Group { name, description, kind, auto_enable, members }` with
//!   `to_toml` / `from_toml` round-trip and `save_to_file` / `load_from_file`.
//!   On disk the TOML is wrapped under a `[group]` table.
//!
//! ## Invariants
//! - Members are referenced by `name`, not resource id — name is what MCP tools
//!   surface; ids change if a source moves.
//! - `auto_enable` only fires at adoption time (scanner/installer); editing it
//!   later does NOT retroactively enable already-scanned resources. Default
//!   groups set it true.
//! - The `Group.toml` is the source of truth; the DB indexes membership by
//!   resource-id for fast lookup — always write both (`manager::create_group`).
//! - Never store enable state on a group: enable is per-resource-per-target, and
//!   `enable_group(target)` just iterates members.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupKind {
    Default,
    Ecosystem,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemberType {
    Skill,
    Mcp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    pub name: String,
    #[serde(rename = "type")]
    pub member_type: MemberType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupToml {
    group: GroupInner,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GroupInner {
    name: String,
    description: String,
    kind: GroupKind,
    #[serde(default)]
    auto_enable: bool,
    #[serde(default)]
    members: Vec<GroupMember>,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub name: String,
    pub description: String,
    pub kind: GroupKind,
    pub auto_enable: bool,
    pub members: Vec<GroupMember>,
}

impl Group {
    pub fn to_toml(&self) -> Result<String> {
        let wrapper = GroupToml {
            group: GroupInner {
                name: self.name.clone(),
                description: self.description.clone(),
                kind: self.kind,
                auto_enable: self.auto_enable,
                members: self.members.clone(),
            },
        };
        Ok(toml::to_string_pretty(&wrapper)?)
    }

    pub fn from_toml(s: &str) -> Result<Self> {
        let wrapper: GroupToml = toml::from_str(s)?;
        Ok(Self {
            name: wrapper.group.name,
            description: wrapper.group.description,
            kind: wrapper.group.kind,
            auto_enable: wrapper.group.auto_enable,
            members: wrapper.group.members,
        })
    }

    pub fn save_to_file(&self, path: &Path) -> Result<()> {
        let content = self.to_toml()?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn load_from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Self::from_toml(&content)
    }
}

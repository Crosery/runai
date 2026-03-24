use std::path::Path;
use anyhow::Result;
use serde::{Deserialize, Serialize};

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

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub name: String,
    pub url: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelEntry {
    pub name: String,
    pub source: String, // "owner/repo" or "owner/repo@branch"
    pub description: String,
    pub kind: String, // "skill" or "mcp"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub channels: Vec<Channel>,
}

impl ChannelConfig {
    pub fn default_config() -> Self {
        Self {
            channels: vec![
                Channel {
                    name: "ECC Skills".into(),
                    url: "https://github.com/anthropics/claude-code".into(),
                    description: "Everything Claude Code skill collection".into(),
                },
                Channel {
                    name: "Vercel Skills".into(),
                    url: "https://github.com/vercel-labs/skills".into(),
                    description: "Vercel Labs AI coding skills".into(),
                },
            ],
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(Self::default_config())
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn add_channel(&mut self, name: String, url: String, description: String) {
        if !self.channels.iter().any(|c| c.url == url) {
            self.channels.push(Channel {
                name,
                url,
                description,
            });
        }
    }

    pub fn remove_channel(&mut self, idx: usize) {
        if idx < self.channels.len() {
            self.channels.remove(idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_channels() {
        let cfg = ChannelConfig::default_config();
        assert!(cfg.channels.len() >= 2);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("channels.json");

        let mut cfg = ChannelConfig::default_config();
        cfg.add_channel(
            "Test".into(),
            "https://test.com".into(),
            "Test channel".into(),
        );
        cfg.save(&path).unwrap();

        let loaded = ChannelConfig::load(&path).unwrap();
        assert_eq!(loaded.channels.len(), cfg.channels.len());
        assert!(loaded.channels.iter().any(|c| c.name == "Test"));
    }

    #[test]
    fn add_channel_deduplicates() {
        let mut cfg = ChannelConfig::default_config();
        let before = cfg.channels.len();
        cfg.add_channel("Dup".into(), cfg.channels[0].url.clone(), "dup".into());
        assert_eq!(cfg.channels.len(), before); // no change
    }

    #[test]
    fn remove_channel() {
        let mut cfg = ChannelConfig::default_config();
        let before = cfg.channels.len();
        cfg.remove_channel(0);
        assert_eq!(cfg.channels.len(), before - 1);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let cfg = ChannelConfig::load(Path::new("/tmp/nonexistent_channel_12345.json")).unwrap();
        assert!(!cfg.channels.is_empty());
    }
}

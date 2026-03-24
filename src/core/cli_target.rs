use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CliTarget {
    Claude,
    Codex,
    Gemini,
    OpenCode,
}

impl CliTarget {
    pub const ALL: &[CliTarget] = &[
        CliTarget::Claude,
        CliTarget::Codex,
        CliTarget::Gemini,
        CliTarget::OpenCode,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            CliTarget::Claude => "claude",
            CliTarget::Codex => "codex",
            CliTarget::Gemini => "gemini",
            CliTarget::OpenCode => "opencode",
        }
    }

    pub fn skills_dir(&self) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        if cfg!(windows) {
            let appdata = dirs::data_dir().unwrap_or_else(|| home.clone());
            match self {
                CliTarget::Claude => appdata.join("claude").join("skills"),
                CliTarget::Codex => appdata.join("codex").join("skills"),
                CliTarget::Gemini => appdata.join("gemini").join("skills"),
                CliTarget::OpenCode => appdata.join("opencode").join("skills"),
            }
        } else {
            match self {
                CliTarget::Claude => home.join(".claude").join("skills"),
                CliTarget::Codex => home.join(".codex").join("skills"),
                CliTarget::Gemini => home.join(".gemini").join("skills"),
                CliTarget::OpenCode => home.join(".opencode").join("skills"),
            }
        }
    }

    pub fn settings_path(&self) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        if cfg!(windows) {
            let appdata = dirs::data_dir().unwrap_or_else(|| home.clone());
            match self {
                CliTarget::Claude => appdata.join("claude").join("settings.json"),
                CliTarget::Codex => appdata.join("codex").join("settings.json"),
                CliTarget::Gemini => appdata.join("gemini").join("settings.json"),
                CliTarget::OpenCode => appdata.join("opencode").join("settings.json"),
            }
        } else {
            match self {
                CliTarget::Claude => home.join(".claude").join("settings.json"),
                CliTarget::Codex => home.join(".codex").join("settings.json"),
                CliTarget::Gemini => home.join(".gemini").join("settings.json"),
                CliTarget::OpenCode => home.join(".opencode").join("settings.json"),
            }
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(CliTarget::Claude),
            "codex" => Some(CliTarget::Codex),
            "gemini" => Some(CliTarget::Gemini),
            "opencode" => Some(CliTarget::OpenCode),
            _ => None,
        }
    }
}

impl std::fmt::Display for CliTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

    /// User-managed skills directory — where SM creates/removes symlinks.
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

    /// Plugin-managed skills directory — `.agents/skills/`, read-only for SM.
    pub fn agents_skills_dir(&self) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        if cfg!(windows) {
            let appdata = dirs::data_dir().unwrap_or_else(|| home.clone());
            match self {
                CliTarget::Claude => appdata.join("claude").join(".agents").join("skills"),
                CliTarget::Codex => appdata.join("codex").join(".agents").join("skills"),
                CliTarget::Gemini => appdata.join("gemini").join(".agents").join("skills"),
                CliTarget::OpenCode => appdata.join("opencode").join(".agents").join("skills"),
            }
        } else {
            match self {
                CliTarget::Claude => home.join(".claude").join(".agents").join("skills"),
                CliTarget::Codex => home.join(".codex").join(".agents").join("skills"),
                CliTarget::Gemini => home.join(".gemini").join(".agents").join("skills"),
                CliTarget::OpenCode => home.join(".opencode").join(".agents").join("skills"),
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

    /// Path to the MCP config file for this CLI.
    /// Claude: ~/.claude.json
    /// Codex: ~/.codex/config.toml (TOML)
    /// Gemini: ~/.gemini/settings.json
    /// OpenCode: ~/.config/opencode/opencode.json (key="mcp", command=array)
    pub fn mcp_config_path(&self) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_default();
        match self {
            CliTarget::Claude => home.join(".claude.json"),
            CliTarget::Codex => home.join(".codex").join("config.toml"),
            CliTarget::Gemini => home.join(".gemini").join("settings.json"),
            CliTarget::OpenCode => home.join(".config").join("opencode").join("opencode.json"),
        }
    }

    /// Whether this CLI uses TOML format for MCP config (Codex).
    pub fn uses_toml(&self) -> bool {
        matches!(self, CliTarget::Codex)
    }

    /// Whether this CLI uses OpenCode's custom JSON format (key="mcp", command=array).
    pub fn uses_opencode_format(&self) -> bool {
        matches!(self, CliTarget::OpenCode)
    }
}

impl std::fmt::Display for CliTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

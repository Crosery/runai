use crate::core::cli_target::CliTarget;
use crate::core::manager::SkillManager;
use crate::core::market::{MarketSkill, SourceEntry};
use crate::core::resource::{Resource, TrashEntry};
use crate::tui::i18n::Lang;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Skills,
    Mcps,
    Groups,
    Market,
    Community,
    Hooks,
    Trash,
}

impl Tab {
    pub const ALL: &[Tab] = &[
        Tab::Skills,
        Tab::Mcps,
        Tab::Groups,
        Tab::Market,
        Tab::Community,
        Tab::Hooks,
        Tab::Trash,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Skills => "Skills",
            Tab::Mcps => "MCPs",
            Tab::Groups => "Groups",
            Tab::Market => "Market",
            Tab::Community => "Community",
            Tab::Hooks => "Hooks",
            Tab::Trash => "Trash",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum FilterMode {
    All,
    Enabled,
    Disabled,
}

impl FilterMode {
    pub fn next(self) -> Self {
        match self {
            FilterMode::All => FilterMode::Enabled,
            FilterMode::Enabled => FilterMode::Disabled,
            FilterMode::Disabled => FilterMode::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FilterMode::All => "全部",
            FilterMode::Enabled => "已启用",
            FilterMode::Disabled => "未启用",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum InputMode {
    Normal,
    Search,
    CreateGroup(u8),
    AddToGroup,
    FirstLaunch(u8),
    Install,
    AddSource,
    /// Source manager overlay
    SourceManager,
    /// Group detail overlay: view/manage members
    GroupDetail,
    /// Pick a skill to add to current group
    PickSkillForGroup,
    /// Help overlay
    Help,
    /// Rename group
    RenameGroup,
    /// Confirm a pending destructive delete/remove action
    ConfirmDelete,
    /// Community upload picker: shows scanned skill candidates, user picks one + Enter uploads
    CommunityUploadPicker,
}

#[derive(Clone, PartialEq)]
pub enum PendingDelete {
    Resource {
        id: String,
        name: String,
        kind: String,
        directory: PathBuf,
    },
    Group {
        id: String,
        name: String,
    },
    GroupMember {
        group_id: String,
        group_name: String,
        resource_id: String,
        resource_name: String,
    },
    Source {
        repo_id: String,
        label: String,
    },
}

impl PendingDelete {
    pub(super) fn return_mode(&self) -> InputMode {
        match self {
            PendingDelete::GroupMember { .. } => InputMode::GroupDetail,
            PendingDelete::Source { .. } => InputMode::SourceManager,
            PendingDelete::Resource { .. } | PendingDelete::Group { .. } => InputMode::Normal,
        }
    }
}

pub struct App {
    pub mgr: SkillManager,
    pub tab: Tab,
    pub theme_mode: super::super::theme::ThemeMode,
    pub lang: Lang,
    pub active_target: CliTarget,
    pub items: Vec<Resource>,
    pub trash_items: Vec<TrashEntry>,
    pub groups: Vec<(String, String, usize, usize, String)>,
    pub selected: usize,
    pub search: String,
    pub filter_mode: FilterMode,
    pub mode: InputMode,
    pub input_buf: String,
    pub create_name: String,
    pub group_pick_idx: usize,
    pub message: Option<String>,
    pub pending_delete: Option<PendingDelete>,
    pub status: (usize, usize, usize, usize),
    /// Max usage_count across currently-loaded items. Used by the render layer
    /// to scale per-row heat bars. Recomputed in `reload()`.
    pub max_usage_count: u64,
    pub first_launch_info: Option<FirstLaunchInfo>,
    pub scan_log: Vec<String>,
    // Market
    pub market_source_idx: usize,
    pub sources: Vec<SourceEntry>,
    pub source_pick_idx: usize,
    // Group detail
    pub detail_group_id: String,
    pub detail_group_name: String,
    pub detail_members: Vec<Resource>,
    pub detail_idx: usize,
    pub pick_items: Vec<Resource>, // available items to add (not already in group)
    pub pick_idx: usize,
    pub pick_search: String,
    pub pick_show_mcp: bool, // false=skills, true=mcps
    /// Per-source cache
    pub market_cache: HashMap<String, Vec<MarketSkill>>,
    /// Receivers for background fetches: repo_id -> rx
    pub market_rxs: HashMap<String, mpsc::Receiver<Result<Vec<MarketSkill>, String>>>,
    /// Sources currently being fetched
    pub market_fetching: std::collections::HashSet<String>,
    // ── Hooks panel state (PLANNING §1.5) ──
    /// Selected row inside the Hooks tab (0..CliTarget::ALL.len()).
    pub hook_panel_idx: usize,
    /// Per-CLI hook install status snapshot, populated by reload().
    /// True = a `runai recommend` hook entry is present in that target's
    /// settings.json. Only Claude has a real hook today; the other three
    /// rows render as "unsupported" but stay in the table so the panel is
    /// future-proof when more CLIs get a router hook contract.
    pub hook_status: HashMap<CliTarget, HookSlot>,
    // ── Community market tab state (PLANNING §1.5) ──
    /// In-memory cache of `/api/community/list` result.
    pub community_skills: Vec<CommunitySkill>,
    /// True while a fetch is in flight.
    pub community_loading: bool,
    /// Last fetch error string (empty on success or before first fetch).
    pub community_error: String,
    /// Cursor into `visible_community()` (search-filtered).
    pub community_idx: usize,
    // ── Community upload picker (PLANNING §1.5 待办) ──
    /// Local skill candidates scanned from `~/.claude/skills/` + cwd `.claude/skills/`.
    pub upload_candidates: Vec<UploadCandidate>,
    /// Cursor into `upload_candidates`.
    pub upload_idx: usize,
    /// True while a single upload is in flight (blocks Enter to prevent double-fire).
    pub upload_busy: bool,
    /// Last upload error/success string; rendered in the picker footer.
    pub upload_message: String,
}

/// One skill candidate discoverable for community upload.
#[derive(Debug, Clone)]
pub struct UploadCandidate {
    /// Absolute path to the skill directory (must contain SKILL.md).
    pub path: PathBuf,
    /// Skill name (defaults to dir basename, displayed + sent to /api/community/upload).
    pub name: String,
    /// Where this candidate came from. Used for the picker label so the user
    /// sees "USER" vs "PROJECT" instead of having to read the full path.
    pub source: UploadSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadSource {
    /// `~/.claude/skills/<name>/`
    ClaudeUser,
    /// `<cwd>/.claude/skills/<name>/` — project-local skill
    ProjectCwd,
}

impl UploadSource {
    pub fn short_label(&self) -> &'static str {
        match self {
            UploadSource::ClaudeUser => "USER",
            UploadSource::ProjectCwd => "PROJECT",
        }
    }
}

/// Hook-install status row for a single CLI target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookSlot {
    /// True when this CLI exposes a router-hook contract runai can manage.
    /// Today only `Claude` is supported. Other CLIs render as a placeholder
    /// row so the user sees the full per-CLI matrix.
    pub supported: bool,
    /// Hook currently installed for that target. Meaningful only when
    /// `supported`; ignored otherwise.
    pub installed: bool,
}

impl HookSlot {
    pub fn unsupported() -> Self {
        Self {
            supported: false,
            installed: false,
        }
    }
    pub fn supported(installed: bool) -> Self {
        Self {
            supported: true,
            installed,
        }
    }
}

/// One row from the team-mode `/api/community/list` response.
///
/// Mirrors the server `CommunitySkillJson` shape (server::community.rs); we
/// keep the fields we render and discard the rest. Decode is permissive
/// (`#[serde(default)]`) so a server adding new fields doesn't break the
/// TUI mid-version.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CommunitySkill {
    #[serde(default)]
    pub uploader_uid: String,
    #[serde(default)]
    pub uploader_username: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub installs_total: u64,
    #[serde(default)]
    pub created_at: i64,
}

pub struct FirstLaunchInfo {
    pub skills_found: usize,
    pub mcps_found: usize,
}

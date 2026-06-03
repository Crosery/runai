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
    Trash,
}

impl Tab {
    pub const ALL: &[Tab] = &[Tab::Skills, Tab::Mcps, Tab::Groups, Tab::Market, Tab::Trash];

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Skills => "Skills",
            Tab::Mcps => "MCPs",
            Tab::Groups => "Groups",
            Tab::Market => "Market",
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
}

pub struct FirstLaunchInfo {
    pub skills_found: usize,
    pub mcps_found: usize,
}

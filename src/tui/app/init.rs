use super::model::{App, CommunitySkill, FilterMode, HookSlot, InputMode, Tab};
use crate::core::cli_target::CliTarget;
use crate::core::manager::SkillManager;
use crate::core::market::{self, MarketSkill, SourceEntry};
use crate::core::resource::{Resource, TrashEntry};
use crate::tui::i18n::{Lang, T};
use std::collections::HashMap;

impl App {
    pub fn new(mgr: SkillManager) -> Self {
        let first_launch = mgr.is_first_launch();
        let sources = market::load_sources(mgr.paths().data_dir());
        Self {
            mgr,
            tab: Tab::Skills,
            theme_mode: super::super::theme::ThemeMode::Dark,
            lang: Lang::Zh,
            active_target: CliTarget::Claude,
            items: Vec::new(),
            trash_items: Vec::new(),
            groups: Vec::new(),
            selected: 0,
            search: String::new(),
            filter_mode: FilterMode::All,
            mode: if first_launch {
                InputMode::FirstLaunch(0)
            } else {
                InputMode::Normal
            },
            input_buf: String::new(),
            create_name: String::new(),
            group_pick_idx: 0,
            message: None,
            pending_delete: None,
            status: (0, 0, 0, 0),
            max_usage_count: 0,
            first_launch_info: None,
            scan_log: Vec::new(),
            detail_group_id: String::new(),
            detail_group_name: String::new(),
            detail_members: Vec::new(),
            detail_idx: 0,
            pick_items: Vec::new(),
            pick_idx: 0,
            pick_search: String::new(),
            pick_show_mcp: false,
            market_source_idx: 0,
            sources,
            source_pick_idx: 0,
            market_cache: HashMap::new(),
            market_rxs: HashMap::new(),
            market_fetching: std::collections::HashSet::new(),
            hook_panel_idx: 0,
            hook_status: initial_hook_status(),
            community_skills: Vec::new(),
            community_loading: false,
            community_error: String::new(),
            community_idx: 0,
            upload_candidates: Vec::new(),
            upload_idx: 0,
            upload_busy: false,
            upload_message: String::new(),
        }
    }

    pub fn t(&self) -> T {
        T::new(self.lang)
    }

    pub fn is_blocking_quit(&self) -> bool {
        self.mode != InputMode::Normal
    }

    pub fn visible_items(&self) -> Vec<&Resource> {
        let q = self.search.to_lowercase();
        self.items
            .iter()
            .filter(|r| {
                let search_ok = q.is_empty()
                    || r.name.to_lowercase().contains(&q)
                    || r.description.to_lowercase().contains(&q);
                let filter_ok = match self.filter_mode {
                    FilterMode::All => true,
                    FilterMode::Enabled => r.is_enabled_for(self.active_target),
                    FilterMode::Disabled => !r.is_enabled_for(self.active_target),
                };
                search_ok && filter_ok
            })
            .collect()
    }

    pub fn visible_groups(&self) -> Vec<&(String, String, usize, usize, String)> {
        let q = self.search.to_lowercase();
        self.groups
            .iter()
            .filter(|(id, name, _, _, _)| {
                q.is_empty() || name.to_lowercase().contains(&q) || id.to_lowercase().contains(&q)
            })
            .collect()
    }

    pub fn visible_trash(&self) -> Vec<&TrashEntry> {
        let q = self.search.to_lowercase();
        self.trash_items
            .iter()
            .filter(|entry| {
                q.is_empty()
                    || entry.name.to_lowercase().contains(&q)
                    || entry.resource_id.to_lowercase().contains(&q)
            })
            .collect()
    }

    pub fn visible_market(&self) -> Vec<&MarketSkill> {
        let q = self.search.to_lowercase();
        let enabled = self.enabled_sources();
        if let Some(src) = enabled.get(self.market_source_idx)
            && let Some(skills) = self.market_cache.get(&src.repo_id())
        {
            return skills
                .iter()
                .filter(|s| {
                    q.is_empty()
                        || s.name.to_lowercase().contains(&q)
                        || s.source_label.to_lowercase().contains(&q)
                })
                .collect();
        }
        Vec::new()
    }

    pub fn is_market_loading(&self) -> bool {
        !self.market_fetching.is_empty()
    }

    pub fn current_source_loading(&self) -> bool {
        self.current_source()
            .map(|s| self.market_fetching.contains(&s.repo_id()))
            .unwrap_or(false)
    }

    pub fn visible_count(&self) -> usize {
        match self.tab {
            Tab::Groups => self.visible_groups().len(),
            Tab::Market => self.visible_market().len(),
            Tab::Trash => self.visible_trash().len(),
            Tab::Hooks => CliTarget::ALL.len(),
            Tab::Community => self.visible_community().len(),
            _ => self.visible_items().len(),
        }
    }

    /// Enabled sources only.
    pub fn enabled_sources(&self) -> Vec<&SourceEntry> {
        self.sources.iter().filter(|s| s.enabled).collect()
    }

    /// Current source being viewed in Market (among enabled ones).
    pub fn current_source(&self) -> Option<&SourceEntry> {
        let enabled = self.enabled_sources();
        enabled.get(self.market_source_idx).copied()
    }

    pub fn visible_pick_items(&self) -> Vec<&Resource> {
        let q = self.pick_search.to_lowercase();
        self.pick_items
            .iter()
            .filter(|r| {
                q.is_empty()
                    || r.name.to_lowercase().contains(&q)
                    || r.description.to_lowercase().contains(&q)
            })
            .collect()
    }

    /// Search-filtered community feed for the Community tab.
    pub fn visible_community(&self) -> Vec<&CommunitySkill> {
        let q = self.search.to_lowercase();
        self.community_skills
            .iter()
            .filter(|c| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || c.uploader_username.to_lowercase().contains(&q)
                    || c.uploader_uid.to_lowercase().contains(&q)
            })
            .collect()
    }
}

/// Build the initial unsupported-by-default hook status map. `reload()`
/// overwrites Claude with the live filesystem snapshot on every Hooks-tab
/// activation. The three non-Claude rows stay `HookSlot::unsupported`.
fn initial_hook_status() -> HashMap<CliTarget, HookSlot> {
    let mut m = HashMap::new();
    for t in CliTarget::ALL {
        m.insert(*t, HookSlot::unsupported());
    }
    m
}

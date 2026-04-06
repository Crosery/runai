use crate::core::cli_target::CliTarget;
use crate::core::group::{Group, GroupKind};
use crate::core::manager::SkillManager;
use crate::core::market::{self, Market, MarketSkill, SourceEntry};
use crate::core::resource::Resource;
use crate::tui::i18n::{Lang, T};
use crossterm::event::{KeyCode, KeyEvent};
use std::collections::HashMap;
use std::sync::mpsc;

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
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Skills,
    Mcps,
    Groups,
    Market,
}

impl Tab {
    pub const ALL: &[Tab] = &[Tab::Skills, Tab::Mcps, Tab::Groups, Tab::Market];

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Skills => "Skills",
            Tab::Mcps => "MCPs",
            Tab::Groups => "Groups",
            Tab::Market => "Market",
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
}

pub struct App {
    pub mgr: SkillManager,
    pub tab: Tab,
    pub theme_mode: super::theme::ThemeMode,
    pub lang: Lang,
    pub active_target: CliTarget,
    pub items: Vec<Resource>,
    pub groups: Vec<(String, String, usize, usize)>,
    pub selected: usize,
    pub search: String,
    pub filter_mode: FilterMode,
    pub mode: InputMode,
    pub input_buf: String,
    pub create_name: String,
    pub group_pick_idx: usize,
    pub message: Option<String>,
    pub status: (usize, usize, usize, usize),
    pub first_launch_info: Option<FirstLaunchInfo>,
    pub scan_log: Vec<String>,
    /// Last known mtime of CLI config files for change detection
    config_mtimes: HashMap<String, std::time::SystemTime>,
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

impl App {
    pub fn new(mgr: SkillManager) -> Self {
        let first_launch = mgr.is_first_launch();
        let sources = market::load_sources(mgr.paths().data_dir());
        Self {
            mgr,
            tab: Tab::Skills,
            theme_mode: super::theme::ThemeMode::Dark,
            lang: Lang::Zh,
            active_target: CliTarget::Claude,
            items: Vec::new(),
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
            status: (0, 0, 0, 0),
            first_launch_info: None,
            scan_log: Vec::new(),
            config_mtimes: HashMap::new(),
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
        }
    }

    pub fn t(&self) -> T {
        T::new(self.lang)
    }

    /// Load from disk cache first (instant), then background refresh stale ones.
    pub fn prefetch_market(&mut self) {
        let data_dir = self.mgr.paths().data_dir().to_path_buf();
        for source in &self.sources {
            if !source.enabled {
                continue;
            }
            let rid = source.repo_id();
            if self.market_cache.contains_key(&rid) || self.market_fetching.contains(&rid) {
                continue;
            }
            // Try disk cache first
            if let Some(cached) = market::load_cache(&data_dir, source) {
                self.market_cache.insert(rid.clone(), cached);
                // Still refresh in background if stale
            }
            // Background fetch from GitHub API
            self.market_fetching.insert(rid.clone());
            let (tx, rx) = mpsc::channel();
            self.market_rxs.insert(rid, rx);
            let src = source.clone();
            let dd = data_dir.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                let result = rt.block_on(Market::fetch(&src));
                // Save to disk cache on success, save plugin marker if detected
                if let Ok(ref extract) = result {
                    let _ = market::save_cache(&dd, &src, &extract.skills);
                    if extract.plugin_detected {
                        market::save_plugin_marker(&dd, &src);
                    }
                }
                let _ = tx.send(result.map(|e| e.skills).map_err(|e| e.to_string()));
            });
        }
    }

    /// Check if any CLI config file changed since last check. If so, sync and reload.
    /// Cheap: only stat() calls, no file reads unless mtime changed.
    pub fn poll_config_changes(&mut self) {
        let home = dirs::home_dir().unwrap_or_default();
        let configs = [
            home.join(".claude.json"),
            home.join(".gemini/settings.json"),
            home.join(".codex/settings.json"),
            home.join(".opencode/settings.json"),
        ];
        let mut changed = false;
        for path in &configs {
            let key = path.to_string_lossy().to_string();
            let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            if let Some(mt) = mtime {
                let prev = self.config_mtimes.get(&key);
                if prev != Some(&mt) {
                    self.config_mtimes.insert(key, mt);
                    changed = true;
                }
            }
        }
        // Also watch skills directories
        for target in crate::core::cli_target::CliTarget::ALL {
            let skills_dir = target.skills_dir();
            if skills_dir.exists() {
                let key = skills_dir.to_string_lossy().to_string();
                let mtime = std::fs::metadata(&skills_dir)
                    .and_then(|m| m.modified())
                    .ok();
                if let Some(mt) = mtime {
                    let prev = self.config_mtimes.get(&key);
                    if prev != Some(&mt) {
                        self.config_mtimes.insert(key, mt);
                        changed = true;
                    }
                }
            }
        }

        // Watch mcp-configs directory
        let mcp_configs = home.join(".claude").join("mcp-configs");
        if mcp_configs.exists() {
            let key = mcp_configs.to_string_lossy().to_string();
            let mtime = std::fs::metadata(&mcp_configs)
                .and_then(|m| m.modified())
                .ok();
            if let Some(mt) = mtime {
                let prev = self.config_mtimes.get(&key);
                if prev != Some(&mt) {
                    self.config_mtimes.insert(key, mt);
                    changed = true;
                }
            }
        }

        if changed {
            self.reload();
        }
    }

    pub fn reload(&mut self) {
        let kind_filter = match self.tab {
            Tab::Skills => Some(crate::core::resource::ResourceKind::Skill),
            Tab::Mcps => Some(crate::core::resource::ResourceKind::Mcp),
            Tab::Groups | Tab::Market => None,
        };

        self.items = self
            .mgr
            .list_resources(kind_filter, None)
            .unwrap_or_default();

        self.groups = self
            .mgr
            .list_groups()
            .unwrap_or_default()
            .into_iter()
            .map(|(id, g)| {
                let members = self.mgr.get_group_members(&id).unwrap_or_default();
                let enabled = members
                    .iter()
                    .filter(|m| m.is_enabled_for(self.active_target))
                    .count();
                (id, g.name, members.len(), enabled)
            })
            .collect();

        let (es, em) = self.mgr.status(self.active_target).unwrap_or((0, 0));
        let (ts, tm) = self.mgr.resource_count();
        self.status = (es, ts, em, tm);

        if self.selected >= self.visible_count() && self.visible_count() > 0 {
            self.selected = self.visible_count() - 1;
        }
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

    pub fn visible_groups(&self) -> Vec<&(String, String, usize, usize)> {
        let q = self.search.to_lowercase();
        self.groups
            .iter()
            .filter(|(id, name, _, _)| {
                q.is_empty() || name.to_lowercase().contains(&q) || id.to_lowercase().contains(&q)
            })
            .collect()
    }

    pub fn visible_market(&self) -> Vec<&MarketSkill> {
        let q = self.search.to_lowercase();
        let enabled = self.enabled_sources();
        if let Some(src) = enabled.get(self.market_source_idx) {
            if let Some(skills) = self.market_cache.get(&src.repo_id()) {
                return skills
                    .iter()
                    .filter(|s| {
                        q.is_empty()
                            || s.name.to_lowercase().contains(&q)
                            || s.source_label.to_lowercase().contains(&q)
                    })
                    .collect();
            }
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

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.message = None;

        match self.mode {
            InputMode::Search => self.handle_search_key(key),
            InputMode::CreateGroup(step) => self.handle_create_group_key(key, step),
            InputMode::AddToGroup => self.handle_add_to_group_key(key),
            InputMode::FirstLaunch(step) => self.handle_first_launch_key(key, step),
            InputMode::Install => self.handle_install_key(key),
            InputMode::AddSource => self.handle_add_source_key(key),
            InputMode::SourceManager => self.handle_source_manager_key(key),
            InputMode::GroupDetail => self.handle_group_detail_key(key),
            InputMode::PickSkillForGroup => self.handle_pick_skill_key(key),
            InputMode::Help => {
                self.mode = InputMode::Normal;
            }
            InputMode::RenameGroup => self.handle_rename_group_key(key),
            InputMode::Normal => self.handle_normal_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            // Navigation
            KeyCode::Char('j') | KeyCode::Down => {
                if self.visible_count() > 0 {
                    self.selected = (self.selected + 1).min(self.visible_count() - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            KeyCode::Char('g') => self.selected = 0,
            KeyCode::Char('G') => {
                if self.visible_count() > 0 {
                    self.selected = self.visible_count() - 1;
                }
            }

            // Tab switching
            KeyCode::Char('H') | KeyCode::BackTab => {
                let idx = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.tab = Tab::ALL[(idx + Tab::ALL.len() - 1) % Tab::ALL.len()];
                self.selected = 0;
                self.search.clear();
                self.reload();
            }
            KeyCode::Char('L') | KeyCode::Tab => {
                let idx = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
                self.tab = Tab::ALL[(idx + 1) % Tab::ALL.len()];
                self.selected = 0;
                self.search.clear();
                self.reload();
            }

            // Market: switch enabled source with [ ]
            KeyCode::Char('[') if self.tab == Tab::Market => {
                let total = self.enabled_sources().len();
                if total > 0 {
                    self.market_source_idx = if self.market_source_idx > 0 {
                        self.market_source_idx - 1
                    } else {
                        total - 1
                    };
                    self.selected = 0;
                }
            }
            KeyCode::Char(']') if self.tab == Tab::Market => {
                let total = self.enabled_sources().len();
                if total > 0 {
                    self.market_source_idx = (self.market_source_idx + 1) % total;
                    self.selected = 0;
                }
            }

            // Market: Enter to install
            KeyCode::Enter if self.tab == Tab::Market => {
                self.install_from_market();
            }

            // Groups: Enter opens group detail
            KeyCode::Enter if self.tab == Tab::Groups => {
                self.open_group_detail();
            }

            // Market: 's' to open source manager
            KeyCode::Char('s') if self.tab == Tab::Market => {
                self.mode = InputMode::SourceManager;
                self.source_pick_idx = 0;
            }

            // Market: Enter to install selected skill
            KeyCode::Enter if self.tab == Tab::Market => {
                self.install_market_selected();
            }

            // Toggle enable/disable
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle_selected(),

            // Search
            KeyCode::Char('/') => {
                self.mode = InputMode::Search;
                self.search.clear();
            }

            // Switch CLI target
            KeyCode::Char('1') => {
                self.active_target = CliTarget::Claude;
                self.reload();
            }
            KeyCode::Char('2') => {
                self.active_target = CliTarget::Codex;
                self.reload();
            }
            KeyCode::Char('3') => {
                self.active_target = CliTarget::Gemini;
                self.reload();
            }
            KeyCode::Char('4') => {
                self.active_target = CliTarget::OpenCode;
                self.reload();
            }

            // Scan
            KeyCode::Char('s') => {
                let _ = self.mgr.scan();
                self.reload();
                self.message = Some(self.t().msg_scan_done().to_string());
            }

            // Language toggle
            KeyCode::Char('l') if !matches!(self.tab, Tab::Groups) => {
                self.lang = self.lang.toggle();
                self.message = Some(self.t().msg_lang_switched().to_string());
            }

            // Filter mode toggle (Skills/MCPs tabs only)
            KeyCode::Char('f') if self.tab == Tab::Skills || self.tab == Tab::Mcps => {
                self.filter_mode = self.filter_mode.next();
                self.selected = 0;
                let label = match self.filter_mode {
                    FilterMode::All => self.t().filter_all(),
                    FilterMode::Enabled => self.t().filter_enabled(),
                    FilterMode::Disabled => self.t().filter_disabled(),
                };
                self.message = Some(self.t().msg_filter(label));
            }

            // Theme toggle
            KeyCode::Char('t') => {
                self.theme_mode = self.theme_mode.toggle();
                self.message = Some(self.t().msg_theme(self.theme_mode.label()));
            }

            // Help
            KeyCode::Char('?') => {
                self.mode = InputMode::Help;
            }

            // Create group
            KeyCode::Char('c') => {
                self.mode = InputMode::CreateGroup(0);
                self.input_buf.clear();
                self.create_name.clear();
            }

            // Add to group
            KeyCode::Char('a') if self.tab != Tab::Groups && self.tab != Tab::Market => {
                if !self.groups.is_empty() && self.visible_count() > 0 {
                    self.mode = InputMode::AddToGroup;
                    self.group_pick_idx = 0;
                } else if self.groups.is_empty() {
                    self.message = Some("No groups yet. Press 'c' to create one.".into());
                }
            }

            // Install from GitHub
            KeyCode::Char('i') => {
                self.mode = InputMode::Install;
                self.input_buf.clear();
            }

            // Rename group
            KeyCode::Char('r') if self.tab == Tab::Groups => {
                let visible = self.visible_groups();
                if let Some((_, name, _, _)) = visible.get(self.selected) {
                    self.input_buf = name.clone();
                    self.mode = InputMode::RenameGroup;
                }
            }

            // Delete group
            KeyCode::Char('d') if self.tab == Tab::Groups => {
                self.delete_selected_group();
            }

            // Delete skill/mcp
            KeyCode::Char('d') if self.tab == Tab::Skills || self.tab == Tab::Mcps => {
                self.delete_selected_resource();
            }

            _ => {}
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.search.clear();
                self.selected = 0;
            }
            KeyCode::Enter => self.mode = InputMode::Normal,
            KeyCode::Backspace => {
                self.search.pop();
                self.selected = 0;
            }
            KeyCode::Char(c) => {
                self.search.push(c);
                self.selected = 0;
            }
            _ => {}
        }
    }

    fn handle_create_group_key(&mut self, key: KeyEvent, step: u8) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.input_buf.clear();
            }
            KeyCode::Enter => {
                if step == 0 {
                    if self.input_buf.trim().is_empty() {
                        self.mode = InputMode::Normal;
                        return;
                    }
                    self.create_name = self.input_buf.trim().to_string();
                    self.input_buf.clear();
                    self.mode = InputMode::CreateGroup(1);
                } else {
                    let name = self.create_name.clone();
                    let desc = self.input_buf.trim().to_string();
                    let id = name
                        .to_lowercase()
                        .chars()
                        .map(|c| if c.is_alphanumeric() { c } else { '-' })
                        .collect::<String>()
                        .split('-')
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("-");
                    let group = Group {
                        name,
                        description: desc,
                        kind: GroupKind::Custom,
                        auto_enable: false,
                        members: vec![],
                    };
                    match self.mgr.create_group(&id, &group) {
                        Ok(_) => self.message = Some(format!("Group '{id}' created")),
                        Err(e) => self.message = Some(format!("Error: {e}")),
                    }
                    self.input_buf.clear();
                    self.mode = InputMode::Normal;
                    self.tab = Tab::Groups;
                    self.reload();
                }
            }
            KeyCode::Backspace => {
                self.input_buf.pop();
            }
            KeyCode::Char(c) => self.input_buf.push(c),
            _ => {}
        }
    }

    fn handle_add_to_group_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = InputMode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                if self.group_pick_idx + 1 < self.groups.len() {
                    self.group_pick_idx += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.group_pick_idx > 0 {
                    self.group_pick_idx -= 1;
                }
            }
            KeyCode::Enter => {
                if let Some((group_id, group_name, _, _)) = self.groups.get(self.group_pick_idx) {
                    let resource_id = match self.tab {
                        Tab::Groups | Tab::Market => {
                            self.mode = InputMode::Normal;
                            return;
                        }
                        _ => {
                            let visible = self.visible_items();
                            match visible.get(self.selected) {
                                Some(r) => r.id.clone(),
                                None => {
                                    self.mode = InputMode::Normal;
                                    return;
                                }
                            }
                        }
                    };
                    let gid = group_id.clone();
                    let gname = group_name.clone();
                    match self.mgr.db().add_group_member(&gid, &resource_id) {
                        Ok(_) => self.message = Some(format!("Added to '{gname}'")),
                        Err(e) => self.message = Some(format!("Error: {e}")),
                    }
                    self.mode = InputMode::Normal;
                    self.reload();
                }
            }
            _ => {}
        }
    }

    fn install_market_selected(&mut self) {
        let visible = self.visible_market();
        if let Some(skill) = visible.get(self.selected) {
            if skill.installed {
                self.message = Some(format!("'{}' is already installed", skill.name));
                return;
            }
            let name = skill.name.clone();
            let source_repo = skill.source_repo.clone();
            self.message = Some(format!("Installing '{name}'..."));

            // Try market install
            let data_dir = self.mgr.paths().data_dir().to_path_buf();
            let sources = market::load_sources(&data_dir);
            if let Some(found) = market::find_skill_in_sources(&data_dir, &sources, &name, None) {
                let paths = self.mgr.paths().clone();
                let rt = tokio::runtime::Runtime::new().unwrap();
                match rt.block_on(Market::install_single(&found, &paths)) {
                    Ok(_) => {
                        let _ = self.mgr.register_local_skill(&name);
                        if let Some(id) = self.mgr.find_resource_id(&name) {
                            let _ = self.mgr.enable_resource(&id, self.active_target, None);
                        }
                        self.message = Some(format!("Installed '{name}' from {source_repo}"));
                        self.reload();
                    }
                    Err(e) => {
                        self.message = Some(format!("Install failed: {e}"));
                    }
                }
            } else {
                self.message = Some(format!("'{name}' not found in market sources"));
            }
        }
    }

    fn toggle_selected(&mut self) {
        match self.tab {
            Tab::Groups => {
                let visible = self.visible_groups();
                if let Some((id, _, total, enabled)) = visible.get(self.selected) {
                    let enable = *enabled == 0 || *enabled < *total;
                    let id = id.clone();
                    if enable {
                        let _ = self.mgr.enable_group(&id, self.active_target, None);
                    } else {
                        let _ = self.mgr.disable_group(&id, self.active_target, None);
                    }
                    self.reload();
                }
            }
            _ => {
                let visible = self.visible_items();
                if let Some(r) = visible.get(self.selected) {
                    let id = r.id.clone();
                    let enabled = r.is_enabled_for(self.active_target);
                    if enabled {
                        let _ = self.mgr.disable_resource(&id, self.active_target, None);
                    } else {
                        let _ = self.mgr.enable_resource(&id, self.active_target, None);
                    }
                    self.reload();
                }
            }
        }
    }

    fn delete_selected_resource(&mut self) {
        let visible = self.visible_items();
        let entry = visible
            .get(self.selected)
            .map(|r| (r.id.clone(), r.name.clone(), r.directory.clone()));
        if let Some((id, name, dir)) = entry {
            // Remove symlinks from all CLIs
            for target in CliTarget::ALL {
                let link = target.skills_dir().join(&name);
                if link.is_symlink() {
                    let _ = std::fs::remove_file(&link);
                }
            }
            // Remove managed directory
            if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            // Remove from DB
            let _ = self.mgr.db().delete_resource(&id);
            self.message = Some(format!("Deleted '{name}'"));
            self.reload();
        }
    }

    fn delete_selected_group(&mut self) {
        let visible = self.visible_groups();
        if let Some((id, name, _, _)) = visible.get(self.selected) {
            let path = self.mgr.paths().groups_dir().join(format!("{id}.toml"));
            if path.exists() {
                let _ = std::fs::remove_file(&path);
            }
            self.message = Some(format!("Group '{name}' deleted"));
            self.reload();
        }
    }

    fn handle_rename_group_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.input_buf.clear();
            }
            KeyCode::Enter => {
                let new_name = self.input_buf.trim().to_string();
                if new_name.is_empty() {
                    self.mode = InputMode::Normal;
                    return;
                }
                let visible = self.visible_groups();
                if let Some((id, _, _, _)) = visible.get(self.selected) {
                    let id = id.clone();
                    match self.mgr.rename_group(&id, &new_name) {
                        Ok(_) => self.message = Some(format!("Renamed to '{new_name}'")),
                        Err(e) => self.message = Some(format!("Error: {e}")),
                    }
                }
                self.input_buf.clear();
                self.mode = InputMode::Normal;
                self.reload();
            }
            KeyCode::Backspace => {
                self.input_buf.pop();
            }
            KeyCode::Char(c) => self.input_buf.push(c),
            _ => {}
        }
    }

    fn handle_install_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.input_buf.clear();
            }
            KeyCode::Enter => {
                let source = self.input_buf.trim().to_string();
                if source.is_empty() {
                    self.mode = InputMode::Normal;
                    return;
                }
                self.input_buf.clear();
                self.mode = InputMode::Normal;

                match crate::core::installer::Installer::parse_github_source(&source) {
                    Ok((owner, repo, branch)) => {
                        self.message = Some(format!("Installing {owner}/{repo}@{branch}..."));
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        match rt.block_on(crate::core::installer::Installer::install_from_github(
                            &owner,
                            &repo,
                            &branch,
                            self.mgr.paths(),
                        )) {
                            Ok(results) => {
                                let mut registered = 0;
                                for r in &results {
                                    if self.mgr.register_local_skill(&r.name).is_ok() {
                                        registered += 1;
                                    }
                                }
                                self.message = Some(format!(
                                    "Installed {} skills from {owner}/{repo}",
                                    registered
                                ));
                                self.reload();
                            }
                            Err(e) => self.message = Some(format!("Install failed: {e}")),
                        }
                    }
                    Err(e) => self.message = Some(format!("Invalid source: {e}")),
                }
            }
            KeyCode::Backspace => {
                self.input_buf.pop();
            }
            KeyCode::Char(c) => self.input_buf.push(c),
            _ => {}
        }
    }

    fn handle_first_launch_key(&mut self, key: KeyEvent, step: u8) {
        match step {
            0 => match key.code {
                KeyCode::Enter => {
                    self.mode = InputMode::FirstLaunch(1);
                    self.scan_log.clear();
                    self.scan_log.push("Starting scan...".into());
                }
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.mode = InputMode::Normal;
                    self.reload();
                }
                _ => {}
            },
            1 => {} // scanning
            2 => {
                self.mode = InputMode::Normal;
                self.reload();
                self.prefetch_market();
            }
            _ => {
                self.mode = InputMode::Normal;
                self.reload();
            }
        }
    }

    pub fn do_first_launch_scan(&mut self) {
        self.scan_log.clear();

        self.scan_log.push("Scanning skill directories...".into());
        for t in CliTarget::ALL {
            for dir in &[t.skills_dir(), t.agents_skills_dir()] {
                if dir.exists() {
                    self.scan_log
                        .push(format!("  ✓ {} — {}", t.name(), dir.display()));
                }
            }
        }

        let scan_result = self.mgr.scan().unwrap_or_default();
        self.scan_log.push(format!(
            "  Found {} skills ({} new, {} existing)",
            scan_result.adopted + scan_result.skipped,
            scan_result.adopted,
            scan_result.skipped,
        ));
        if !scan_result.errors.is_empty() {
            self.scan_log.push(format!(
                "  ⚠ {} errors (see ~/.runai/scan.log)",
                scan_result.errors.len()
            ));
            let log_path = self.mgr.paths().data_dir().join("scan.log");
            let log_content = format!(
                "=== Scan Log {} ===\n\n{}\n",
                chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
                scan_result.errors.join("\n"),
            );
            let _ = std::fs::write(&log_path, log_content);
        }

        self.scan_log.push("Discovering MCP servers...".into());
        let home = dirs::home_dir().unwrap_or_default();
        let mcp_entries = crate::core::mcp_discovery::McpDiscovery::discover_all(&home);
        self.scan_log
            .push(format!("  Found {} MCP servers", mcp_entries.len()));
        for entry in &mcp_entries {
            let status = if entry.disabled {
                "disabled"
            } else {
                "enabled"
            };
            self.scan_log
                .push(format!("    · {} ({})", entry.name, status));
        }

        self.scan_log
            .push("Registering MCP server to all CLIs...".into());
        let reg_result = crate::core::mcp_register::McpRegister::register_all(&home);
        for name in &reg_result.registered {
            self.scan_log.push(format!("  ✓ Registered to {name}"));
        }
        for name in &reg_result.skipped {
            self.scan_log
                .push(format!("  · {name} (already registered)"));
        }
        for err in &reg_result.errors {
            self.scan_log.push(format!("  ⚠ {err}"));
        }

        self.scan_log.push("Done!".into());

        self.first_launch_info = Some(FirstLaunchInfo {
            skills_found: scan_result.adopted + scan_result.skipped,
            mcps_found: mcp_entries.len(),
        });
    }

    // ── Source Manager ──

    // ── Group Detail ──

    fn open_group_detail(&mut self) {
        let entry = self
            .visible_groups()
            .get(self.selected)
            .map(|(id, name, _, _)| (id.clone(), name.clone()));
        if let Some((id, name)) = entry {
            self.detail_group_id = id;
            self.detail_group_name = name;
            self.reload_group_detail();
            self.detail_idx = 0;
            self.mode = InputMode::GroupDetail;
        }
    }

    fn reload_group_detail(&mut self) {
        self.detail_members = self
            .mgr
            .get_group_members(&self.detail_group_id)
            .unwrap_or_default();
    }

    fn handle_group_detail_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::Normal;
                self.reload();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.detail_members.is_empty()
                    && self.detail_idx + 1 < self.detail_members.len()
                {
                    self.detail_idx += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.detail_idx > 0 {
                    self.detail_idx -= 1;
                }
            }
            // Toggle enable/disable selected member
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(r) = self.detail_members.get(self.detail_idx) {
                    let id = r.id.clone();
                    let enabled = r.is_enabled_for(self.active_target);
                    if enabled {
                        let _ = self.mgr.disable_resource(&id, self.active_target, None);
                    } else {
                        let _ = self.mgr.enable_resource(&id, self.active_target, None);
                    }
                    self.reload_group_detail();
                }
            }
            // Remove member from group
            KeyCode::Char('d') => {
                if let Some(r) = self.detail_members.get(self.detail_idx) {
                    let rid = r.id.clone();
                    let gid = self.detail_group_id.clone();
                    let _ = self.mgr.db().remove_group_member(&gid, &rid);
                    self.reload_group_detail();
                    if self.detail_idx >= self.detail_members.len()
                        && !self.detail_members.is_empty()
                    {
                        self.detail_idx = self.detail_members.len() - 1;
                    }
                }
            }
            // Add skill/mcp to this group
            KeyCode::Char('a') => {
                self.pick_show_mcp = false;
                self.load_pick_items();
                self.pick_idx = 0;
                self.pick_search.clear();
                self.mode = InputMode::PickSkillForGroup;
            }
            // Switch CLI target
            KeyCode::Char('1') => {
                self.active_target = CliTarget::Claude;
                self.reload_group_detail();
            }
            KeyCode::Char('2') => {
                self.active_target = CliTarget::Codex;
                self.reload_group_detail();
            }
            KeyCode::Char('3') => {
                self.active_target = CliTarget::Gemini;
                self.reload_group_detail();
            }
            KeyCode::Char('4') => {
                self.active_target = CliTarget::OpenCode;
                self.reload_group_detail();
            }
            _ => {}
        }
    }

    fn load_pick_items(&mut self) {
        let member_ids: std::collections::HashSet<String> =
            self.detail_members.iter().map(|r| r.id.clone()).collect();
        let kind = if self.pick_show_mcp {
            Some(crate::core::resource::ResourceKind::Mcp)
        } else {
            Some(crate::core::resource::ResourceKind::Skill)
        };
        self.pick_items = self
            .mgr
            .list_resources(kind, None)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| !member_ids.contains(&r.id))
            .collect();
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

    fn handle_pick_skill_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = InputMode::GroupDetail;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let count = self.visible_pick_items().len();
                if count > 0 && self.pick_idx + 1 < count {
                    self.pick_idx += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.pick_idx > 0 {
                    self.pick_idx -= 1;
                }
            }
            KeyCode::Enter => {
                let rid = self
                    .visible_pick_items()
                    .get(self.pick_idx)
                    .map(|r| (r.id.clone(), r.name.clone()));
                if let Some((rid, rname)) = rid {
                    let gid = self.detail_group_id.clone();
                    let _ = self.mgr.db().add_group_member(&gid, &rid);
                    self.message = Some(format!("Added '{rname}'"));
                    self.pick_items.retain(|r| r.id != rid);
                    let count = self.visible_pick_items().len();
                    if self.pick_idx >= count && count > 0 {
                        self.pick_idx = count - 1;
                    }
                    self.reload_group_detail();
                }
            }
            // TAB to switch between Skills and MCPs
            KeyCode::Tab => {
                self.pick_show_mcp = !self.pick_show_mcp;
                self.load_pick_items();
                self.pick_idx = 0;
            }
            KeyCode::Backspace => {
                self.pick_search.pop();
                self.pick_idx = 0;
            }
            KeyCode::Char(c) => {
                self.pick_search.push(c);
                self.pick_idx = 0;
            }
            _ => {}
        }
    }

    // ── Source Manager ──

    fn handle_source_manager_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('s') => {
                self.mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.source_pick_idx + 1 < self.sources.len() {
                    self.source_pick_idx += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.source_pick_idx > 0 {
                    self.source_pick_idx -= 1;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let idx = self.source_pick_idx;
                if idx < self.sources.len() {
                    self.sources[idx].enabled = !self.sources[idx].enabled;
                    let _ = market::save_sources(self.mgr.paths().data_dir(), &self.sources);
                    let rid = self.sources[idx].repo_id();
                    if self.sources[idx].enabled {
                        self.prefetch_market();
                    } else {
                        self.market_cache.remove(&rid);
                        self.market_fetching.remove(&rid);
                    }
                }
            }
            KeyCode::Char('a') => {
                // Switch to AddSource input
                self.mode = InputMode::AddSource;
                self.input_buf.clear();
            }
            KeyCode::Char('d') => {
                // Delete user-added source
                if let Some(src) = self.sources.get(self.source_pick_idx) {
                    if src.builtin {
                        self.message =
                            Some("Can't delete built-in source (disable it instead)".into());
                    } else {
                        let label = src.label.clone();
                        let rid = src.repo_id();
                        self.sources.remove(self.source_pick_idx);
                        let _ = market::save_sources(self.mgr.paths().data_dir(), &self.sources);
                        if self.source_pick_idx >= self.sources.len() && !self.sources.is_empty() {
                            self.source_pick_idx = self.sources.len() - 1;
                        }
                        self.market_cache.remove(&rid);
                        self.message = Some(format!("Removed '{label}'"));
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_add_source_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = if self.tab == Tab::Market {
                    InputMode::SourceManager
                } else {
                    InputMode::Normal
                };
                self.input_buf.clear();
            }
            KeyCode::Enter => {
                let input = self.input_buf.trim().to_string();
                self.input_buf.clear();

                if input.is_empty() {
                    self.mode = InputMode::SourceManager;
                    return;
                }

                match SourceEntry::from_input(&input) {
                    Ok(source) => {
                        self.sources.push(source);
                        let _ = market::save_sources(self.mgr.paths().data_dir(), &self.sources);
                        self.source_pick_idx = self.sources.len() - 1;
                        self.prefetch_market(); // fetch new source
                        self.message = Some(format!("Added source: {input}"));
                    }
                    Err(e) => {
                        self.message = Some(format!("Invalid: {e}"));
                    }
                }
                self.mode = InputMode::SourceManager;
            }
            KeyCode::Backspace => {
                self.input_buf.pop();
            }
            KeyCode::Char(c) => self.input_buf.push(c),
            _ => {}
        }
    }

    // ── Market ──

    /// Poll all background market fetches, collecting results into cache.
    pub fn poll_market(&mut self) {
        let installed: Option<Vec<String>> = if !self.market_rxs.is_empty() {
            Some(
                self.mgr
                    .list_resources(None, None)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|r| r.name)
                    .collect(),
            )
        } else {
            None
        };

        let keys: Vec<String> = self.market_rxs.keys().cloned().collect();
        for rid in keys {
            let rx = match self.market_rxs.get(&rid) {
                Some(rx) => rx,
                None => continue,
            };
            match rx.try_recv() {
                Ok(Ok(mut skills)) => {
                    if let Some(ref installed) = installed {
                        Market::mark_installed(&mut skills, installed);
                    }
                    self.market_cache.insert(rid.clone(), skills);
                    self.market_fetching.remove(&rid);
                    self.market_rxs.remove(&rid);
                }
                Ok(Err(_e)) => {
                    self.market_cache.insert(rid.clone(), Vec::new());
                    self.market_fetching.remove(&rid);
                    self.market_rxs.remove(&rid);
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.market_fetching.remove(&rid);
                    self.market_rxs.remove(&rid);
                }
                Err(mpsc::TryRecvError::Empty) => {} // still loading
            }
        }
    }

    fn install_from_market(&mut self) {
        let visible = self.visible_market();
        let skill = match visible.get(self.selected) {
            Some(s) => (*s).clone(),
            None => return,
        };

        if skill.installed {
            self.message = Some(format!("'{}' already installed", skill.name));
            return;
        }

        self.message = Some(format!("Installing '{}'...", skill.name));

        // Download only the SKILL.md for this one skill
        let rt = tokio::runtime::Runtime::new().unwrap();
        match rt.block_on(Market::install_single(&skill, self.mgr.paths())) {
            Ok(_) => {
                let _ = self.mgr.register_local_skill(&skill.name);
                self.message = Some(format!("Installed '{}'", skill.name));
                // Mark installed in cache
                let rid = skill.source_repo.clone();
                if let Some(cached) = self.market_cache.get_mut(&rid) {
                    for item in cached.iter_mut() {
                        if item.name == skill.name {
                            item.installed = true;
                        }
                    }
                }
            }
            Err(e) => {
                self.message = Some(format!("Install failed: {e}"));
            }
        }
    }
}

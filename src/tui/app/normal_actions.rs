use super::model::{App, FilterMode, InputMode, Tab};
use crate::core::cli_target::CliTarget;
use crossterm::event::{KeyCode, KeyEvent};

impl App {
    pub(super) fn handle_normal_key(&mut self, key: KeyEvent) {
        // Hooks tab owns its own key grammar — let it consume the event
        // before the default skill/mcp/list keymap below kicks in.
        if self.tab == Tab::Hooks && self.handle_hooks_key(key) {
            return;
        }
        // Community tab: 'u' opens the upload picker (scan + select + upload).
        // The picker itself runs in InputMode::CommunityUploadPicker so its
        // key handler lives in keybindings.rs alongside the other overlay handlers.
        if self.tab == Tab::Community && matches!(key.code, KeyCode::Char('u')) {
            self.scan_upload_candidates();
            self.mode = InputMode::CommunityUploadPicker;
            return;
        }
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
                if let Some((_, name, _, _, _)) = visible.get(self.selected) {
                    self.input_buf = name.clone();
                    self.mode = InputMode::RenameGroup;
                }
            }

            // Restore from trash
            KeyCode::Char('r') if self.tab == Tab::Trash => {
                self.restore_selected_trash();
            }

            // Delete group
            KeyCode::Char('d') if self.tab == Tab::Groups => {
                self.confirm_delete_selected_group();
            }

            // Delete skill/mcp
            KeyCode::Char('d') if self.tab == Tab::Skills || self.tab == Tab::Mcps => {
                self.confirm_delete_selected_resource();
            }

            // Purge from trash
            KeyCode::Char('D') if self.tab == Tab::Trash => {
                self.purge_selected_trash();
            }

            _ => {}
        }
    }

    fn toggle_selected(&mut self) {
        match self.tab {
            Tab::Groups => {
                let visible = self.visible_groups();
                if let Some((id, _, total, enabled, _)) = visible.get(self.selected) {
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
            Tab::Skills | Tab::Mcps => {
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
            Tab::Market | Tab::Trash | Tab::Hooks | Tab::Community => {}
        }
    }
}

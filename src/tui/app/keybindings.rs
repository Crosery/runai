use super::model::{App, InputMode, PendingDelete, Tab};
use crate::core::group::{Group, GroupKind};
use crate::core::market::{self, SourceEntry};
use crossterm::event::{KeyCode, KeyEvent};

impl App {
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
            InputMode::ConfirmDelete => self.handle_confirm_delete_key(key),
            InputMode::Normal => self.handle_normal_key(key),
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
                if let Some((group_id, group_name, _, _, _)) = self.groups.get(self.group_pick_idx)
                {
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

    fn handle_confirm_delete_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = self
                    .pending_delete
                    .as_ref()
                    .map(PendingDelete::return_mode)
                    .unwrap_or(InputMode::Normal);
                self.pending_delete = None;
            }
            KeyCode::Enter => {
                if let Some(pending) = self.pending_delete.take() {
                    let next_mode = pending.return_mode();
                    self.mode = next_mode;
                    match pending {
                        PendingDelete::Resource { id, name, .. } => {
                            self.mode = InputMode::Normal;
                            self.delete_pending_resource(id, name);
                        }
                        PendingDelete::Group { id, name } => {
                            self.mode = InputMode::Normal;
                            self.delete_pending_group(id, name);
                        }
                        PendingDelete::GroupMember {
                            group_id,
                            resource_id,
                            resource_name,
                            ..
                        } => {
                            let _ = self.mgr.db().remove_group_member(&group_id, &resource_id);
                            self.reload_group_detail();
                            if self.detail_idx >= self.detail_members.len()
                                && !self.detail_members.is_empty()
                            {
                                self.detail_idx = self.detail_members.len() - 1;
                            }
                            self.message = Some(format!("Removed '{resource_name}' from group"));
                        }
                        PendingDelete::Source { repo_id, label } => {
                            if let Some(idx) =
                                self.sources.iter().position(|src| src.repo_id() == repo_id)
                            {
                                self.sources.remove(idx);
                                let _ = market::save_sources(
                                    self.mgr.paths().data_dir(),
                                    &self.sources,
                                );
                                if self.source_pick_idx >= self.sources.len()
                                    && !self.sources.is_empty()
                                {
                                    self.source_pick_idx = self.sources.len() - 1;
                                }
                                self.market_cache.remove(&repo_id);
                                self.market_fetching.remove(&repo_id);
                                self.message = Some(format!("Removed '{label}'"));
                            }
                        }
                    }
                } else {
                    self.mode = InputMode::Normal;
                }
            }
            _ => {}
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
                if let Some((id, _, _, _, _)) = visible.get(self.selected) {
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
                        self.message = Some(self.t().cant_delete_builtin().into());
                    } else {
                        self.pending_delete = Some(PendingDelete::Source {
                            repo_id: src.repo_id(),
                            label: src.label.clone(),
                        });
                        self.mode = InputMode::ConfirmDelete;
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
}

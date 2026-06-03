use super::model::{App, InputMode, PendingDelete};
use crate::core::cli_target::CliTarget;
use crossterm::event::{KeyCode, KeyEvent};

impl App {
    pub(super) fn open_group_detail(&mut self) {
        let entry = self
            .visible_groups()
            .get(self.selected)
            .map(|(id, name, _, _, _)| (id.clone(), name.clone()));
        if let Some((id, name)) = entry {
            self.detail_group_id = id;
            self.detail_group_name = name;
            self.reload_group_detail();
            self.detail_idx = 0;
            self.mode = InputMode::GroupDetail;
        }
    }

    pub(super) fn reload_group_detail(&mut self) {
        self.detail_members = self
            .mgr
            .get_group_members(&self.detail_group_id)
            .unwrap_or_default();
    }

    pub(super) fn handle_group_detail_key(&mut self, key: KeyEvent) {
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
                    self.pending_delete = Some(PendingDelete::GroupMember {
                        group_id: self.detail_group_id.clone(),
                        group_name: self.detail_group_name.clone(),
                        resource_id: r.id.clone(),
                        resource_name: r.name.clone(),
                    });
                    self.mode = InputMode::ConfirmDelete;
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

    pub(super) fn handle_pick_skill_key(&mut self, key: KeyEvent) {
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
}

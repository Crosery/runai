use super::model::{App, InputMode, PendingDelete};

impl App {
    pub(super) fn confirm_delete_selected_resource(&mut self) {
        let visible = self.visible_items();
        let entry = visible.get(self.selected).map(|r| {
            (
                r.id.clone(),
                r.name.clone(),
                r.kind.as_str().to_string(),
                r.directory.clone(),
            )
        });
        if let Some((id, name, kind, directory)) = entry {
            self.pending_delete = Some(PendingDelete::Resource {
                id,
                name,
                kind,
                directory,
            });
            self.mode = InputMode::ConfirmDelete;
        }
    }

    pub(super) fn delete_pending_resource(&mut self, id: String, name: String) {
        match self.mgr.trash_resource(&id) {
            Ok(_) => self.message = Some(format!("Moved '{name}' to trash")),
            Err(e) => self.message = Some(format!("Error: {e}")),
        }
        self.reload();
    }

    pub(super) fn restore_selected_trash(&mut self) {
        let visible = self.visible_trash();
        if let Some(entry) = visible.get(self.selected) {
            let trash_id = entry.id.clone();
            let name = entry.name.clone();
            match self.mgr.restore_from_trash(&trash_id) {
                Ok(_) => self.message = Some(format!("Restored '{name}'")),
                Err(e) => self.message = Some(format!("Error: {e}")),
            }
            self.reload();
        }
    }

    pub(super) fn purge_selected_trash(&mut self) {
        let visible = self.visible_trash();
        if let Some(entry) = visible.get(self.selected) {
            let trash_id = entry.id.clone();
            let name = entry.name.clone();
            match self.mgr.purge_trash(&trash_id) {
                Ok(_) => self.message = Some(format!("Permanently deleted '{name}'")),
                Err(e) => self.message = Some(format!("Error: {e}")),
            }
            self.reload();
        }
    }

    pub(super) fn confirm_delete_selected_group(&mut self) {
        let visible = self.visible_groups();
        if let Some((id, name, _, _, _)) = visible.get(self.selected) {
            self.pending_delete = Some(PendingDelete::Group {
                id: id.clone(),
                name: name.clone(),
            });
            self.mode = InputMode::ConfirmDelete;
        }
    }

    pub(super) fn delete_pending_group(&mut self, id: String, name: String) {
        let path = self.mgr.paths().groups_dir().join(format!("{id}.toml"));
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        self.message = Some(format!("Group '{name}' deleted"));
        self.reload();
    }
}

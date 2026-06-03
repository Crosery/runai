use super::model::App;
use crate::core::market::{self, Market};

impl App {
    pub(super) fn install_market_selected(&mut self) {
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
                let install_root = paths.skills_dir();
                let rt = tokio::runtime::Runtime::new().unwrap();
                match rt.block_on(Market::install_single(&found, &install_root)) {
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

    pub(super) fn install_from_market(&mut self) {
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
        match rt.block_on(Market::install_single(
            &skill,
            &self.mgr.paths().skills_dir(),
        )) {
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

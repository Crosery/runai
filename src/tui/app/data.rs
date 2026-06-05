use super::model::{App, Tab};
use crate::core::market::{self, Market};
use std::sync::mpsc;

impl App {
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

    pub fn reload(&mut self) {
        let kind_filter = match self.tab {
            Tab::Skills => Some(crate::core::resource::ResourceKind::Skill),
            Tab::Mcps => Some(crate::core::resource::ResourceKind::Mcp),
            Tab::Groups | Tab::Market | Tab::Trash | Tab::Hooks | Tab::Community => None,
        };

        self.items = self
            .mgr
            .list_resources(kind_filter, None)
            .unwrap_or_default();
        self.trash_items = self.mgr.list_trash().unwrap_or_default();

        // Overlay transcript-derived usage counts and sort by most-used first.
        if let Ok(stats) = crate::core::transcript_stats::scan_default() {
            use crate::core::resource::ResourceKind;
            use crate::core::transcript_stats::StatKind;
            for r in &mut self.items {
                let sk = match r.kind {
                    ResourceKind::Skill => StatKind::Skill,
                    ResourceKind::Mcp => StatKind::Mcp,
                };
                let (count, last) = stats.lookup(sk, &r.name);
                r.usage_count = count;
                r.last_used_at = last;
            }
            self.items.sort_by(|a, b| {
                b.usage_count
                    .cmp(&a.usage_count)
                    .then_with(|| a.name.cmp(&b.name))
            });
        }

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
                (id, g.name, members.len(), enabled, g.description)
            })
            .collect();

        let (es, em) = self.mgr.status(self.active_target).unwrap_or((0, 0));
        let (ts, tm) = self.mgr.resource_count();
        self.status = (es, ts, em, tm);
        self.max_usage_count = self.items.iter().map(|r| r.usage_count).max().unwrap_or(0);

        if self.selected >= self.visible_count() && self.visible_count() > 0 {
            self.selected = self.visible_count() - 1;
        }

        // Tab-specific lazy loads: keep these last so the cheap shared
        // refresh above always runs even on Hooks / Community tabs.
        match self.tab {
            Tab::Hooks => self.reload_hook_status(),
            Tab::Community => self.reload_community(),
            _ => {}
        }
    }

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
}

use super::Scanner;
use crate::core::db::Database;
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Default)]
pub struct ScanResult {
    pub adopted: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
    /// Names of skills newly adopted into the managed dir this scan pass.
    /// `runai scan` uses these to spawn a targeted enrich so each newly
    /// adopted skill gets an AI summary immediately rather than having to
    /// wait for a SessionStart enrich pass.
    pub adopted_names: Vec<String>,
}

impl Scanner {
    /// Scan the managed skills directory (~/.skill-manager/skills/) and register
    /// any skill that isn't already in the database.
    pub(super) fn scan_managed_dir(paths: &AppPaths, db: &Database) -> ScanResult {
        let mut result = ScanResult::default();
        let skills_dir = paths.skills_dir();

        if !skills_dir.exists() {
            return result;
        }

        let entries = match std::fs::read_dir(&skills_dir) {
            Ok(e) => e,
            Err(_) => return result,
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let name = match entry.file_name().to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Check if already in DB — if so, refresh description if stale
            let existing = ["local:", "adopted:", "github:"]
                .iter()
                .find_map(|prefix| {
                    let id = format!("{prefix}{name}");
                    db.get_resource(&id).ok().flatten()
                })
                .or_else(|| {
                    db.list_resources(None, None)
                        .ok()
                        .and_then(|all| all.into_iter().find(|r| r.name == name))
                });

            if let Some(existing) = existing {
                if Self::is_stale_description(&existing.description) {
                    let desc = Self::extract_description(&path);
                    if !Self::is_stale_description(&desc) {
                        let _ = db.update_description(&existing.id, &desc);
                    }
                }
                result.skipped += 1;
                continue;
            }

            // Register as local skill
            let description = Self::extract_description(&path);
            let resource = Resource {
                id: format!("local:{name}"),
                name: name.clone(),
                kind: ResourceKind::Skill,
                description,
                directory: path.clone(),
                source: Source::Local { path: path.clone() },
                installed_at: chrono::Utc::now().timestamp(),
                enabled: HashMap::new(),
                usage_count: 0,
                last_used_at: None,
                owner_user_id: None,
            };

            match db.insert_resource(&resource) {
                Ok(_) => {
                    result.adopted += 1;
                    result.adopted_names.push(name.clone());
                }
                Err(e) => result.errors.push(format!("{name}: {e}")),
            }
        }

        result
    }

    /// Scan .agents/skills/ — read-only, register in DB but never move files.
    pub(super) fn scan_agents_dir(dir: &Path, db: &Database) -> ScanResult {
        let mut result = ScanResult::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return result,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match entry.file_name().to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !path.join("SKILL.md").exists() {
                continue;
            }

            // Skip if already known — refresh description if stale
            let existing = db
                .list_resources(None, None)
                .ok()
                .and_then(|all| all.into_iter().find(|r| r.name == name));
            if let Some(existing) = existing {
                if Self::is_stale_description(&existing.description) {
                    let desc = Self::extract_description(&path);
                    if !Self::is_stale_description(&desc) {
                        let _ = db.update_description(&existing.id, &desc);
                    }
                }
                result.skipped += 1;
                continue;
            }

            let description = Self::extract_description(&path);
            let resource = Resource {
                id: format!("local:{name}"),
                name,
                kind: ResourceKind::Skill,
                description,
                directory: path.clone(),
                source: Source::Local { path: path.clone() },
                installed_at: chrono::Utc::now().timestamp(),
                enabled: HashMap::new(),
                usage_count: 0,
                last_used_at: None,
                owner_user_id: None,
            };
            let adopted_name = resource.name.clone();
            match db.insert_resource(&resource) {
                Ok(_) => {
                    result.adopted += 1;
                    result.adopted_names.push(adopted_name);
                }
                Err(e) => result
                    .errors
                    .push(format!("{}: {e}", entry.file_name().to_string_lossy())),
            }
        }
        result
    }
}

use super::SkillManager;
use crate::core::cli_target::CliTarget;
use crate::core::linker::Linker;
use crate::core::resource::{Resource, ResourceKind, Source, TrashEntry};
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Outcome of [`SkillManager::delete_user_cascade`].
#[derive(Debug, Default, Clone, Copy)]
pub struct UserCascadeReport {
    /// Private skills moved to the public (admin-recoverable) trash.
    pub trashed: usize,
    /// Community-pool entries removed.
    pub community_removed: usize,
    /// `user_skill_library` subscriptions cleared.
    pub library_cleared: usize,
}

impl SkillManager {
    fn trash_entry_id(resource_id: &str, deleted_at_ms: i64) -> String {
        format!("trash:{deleted_at_ms}:{resource_id}")
    }

    fn trash_payload_path(
        &self,
        name: &str,
        deleted_at_ms: i64,
        owner_user_id: Option<&str>,
    ) -> Result<PathBuf> {
        let slug = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .trim_matches('-')
            .to_string();
        let slug = if slug.is_empty() { "resource" } else { &slug };
        // Public resources land in the global trash; private ones land in
        // the owner's per-user trash subtree so restore mirrors install
        // and `purge_trash` can never cross owner boundaries.
        let root = match owner_user_id {
            None => self.paths.trash_dir(),
            Some(uid) => {
                self.paths.ensure_user_dirs(uid)?;
                self.paths.user_trash_dir(uid)?
            }
        };
        Ok(root.join(format!("{deleted_at_ms}-{slug}")))
    }

    pub fn list_trash(&self) -> Result<Vec<TrashEntry>> {
        self.db.list_trash_entries()
    }

    pub fn find_trash_id(&self, query: &str) -> Option<String> {
        let entries = self.list_trash().ok()?;
        if let Some(entry) = entries.iter().find(|entry| entry.id == query) {
            return Some(entry.id.clone());
        }
        entries
            .into_iter()
            .find(|entry| entry.name == query)
            .map(|entry| entry.id)
    }

    pub fn trash_resource(&self, resource_id: &str) -> Result<TrashEntry> {
        let now = chrono::Utc::now();
        let deleted_at = now.timestamp();
        let deleted_at_ms = now.timestamp_millis();

        if let Some(mcp_name) = resource_id.strip_prefix("mcp:") {
            let mut enabled_targets = Vec::new();
            let mut mcp_configs = HashMap::new();
            for target in CliTarget::ALL {
                if let Some(entry) = self.remove_mcp_entry_from_target(mcp_name, *target)? {
                    enabled_targets.push(*target);
                    mcp_configs.insert(*target, entry);
                }
            }

            let disabled_backup = self.read_mcp_backup(mcp_name)?;
            self.remove_mcp_backup(mcp_name)?;
            let group_ids = self.db.take_groups_for_resource(resource_id)?;

            if mcp_configs.is_empty() && disabled_backup.is_none() {
                bail!("resource not found: {resource_id}");
            }

            let entry = TrashEntry {
                id: Self::trash_entry_id(resource_id, deleted_at_ms),
                resource_id: resource_id.to_string(),
                name: mcp_name.to_string(),
                kind: ResourceKind::Mcp,
                description: String::new(),
                directory: PathBuf::new(),
                source: Source::Local {
                    path: PathBuf::new(),
                },
                installed_at: 0,
                usage_count: 0,
                last_used_at: None,
                owner_user_id: None,
                deleted_at,
                payload_path: None,
                enabled_targets,
                group_ids,
                mcp_configs,
                disabled_backup,
            };
            self.db.insert_trash_entry(&entry)?;
            return Ok(entry);
        }

        let resource = self
            .db
            .get_resource(resource_id)?
            .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;

        let enabled_map = self.check_skill_symlinks(&resource.name);
        let enabled_targets = CliTarget::ALL
            .iter()
            .copied()
            .filter(|target| enabled_map.get(target).copied().unwrap_or(false))
            .collect::<Vec<_>>();
        let payload_path = self.trash_payload_path(
            &resource.name,
            deleted_at_ms,
            resource.owner_user_id.as_deref(),
        )?;

        self.remove_skill_links(&resource.name)?;
        if resource.directory.exists() {
            if let Some(parent) = payload_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            Linker::move_dir(&resource.directory, &payload_path)?;
        } else {
            bail!(
                "resource directory missing: {}",
                resource.directory.display()
            );
        }

        let group_ids = self.db.take_groups_for_resource(resource_id)?;
        // Drop AI summary + user rating rows so they don't linger in the
        // dashboard's enrichment-progress count for a skill that's been
        // trashed. If the user restores the skill, they can re-run
        // `runai recommend enrich` to regenerate.
        self.db.delete_skill_scoring(&resource.name)?;
        self.db.delete_resource(resource_id)?;

        let entry = TrashEntry {
            id: Self::trash_entry_id(resource_id, deleted_at_ms),
            resource_id: resource.id.clone(),
            name: resource.name.clone(),
            kind: resource.kind,
            description: resource.description.clone(),
            directory: resource.directory.clone(),
            source: resource.source.clone(),
            installed_at: resource.installed_at,
            usage_count: resource.usage_count,
            last_used_at: resource.last_used_at,
            owner_user_id: resource.owner_user_id.clone(),
            deleted_at,
            payload_path: Some(payload_path),
            enabled_targets,
            group_ids,
            mcp_configs: HashMap::new(),
            disabled_backup: None,
        };
        self.db.insert_trash_entry(&entry)?;
        // Trashing a skill should also drop every user's library
        // subscription so the "我的库" tab doesn't show a row that
        // 404s on click. For a private skill only the owner could
        // have subscribed; for a public-pool skill any user could —
        // either way `library_remove_for_all(name)` is correct and
        // idempotent.
        let _ = self.db.library_remove_for_all(&entry.name);
        Ok(entry)
    }

    pub fn uninstall(&self, resource_id: &str) -> Result<()> {
        let _ = self.trash_resource(resource_id)?;
        Ok(())
    }

    /// Cascade-delete everything a user owns, then the user row itself
    /// (PLANNING owner/auth cleanup, B1/B3/B4). Order is FS-before-DB so a crash
    /// leaves recoverable state. The private skills land in the PUBLIC
    /// (admin-recoverable) trash, de-owned, with their restore target pointed at
    /// the public pool — the owner is going away, so restoring later mustn't
    /// recreate an orphan under `<data>/users/<uid>/`.
    ///
    /// HIGH-RISK destructive path: every `remove_dir_all` goes through
    /// [`Self::guarded_remove_dir_all`] (canonicalize + `starts_with`) and the
    /// uid is validated by `AppPaths::user_root` (rejects traversal) before any
    /// FS op. Covered by `tests/user_delete_cascade_e2e.rs` (incl. the
    /// `RUNE_DATA_DIR` decoy run that proves it uses `self.paths`, never the
    /// env-reading `paths::data_dir()`).
    pub fn delete_user_cascade(&self, user_id: &str) -> Result<UserCascadeReport> {
        // Validate the uid up front: user_root() bails on traversal-y ids, so a
        // bad id can never reach remove_dir_all below.
        let user_root = self.paths.user_root(user_id)?;

        let now = chrono::Utc::now();
        let deleted_at = now.timestamp();
        let base_ms = now.timestamp_millis();

        // 1. Trash each owned private skill → public trash, restorable to pool.
        let owned: Vec<Resource> = self
            .db
            .list_resources_for_user(None, Some(user_id))?
            .into_iter()
            .filter(|r| r.owner_user_id.as_deref() == Some(user_id))
            .collect();
        let mut trashed = 0usize;
        for (i, r) in owned.iter().enumerate() {
            let deleted_at_ms = base_ms + i as i64; // unique payload dir per row
            let payload = self.trash_payload_path(&r.name, deleted_at_ms, None)?; // None → public trash
            if r.directory.exists() {
                if let Some(parent) = payload.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Linker::move_dir(&r.directory, &payload)?;
            }
            let group_ids = self.db.take_groups_for_resource(&r.id)?;
            self.db.delete_skill_scoring(&r.name)?;
            self.db.delete_resource(&r.id)?;
            let entry = TrashEntry {
                id: Self::trash_entry_id(&r.id, deleted_at_ms),
                resource_id: r.id.clone(),
                name: r.name.clone(),
                kind: r.kind,
                description: r.description.clone(),
                // restore target = public pool (original owner is gone).
                directory: self.paths.skills_dir().join(&r.name),
                source: r.source.clone(),
                installed_at: r.installed_at,
                usage_count: r.usage_count,
                last_used_at: r.last_used_at,
                owner_user_id: None,
                deleted_at,
                payload_path: Some(payload),
                enabled_targets: Vec::new(),
                group_ids,
                mcp_configs: HashMap::new(),
                disabled_backup: None,
            };
            self.db.insert_trash_entry(&entry)?;
            let _ = self.db.library_remove_for_all(&r.name);
            trashed += 1;
        }

        // 2. Community-pool uploads (rows + per-uploader payload tree).
        let mut community_removed = 0usize;
        for cs in self.db.community_skills_by_uploader(user_id)? {
            let _ = self.db.delete_community_skill(user_id, &cs.name);
            community_removed += 1;
        }
        if let Ok(cdir) = self.paths.community_uploader_dir(user_id)
            && cdir.exists()
        {
            self.guarded_remove_dir_all(&cdir, &self.paths.community_dir())?;
        }

        // 3. Physical per-user subtree (skills already moved to trash above).
        if user_root.exists() {
            let users_base = self.paths.data_dir().join("users");
            self.guarded_remove_dir_all(&user_root, &users_base)?;
        }

        // 4. Anonymize their telemetry.
        let _ = self.db.anonymize_router_events_for_user(user_id);

        // 5. Library subscriptions + the user row.
        let library_cleared = self.db.library_count(user_id).unwrap_or(0);
        let _ = self.db.library_clear(user_id);
        self.db.delete_user(user_id)?;

        Ok(UserCascadeReport {
            trashed,
            community_removed,
            library_cleared,
        })
    }

    /// `remove_dir_all` with a path-containment guard: canonicalize `target` and
    /// `must_be_under`, refuse unless `target` resolves inside the allowed base.
    /// The one place destructive recursion is allowed in the cascade.
    fn guarded_remove_dir_all(&self, target: &Path, must_be_under: &Path) -> Result<()> {
        let target_real = target.canonicalize()?;
        let base_real = must_be_under.canonicalize()?;
        if !target_real.starts_with(&base_real) {
            bail!(
                "refusing remove_dir_all: {} escaped {}",
                target_real.display(),
                base_real.display()
            );
        }
        std::fs::remove_dir_all(&target_real)?;
        Ok(())
    }

    pub fn restore_from_trash(&self, trash_id: &str) -> Result<()> {
        let entry = self
            .db
            .get_trash_entry(trash_id)?
            .ok_or_else(|| anyhow::anyhow!("trash entry not found: {trash_id}"))?;

        match entry.kind {
            ResourceKind::Skill => {
                let payload_path = entry
                    .payload_path
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("trash payload missing for {}", entry.name))?;
                if !payload_path.exists() {
                    bail!("trash payload missing: {}", payload_path.display());
                }
                if entry.directory.exists() || self.db.get_resource(&entry.resource_id)?.is_some() {
                    bail!("resource already exists: {}", entry.name);
                }
                if let Some(parent) = entry.directory.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                Linker::move_dir(&payload_path, &entry.directory)?;

                let resource = Resource {
                    id: entry.resource_id.clone(),
                    name: entry.name.clone(),
                    kind: entry.kind,
                    description: entry.description.clone(),
                    directory: entry.directory.clone(),
                    source: entry.source.clone(),
                    installed_at: entry.installed_at,
                    enabled: HashMap::new(),
                    usage_count: entry.usage_count,
                    last_used_at: entry.last_used_at,
                    owner_user_id: entry.owner_user_id.clone(),
                    publish_status: "draft".to_string(),
                };
                self.db.insert_resource(&resource)?;
                for group_id in &entry.group_ids {
                    self.db.add_group_member(group_id, &entry.resource_id)?;
                }
                for target in &entry.enabled_targets {
                    self.enable_resource(&entry.resource_id, *target, None)?;
                }
            }
            ResourceKind::Mcp => {
                let mcp_status = Self::read_mcp_status_from_configs();
                for target in entry.mcp_configs.keys() {
                    if mcp_status
                        .get(&entry.name)
                        .and_then(|targets| targets.get(target))
                        .copied()
                        .unwrap_or(false)
                    {
                        bail!("MCP already exists for {} on {}", entry.name, target.name());
                    }
                }

                if entry.disabled_backup.is_some()
                    && self
                        .paths
                        .mcps_dir()
                        .join(format!("{}.json", entry.name))
                        .exists()
                {
                    bail!("disabled MCP backup already exists: {}", entry.name);
                }

                for (target, mcp_entry) in &entry.mcp_configs {
                    self.write_mcp_entry_to_target(&entry.name, *target, mcp_entry)?;
                }
                if let Some(ref disabled_backup) = entry.disabled_backup {
                    self.write_mcp_backup(&entry.name, disabled_backup)?;
                }
                for group_id in &entry.group_ids {
                    self.db.add_group_member(group_id, &entry.resource_id)?;
                }
            }
        }

        self.db.delete_trash_entry(trash_id)?;
        Ok(())
    }

    pub fn purge_trash(&self, trash_id: &str) -> Result<()> {
        let entry = self
            .db
            .get_trash_entry(trash_id)?
            .ok_or_else(|| anyhow::anyhow!("trash entry not found: {trash_id}"))?;

        if let Some(payload_path) = entry.payload_path
            && payload_path.exists()
        {
            if payload_path.is_dir() {
                std::fs::remove_dir_all(&payload_path)?;
            } else {
                std::fs::remove_file(&payload_path)?;
            }
        }

        self.db.delete_trash_entry(trash_id)?;
        Ok(())
    }

    pub fn empty_trash(&self) -> Result<usize> {
        let entries = self.list_trash()?;
        for entry in &entries {
            self.purge_trash(&entry.id)?;
        }
        Ok(entries.len())
    }
}

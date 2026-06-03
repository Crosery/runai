use super::SkillManager;
use crate::core::cli_target::CliTarget;
use crate::core::linker::Linker;
use crate::core::resource::{Resource, ResourceKind, Source};
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::path::Path;

impl SkillManager {
    // --- Resource management ---

    pub fn register_local_skill(&self, name: &str) -> Result<()> {
        self.register_local_skill_for(name, None)
    }

    /// Owner-aware variant. `owner_user_id`:
    /// - `None` → adopt `<data>/skills/<name>/` into the public pool
    /// - `Some(uid)` → adopt `<data>/users/<uid>/skills/<name>/` into uid's private pool
    pub fn register_local_skill_for(
        &self,
        name: &str,
        owner_user_id: Option<&str>,
    ) -> Result<()> {
        let root = match owner_user_id {
            None => self.paths.skills_dir(),
            Some(uid) => self.paths.user_skills_dir(uid)?,
        };
        let dir = root.join(name);
        if !dir.exists() {
            bail!("skill directory not found: {}", dir.display());
        }

        let description = Self::extract_description(&dir);
        let source = Source::Local { path: dir.clone() };
        let id = Resource::generate_id(&source, name, owner_user_id);

        let resource = Resource {
            id,
            name: name.to_string(),
            kind: ResourceKind::Skill,
            description,
            directory: dir,
            source,
            installed_at: chrono::Utc::now().timestamp(),
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: owner_user_id.map(String::from),
        };

        self.db.insert_resource(&resource)?;
        Ok(())
    }

    pub fn enable_resource(
        &self,
        resource_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        if let Some(mcp_name) = resource_id.strip_prefix("mcp:") {
            self.restore_mcp(mcp_name, target)
        } else {
            let resource = self
                .db
                .get_resource(resource_id)?
                .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;
            let cli_dir = cli_dir_override
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| target.skills_dir());
            std::fs::create_dir_all(&cli_dir)?;
            let link_path = cli_dir.join(&resource.name);
            // `link_path.exists()` follows symlinks, so a DANGLING symlink
            // returns false here — making the old `if !exists()` check pass
            // and then fail with EEXIST inside `create_link`. Use the force
            // variant so any pre-existing symlink (including dangling) is
            // unlinked and recreated to point at the right managed dir.
            // Real directories at the link path are still left alone (force
            // only clobbers symlinks) — those would surface as a loud error.
            Linker::create_link_force(&resource.directory, &link_path)?;
            Ok(())
        }
    }

    pub fn disable_resource(
        &self,
        resource_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        if let Some(mcp_name) = resource_id.strip_prefix("mcp:") {
            if mcp_name == "runai" || mcp_name == "skill-manager" {
                bail!("Cannot disable runai — it would remove its own MCP connection");
            }
            self.remove_mcp(mcp_name, target)
        } else {
            let resource = self
                .db
                .get_resource(resource_id)?
                .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;
            let cli_dir = cli_dir_override
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| target.skills_dir());
            let link_path = cli_dir.join(&resource.name);
            // Remove symlink regardless of target — handles both our managed dir
            // and legacy paths (e.g. old .skill-manager/ symlinks)
            if Linker::is_symlink(&link_path) {
                Linker::remove_link(&link_path)?;
            }
            Ok(())
        }
    }

    /// Check which CLI targets have this skill (symlink or direct dir in .agents/skills/ or skills/).
    pub(super) fn check_skill_symlinks(&self, name: &str) -> HashMap<CliTarget, bool> {
        let mut map = HashMap::new();
        for target in CliTarget::ALL {
            let primary = target.skills_dir().join(name);
            let legacy = target.agents_skills_dir().join(name);
            // Use symlink_metadata (doesn't follow symlink) to detect even broken symlinks,
            // plus exists() for real directories
            let enabled = primary.symlink_metadata().is_ok() || legacy.symlink_metadata().is_ok();
            map.insert(*target, enabled);
        }
        map
    }

    pub(super) fn remove_skill_links(&self, name: &str) -> Result<()> {
        for target in CliTarget::ALL {
            for link in [
                target.skills_dir().join(name),
                target.agents_skills_dir().join(name),
            ] {
                if Linker::is_our_symlink(&link, self.paths.data_dir()) {
                    Linker::remove_link(&link)?;
                }
            }
        }
        Ok(())
    }
}

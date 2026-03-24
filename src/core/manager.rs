use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::{Result, bail};
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::group::Group;
use crate::core::linker::Linker;
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};
use crate::core::scanner::Scanner;
use crate::core::classifier::Classifier;

pub struct SkillManager {
    paths: AppPaths,
    db: Database,
}

impl SkillManager {
    pub fn new() -> Result<Self> {
        let paths = AppPaths::default_path();
        paths.ensure_dirs()?;
        let db = Database::open(&paths.db_path())?;
        Ok(Self { paths, db })
    }

    pub fn with_base(base: PathBuf) -> Result<Self> {
        let paths = AppPaths::with_base(base);
        paths.ensure_dirs()?;
        let db = Database::open(&paths.db_path())?;
        Ok(Self { paths, db })
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn db(&self) -> &Database {
        &self.db
    }

    // --- Scan ---

    pub fn scan(&self) -> Result<crate::core::scanner::ScanResult> {
        Scanner::scan_all(&self.paths, &self.db)
    }

    // --- Resource management ---

    pub fn register_local_skill(&self, name: &str) -> Result<()> {
        let dir = self.paths.skills_dir().join(name);
        if !dir.exists() {
            bail!("skill directory not found: {}", dir.display());
        }

        let description = Self::extract_description(&dir);
        let source = Source::Local { path: dir.clone() };
        let id = Resource::generate_id(&source, name);

        let resource = Resource {
            id,
            name: name.to_string(),
            kind: ResourceKind::Skill,
            description,
            directory: dir,
            source,
            installed_at: chrono::Utc::now().timestamp(),
            enabled: HashMap::new(),
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
        let resource = self.db.get_resource(resource_id)?
            .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;

        let cli_dir = cli_dir_override
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| target.skills_dir());

        std::fs::create_dir_all(&cli_dir)?;
        let link_path = cli_dir.join(&resource.name);

        if !link_path.exists() {
            Linker::create_link(&resource.directory, &link_path)?;
        }

        self.db.set_target_enabled(resource_id, target, true)?;
        Ok(())
    }

    pub fn disable_resource(
        &self,
        resource_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        let resource = self.db.get_resource(resource_id)?
            .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;

        let cli_dir = cli_dir_override
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| target.skills_dir());

        let link_path = cli_dir.join(&resource.name);

        if Linker::is_our_symlink(&link_path, self.paths.data_dir()) {
            Linker::remove_link(&link_path)?;
        }

        self.db.set_target_enabled(resource_id, target, false)?;
        Ok(())
    }

    pub fn list_resources(
        &self,
        kind: Option<ResourceKind>,
        enabled_for: Option<CliTarget>,
    ) -> Result<Vec<Resource>> {
        self.db.list_resources(kind, enabled_for)
    }

    pub fn uninstall(&self, resource_id: &str) -> Result<()> {
        let resource = self.db.get_resource(resource_id)?
            .ok_or_else(|| anyhow::anyhow!("resource not found: {resource_id}"))?;

        for target in CliTarget::ALL {
            let link = target.skills_dir().join(&resource.name);
            if Linker::is_our_symlink(&link, self.paths.data_dir()) {
                Linker::remove_link(&link)?;
            }
        }

        self.db.delete_resource(resource_id)?;
        Ok(())
    }

    // --- Group management ---

    pub fn create_group(&self, group_id: &str, group: &Group) -> Result<()> {
        let path = self.paths.groups_dir().join(format!("{group_id}.toml"));
        group.save_to_file(&path)?;

        for member in &group.members {
            if let Some(rid) = self.find_resource_id(&member.name) {
                self.db.add_group_member(group_id, &rid)?;
            }
        }

        Ok(())
    }

    pub fn list_groups(&self) -> Result<Vec<(String, Group)>> {
        let dir = self.paths.groups_dir();
        let mut groups = Vec::new();

        if !dir.exists() {
            return Ok(groups);
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("toml") {
                let id = path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                match Group::load_from_file(&path) {
                    Ok(group) => groups.push((id, group)),
                    Err(_) => continue,
                }
            }
        }

        groups.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        Ok(groups)
    }

    pub fn enable_group(
        &self,
        group_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        let members = self.db.get_group_members(group_id)?;
        for member in &members {
            self.enable_resource(&member.id, target, cli_dir_override)?;
        }
        Ok(())
    }

    pub fn disable_group(
        &self,
        group_id: &str,
        target: CliTarget,
        cli_dir_override: Option<&Path>,
    ) -> Result<()> {
        let members = self.db.get_group_members(group_id)?;
        for member in &members {
            self.disable_resource(&member.id, target, cli_dir_override)?;
        }
        Ok(())
    }

    pub fn get_suggested_groups(&self, name: &str, description: &str) -> Vec<String> {
        Classifier::suggest_groups(name, description)
    }

    pub fn status(&self, target: CliTarget) -> Result<(usize, usize)> {
        self.db.enabled_count(target)
    }

    // --- Internal ---

    fn extract_description(skill_dir: &Path) -> String {
        let skill_md = skill_dir.join("SKILL.md");
        if let Ok(content) = std::fs::read_to_string(&skill_md) {
            for line in content.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                return trimmed.chars().take(200).collect();
            }
        }
        String::new()
    }

    /// Returns true if no resources have been registered yet.
    pub fn is_first_launch(&self) -> bool {
        self.db.resource_count().map(|(s, m)| s + m == 0).unwrap_or(true)
    }

    /// Register discovered MCP servers into the database.
    pub fn register_mcps(&self, entries: &[crate::core::mcp_discovery::McpEntry]) -> usize {
        let mut count = 0;
        for entry in entries {
            let id = format!("mcp:{}", entry.name);
            let is_new = self.db.get_resource(&id).ok().flatten().is_none();

            if is_new {
                let resource = Resource {
                    id: id.clone(),
                    name: entry.name.clone(),
                    kind: ResourceKind::Mcp,
                    description: if entry.description.is_empty() {
                        format!("{} {}", entry.command, entry.args.join(" "))
                    } else {
                        entry.description.clone()
                    },
                    directory: entry.source_file.parent().unwrap_or(Path::new(".")).to_path_buf(),
                    source: Source::Local { path: entry.source_file.clone() },
                    installed_at: chrono::Utc::now().timestamp(),
                    enabled: HashMap::new(),
                };
                if self.db.insert_resource(&resource).is_err() {
                    continue;
                }
                count += 1;
            }

            // Sync enabled status from the CLI config
            let target = CliTarget::from_str(&entry.source_cli)
                .unwrap_or(CliTarget::Claude);
            let _ = self.db.set_target_enabled(&id, target, !entry.disabled);
        }
        count
    }

    pub fn find_resource_id(&self, name: &str) -> Option<String> {
        for prefix in &["local:", "adopted:", "github:"] {
            let id = format!("{prefix}{name}");
            if let Ok(Some(_)) = self.db.get_resource(&id) {
                return Some(id);
            }
        }
        if let Ok(all) = self.db.list_resources(None, None) {
            for r in all {
                if r.name == name {
                    return Some(r.id);
                }
            }
        }
        None
    }
}

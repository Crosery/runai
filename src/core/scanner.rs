use std::collections::HashMap;
use std::path::Path;
use anyhow::Result;
use crate::core::backup;
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::linker::{Linker, EntryType};
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};

#[derive(Debug, Default)]
pub struct ScanResult {
    pub adopted: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}

pub struct Scanner;

impl Scanner {
    pub fn scan_all(paths: &AppPaths, db: &Database) -> Result<ScanResult> {
        let mut total = ScanResult::default();

        // 0. Create backup before first scan if no backup exists
        if !backup::has_backup(paths) {
            let _ = backup::create_backup(paths);
        }

        // 1. Register all skills already in the managed directory
        let managed_result = Self::scan_managed_dir(paths, db);
        total.adopted += managed_result.adopted;
        total.skipped += managed_result.skipped;
        total.errors.extend(managed_result.errors);

        // 2. Then scan CLI directories for new/foreign skills
        for target in CliTarget::ALL {
            let cli_dir = target.skills_dir();
            if cli_dir.exists() {
                let result = Self::scan_cli_dir(&cli_dir, paths, db, *target)?;
                total.adopted += result.adopted;
                total.skipped += result.skipped;
                total.errors.extend(result.errors);
            }
        }

        // 3. Scan additional skill locations (plugins, .agents)
        let home = dirs::home_dir().unwrap_or_default();
        let extra_dirs = [
            home.join(".claude/.agents/skills"),
            home.join(".claude/plugins/cache"),
        ];
        for dir in &extra_dirs {
            if dir.exists() {
                let result = Self::scan_extra_skills_dir(dir, paths, db);
                total.adopted += result.adopted;
                total.skipped += result.skipped;
                total.errors.extend(result.errors);
            }
        }

        Ok(total)
    }

    /// Scan the managed skills directory (~/.skill-manager/skills/) and register
    /// any skill that isn't already in the database.
    fn scan_managed_dir(paths: &AppPaths, db: &Database) -> ScanResult {
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
            if !path.is_dir() { continue; }

            let name = match entry.file_name().to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };

            // Check if already in DB (try common ID prefixes)
            let already_exists = ["local:", "adopted:", "github:"].iter().any(|prefix| {
                let id = format!("{prefix}{name}");
                matches!(db.get_resource(&id), Ok(Some(_)))
            }) || db.list_resources(None, None)
                .map(|all| all.iter().any(|r| r.name == name))
                .unwrap_or(false);

            if already_exists {
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
            };

            match db.insert_resource(&resource) {
                Ok(_) => {
                    result.adopted += 1;
                }
                Err(e) => result.errors.push(format!("{name}: {e}")),
            }
        }

        result
    }

    pub fn scan_cli_dir(
        cli_dir: &Path,
        paths: &AppPaths,
        db: &Database,
        target: CliTarget,
    ) -> Result<ScanResult> {
        let mut result = ScanResult::default();

        let entries = match std::fs::read_dir(cli_dir) {
            Ok(e) => e,
            Err(_) => return Ok(result),
        };

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    result.errors.push(format!("read entry error: {e}"));
                    continue;
                }
            };

            let entry_path = entry.path();
            let name = match entry.file_name().to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };

            match Linker::detect_entry_type(&entry_path, paths.data_dir()) {
                EntryType::OurSymlink => {
                    // Already managed — symlink existence IS the enabled state
                }
                EntryType::ForeignSymlink | EntryType::RealDir => {
                    match Self::adopt_entry(&entry_path, &name, paths, db, target) {
                        Ok(_) => result.adopted += 1,
                        Err(e) => result.errors.push(format!("{name}: {e}")),
                    }
                }
                EntryType::NotExists => continue,
            }
        }

        Ok(result)
    }

    fn adopt_entry(
        entry_path: &Path,
        name: &str,
        paths: &AppPaths,
        db: &Database,
        target: CliTarget,
    ) -> Result<()> {
        let managed_dir = paths.skills_dir().join(name);

        let actual_source = if Linker::is_symlink(entry_path) {
            let link_target = std::fs::read_link(entry_path)?;
            // 解析为绝对路径
            if link_target.is_absolute() {
                link_target
            } else {
                entry_path.parent().unwrap_or(Path::new(".")).join(&link_target)
            }
        } else {
            entry_path.to_path_buf()
        };

        // 验证源目录存在且有 SKILL.md，否则跳过（断链保护）
        if !actual_source.exists() {
            anyhow::bail!("source does not exist: {}", actual_source.display());
        }
        if !actual_source.join("SKILL.md").exists() && actual_source.is_dir() {
            // 检查是否有子目录包含 SKILL.md（如 cc-switch 的嵌套结构）
            let has_skill = std::fs::read_dir(&actual_source)
                .map(|entries| entries.filter_map(|e| e.ok()).any(|e| e.file_name() == "SKILL.md"))
                .unwrap_or(false);
            if !has_skill {
                anyhow::bail!("no SKILL.md found in {}", actual_source.display());
            }
        }

        Linker::adopt_to_managed(&actual_source, &managed_dir, entry_path)?;

        let description = Self::extract_description(&managed_dir);
        let resource_id = format!("adopted:{name}");

        let resource = Resource {
            id: resource_id,
            name: name.to_string(),
            kind: ResourceKind::Skill,
            description,
            directory: managed_dir,
            source: Source::Adopted {
                original_cli: target.name().to_string(),
            },
            installed_at: chrono::Utc::now().timestamp(),
            enabled: HashMap::from([(target, true)]),
        };

        db.insert_resource(&resource)?;
        Ok(())
    }

    /// Scan directories like ~/.claude/.agents/skills/ and plugin caches.
    /// These are read-only — we register them in DB but don't move files.
    fn scan_extra_skills_dir(dir: &Path, paths: &AppPaths, db: &Database) -> ScanResult {
        let mut result = ScanResult::default();
        Self::scan_extra_recursive(dir, paths, db, &mut result, 0);
        result
    }

    fn scan_extra_recursive(dir: &Path, paths: &AppPaths, db: &Database, result: &mut ScanResult, depth: usize) {
        if depth > 5 { return; }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() { continue; }

            // If this dir has SKILL.md, register it
            if path.join("SKILL.md").exists() {
                let name = match entry.file_name().to_str() {
                    Some(n) => n.to_string(),
                    None => continue,
                };
                // Skip if already known
                let already_exists = db.list_resources(None, None)
                    .map(|all| all.iter().any(|r| r.name == name))
                    .unwrap_or(false);
                if already_exists {
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
                };
                match db.insert_resource(&resource) {
                    Ok(_) => result.adopted += 1,
                    Err(e) => result.errors.push(format!("{}: {e}", entry.file_name().to_string_lossy())),
                }
            } else {
                // Recurse into subdirectories (e.g., plugins/cache/xxx/version/skills/)
                Self::scan_extra_recursive(&path, paths, db, result, depth + 1);
            }
        }
    }

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

}

use std::collections::HashMap;
use std::path::Path;
use anyhow::Result;
use crate::core::backup;
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::linker::{Linker, EntryType};
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};

#[derive(Debug, Clone, PartialEq)]
pub enum SkillStatus {
    /// Already in ~/.skill-manager/skills/
    Managed,
    /// In CLI skills dir (~/.claude/skills/ etc.)
    CliDir,
    /// Found elsewhere, can be imported
    Unmanaged,
}

#[derive(Debug, Clone)]
pub struct DiscoveredSkill {
    pub name: String,
    pub path: std::path::PathBuf,
    pub status: SkillStatus,
}

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

        // 2. Scan user skills/ directories — adopt (move) foreign entries
        for target in CliTarget::ALL {
            let cli_dir = target.skills_dir();
            if cli_dir.exists() {
                let result = Self::scan_cli_dir(&cli_dir, paths, db, *target)?;
                total.adopted += result.adopted;
                total.skipped += result.skipped;
                total.errors.extend(result.errors);
            }
        }

        // 3. Scan plugin .agents/skills/ directories — register only, never move files
        for target in CliTarget::ALL {
            let agents_dir = target.agents_skills_dir();
            if agents_dir.exists() {
                let result = Self::scan_agents_dir(&agents_dir, db);
                total.adopted += result.adopted;
                total.skipped += result.skipped;
                total.errors.extend(result.errors);
            }
        }

        // 4. Scan ~/skills/ directory (e.g. SkillHub installs) — register only
        if let Some(home) = dirs::home_dir() {
            let home_skills = home.join("skills");
            if home_skills.exists() {
                let result = Self::scan_agents_dir(&home_skills, db);
                total.adopted += result.adopted;
                total.skipped += result.skipped;
                total.errors.extend(result.errors);
            }
        }

        Ok(total)
    }

    /// Directories to always skip during discovery (no useful SKILL.md inside).
    const SKIP_DIRS: &'static [&'static str] = &[
        ".git", "node_modules", "target", ".cache", ".cargo", ".rustup",
        ".npm", ".pnpm", "venv", "__pycache__", ".venv", ".nvm",
        "dist", "build", ".next", ".nuxt", ".wine", ".steam",
        ".mozilla", ".thunderbird", ".config", ".local",
    ];

    /// Path fragments that indicate a skill is NOT manageable by SM.
    const NOISE_PATHS: &'static [&'static str] = &[
        "/plugins/marketplaces/",   // CC plugin system manages these
        "/cc-profiles/",            // CC profile copies
        "/.vscode/",                // VS Code extensions
        "/.cursor/",                // Cursor extensions
        "/.antigravity/",           // Antigravity extensions
        "/backups/",                // SM backup copies
        "/__MACOSX/",               // macOS zip artifacts
    ];

    /// Discover SKILL.md files under a root dir. Built-in, no external tools needed.
    /// Returns only manageable skills (filters out plugins, backups, IDE extensions, etc.)
    pub fn discover_skills(root: &Path) -> Vec<DiscoveredSkill> {
        let mut raw = Vec::new();
        Self::walk_for_skills(root, &mut raw, 0);

        let home = dirs::home_dir().unwrap_or_default();
        let managed_dir = home.join(".skill-manager").join("skills");

        raw.into_iter()
            .filter_map(|path| {
                let path_str = path.to_string_lossy();

                // Filter out noise
                for noise in Self::NOISE_PATHS {
                    if path_str.contains(noise) { return None; }
                }

                let name = path.file_name()?.to_str()?.to_string();
                let status = if path.starts_with(&managed_dir) {
                    SkillStatus::Managed
                } else if path_str.contains("/.claude/skills/")
                    || path_str.contains("/.codex/skills/")
                    || path_str.contains("/.gemini/skills/")
                    || path_str.contains("/.opencode/skills/")
                {
                    SkillStatus::CliDir
                } else {
                    SkillStatus::Unmanaged
                };

                Some(DiscoveredSkill { name, path, status })
            })
            .collect()
    }

    fn walk_for_skills(dir: &Path, results: &mut Vec<std::path::PathBuf>, depth: usize) {
        if depth > 8 { return; }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() { continue; }
            // Skip symlinks to avoid loops
            if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) { continue; }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if Self::SKIP_DIRS.contains(&name_str.as_ref()) { continue; }
            if path.join("SKILL.md").exists() {
                results.push(path.clone());
            }
            Self::walk_for_skills(&path, results, depth + 1);
        }
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

            // Check if already in DB — if so, refresh description if stale
            let existing = ["local:", "adopted:", "github:"].iter().find_map(|prefix| {
                let id = format!("{prefix}{name}");
                db.get_resource(&id).ok().flatten()
            }).or_else(|| {
                db.list_resources(None, None).ok()
                    .and_then(|all| all.into_iter().find(|r| r.name == name))
            });

            if let Some(existing) = existing {
                // Refresh description if it's stale ("---" or empty)
                if existing.description.is_empty() || existing.description == "---" {
                    let desc = Self::extract_description(&path);
                    if !desc.is_empty() && desc != "---" {
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

            // Skip hidden/system dirs (e.g. .system)
            if name.starts_with('.') {
                result.skipped += 1;
                continue;
            }

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

    /// Scan .agents/skills/ — read-only, register in DB but never move files.
    fn scan_agents_dir(dir: &Path, db: &Database) -> ScanResult {
        let mut result = ScanResult::default();
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return result,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() { continue; }
            let name = match entry.file_name().to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };
            if !path.join("SKILL.md").exists() { continue; }

            // Skip if already known — refresh description if stale
            let existing = db.list_resources(None, None).ok()
                .and_then(|all| all.into_iter().find(|r| r.name == name));
            if let Some(existing) = existing {
                if existing.description.is_empty() || existing.description == "---" {
                    let desc = Self::extract_description(&path);
                    if !desc.is_empty() && desc != "---" {
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
            };
            match db.insert_resource(&resource) {
                Ok(_) => result.adopted += 1,
                Err(e) => result.errors.push(format!("{}: {e}", entry.file_name().to_string_lossy())),
            }
        }
        result
    }

    pub fn extract_description(skill_dir: &Path) -> String {
        let skill_md = skill_dir.join("SKILL.md");
        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(_) => return String::new(),
        };

        let mut lines = content.lines();
        let first = lines.next().unwrap_or("");

        // If starts with frontmatter, parse it
        if first.trim() == "---" {
            let mut in_frontmatter = true;
            let mut fm_description = String::new();

            for line in &mut lines {
                let trimmed = line.trim();
                if trimmed == "---" {
                    in_frontmatter = false;
                    break;
                }
                // Parse description field from frontmatter
                if let Some(rest) = trimmed.strip_prefix("description:") {
                    fm_description = rest.trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string();
                }
            }

            if !fm_description.is_empty() {
                return fm_description.chars().take(200).collect();
            }

            if in_frontmatter {
                return String::new(); // malformed frontmatter
            }

            // No description in frontmatter — fall through to body text
            for line in lines {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                return trimmed.chars().take(200).collect();
            }
            String::new()
        } else {
            // No frontmatter — original logic (first is already consumed)
            let trimmed = first.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('#') {
                return trimmed.chars().take(200).collect();
            }
            for line in lines {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                return trimmed.chars().take(200).collect();
            }
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_description_skips_frontmatter_reads_field() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: brainstorming\ndescription: \"Explores user intent and design before implementation.\"\n---\n\n# Brainstorming\n\nHelp turn ideas into designs.\n").unwrap();

        let desc = Scanner::extract_description(&skill_dir);
        assert_eq!(desc, "Explores user intent and design before implementation.");
    }

    #[test]
    fn extract_description_no_frontmatter_reads_first_text_line() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("simple-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "# My Skill\n\nThis skill does something useful.\n\nMore details here.\n").unwrap();

        let desc = Scanner::extract_description(&skill_dir);
        assert_eq!(desc, "This skill does something useful.");
    }

    #[test]
    fn extract_description_frontmatter_without_description_reads_body() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("no-desc");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: no-desc\n---\n\n# No Description Skill\n\nBut this line explains it.\n").unwrap();

        let desc = Scanner::extract_description(&skill_dir);
        assert_eq!(desc, "But this line explains it.");
    }

    #[test]
    fn discover_finds_skills_with_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Create valid skill dirs
        let s1 = root.join("skills").join("brainstorming");
        std::fs::create_dir_all(&s1).unwrap();
        std::fs::write(s1.join("SKILL.md"), "# Brainstorming").unwrap();

        let s2 = root.join("myproject").join("skills").join("tdd");
        std::fs::create_dir_all(&s2).unwrap();
        std::fs::write(s2.join("SKILL.md"), "# TDD").unwrap();

        // Dir WITHOUT SKILL.md — should NOT be found
        let no_skill = root.join("not-a-skill");
        std::fs::create_dir_all(&no_skill).unwrap();
        std::fs::write(no_skill.join("README.md"), "not a skill").unwrap();

        let found = Scanner::discover_skills(root);
        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"brainstorming"));
        assert!(names.contains(&"tdd"));
        assert!(!names.contains(&"not-a-skill"));
    }

    #[test]
    fn discover_filters_noise_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Plugin dir — should be filtered out
        let plugin = root.join("plugins").join("marketplaces").join("x").join("skills").join("foo");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("SKILL.md"), "# Plugin skill").unwrap();

        // Backup dir — should be filtered out
        let backup = root.join("backups").join("20260325").join("skills").join("bar");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(backup.join("SKILL.md"), "# Backup skill").unwrap();

        // Valid dir
        let valid = root.join("skills").join("real");
        std::fs::create_dir_all(&valid).unwrap();
        std::fs::write(valid.join("SKILL.md"), "# Real").unwrap();

        let found = Scanner::discover_skills(root);
        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"real"));
        assert!(!names.contains(&"foo"), "plugin skills should be filtered");
        assert!(!names.contains(&"bar"), "backup skills should be filtered");
    }

    #[test]
    fn discover_skips_git_and_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Skill inside node_modules — should be skipped
        let nm = root.join("node_modules").join("some-pkg").join("skill");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(nm.join("SKILL.md"), "# NM").unwrap();

        // Skill inside .git — should be skipped
        let git = root.join(".git").join("hooks").join("skill");
        std::fs::create_dir_all(&git).unwrap();
        std::fs::write(git.join("SKILL.md"), "# Git").unwrap();

        let found = Scanner::discover_skills(root);
        assert!(found.is_empty());
    }

    #[test]
    fn discover_classifies_status_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Unmanaged skill (not in CLI or managed dir)
        let s = root.join("myproject").join("skills").join("test-skill");
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join("SKILL.md"), "# Test").unwrap();

        let found = Scanner::discover_skills(root);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].status, SkillStatus::Unmanaged);
    }
}

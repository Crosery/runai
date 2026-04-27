use crate::core::backup;
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::linker::{EntryType, Linker};
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

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

/// What happened to a single entry during adoption.
#[derive(Debug, PartialEq, Eq)]
enum AdoptOutcome {
    /// Newly moved into the managed dir and linked back.
    Adopted,
    /// Dangling symlink whose name matched an already-managed skill — link redirected.
    Healed,
    /// Dangling symlink with no managed counterpart — left alone, counted as skipped.
    Orphaned,
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
        ".git",
        "node_modules",
        "target",
        ".cache",
        ".cargo",
        ".rustup",
        ".npm",
        ".pnpm",
        "venv",
        "__pycache__",
        ".venv",
        ".nvm",
        "dist",
        "build",
        ".next",
        ".nuxt",
        ".wine",
        ".steam",
        ".mozilla",
        ".thunderbird",
        ".config",
        ".local",
    ];

    /// Path fragments that indicate a skill is NOT manageable by SM.
    const NOISE_PATHS: &'static [&'static str] = &[
        "/plugins/marketplaces/", // CC plugin system manages these
        "/cc-profiles/",          // CC profile copies
        "/.vscode/",              // VS Code extensions
        "/.cursor/",              // Cursor extensions
        "/.antigravity/",         // Antigravity extensions
        "/backups/",              // SM backup copies
        "/__MACOSX/",             // macOS zip artifacts
    ];

    /// Discover SKILL.md files under a root dir. Built-in, no external tools needed.
    /// Returns only manageable skills (filters out plugins, backups, IDE extensions, etc.)
    pub fn discover_skills(root: &Path) -> Vec<DiscoveredSkill> {
        let mut raw = Vec::new();
        Self::walk_for_skills(root, &mut raw, 0);

        let home = dirs::home_dir().unwrap_or_default();
        let managed_dir = home.join(".runai").join("skills");
        let managed_dir_old = home.join(".skill-manager").join("skills");

        raw.into_iter()
            .filter_map(|path| {
                // Normalize separators so noise patterns using '/' also match on Windows
                // where path components are joined with '\'.
                let path_str = path.to_string_lossy().replace('\\', "/");

                // Filter out noise
                for noise in Self::NOISE_PATHS {
                    if path_str.contains(noise) {
                        return None;
                    }
                }

                let name = path.file_name()?.to_str()?.to_string();
                let status = if path.starts_with(&managed_dir) || path.starts_with(&managed_dir_old)
                {
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
        if depth > 8 {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            // Skip symlinks to avoid loops
            if entry.file_type().map(|ft| ft.is_symlink()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if Self::SKIP_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
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
                        Ok(AdoptOutcome::Adopted | AdoptOutcome::Healed) => result.adopted += 1,
                        Ok(AdoptOutcome::Orphaned) => result.skipped += 1,
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
    ) -> Result<AdoptOutcome> {
        let managed_dir = paths.skills_dir().join(name);

        let actual_source = if Linker::is_symlink(entry_path) {
            let link_target = std::fs::read_link(entry_path)?;
            // 解析为绝对路径
            if link_target.is_absolute() {
                link_target
            } else {
                entry_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(&link_target)
            }
        } else {
            entry_path.to_path_buf()
        };

        // ── Cross-data-dir safety guard ───────────────────────────────────────
        // 2026-04-27 incident root cause: developer ran
        //   RUNE_DATA_DIR=/tmp/x runai scan
        // — adopt then `std::fs::rename`d real skill data out of the user's
        // default ~/.runai/skills/ into /tmp/x/skills/, after which
        // `rm -rf /tmp/x` permanently deleted 5 skills.
        //
        // Trigger condition: actual_source resolves into the DEFAULT data
        // dir's skills/ subtree, BUT the active data dir points somewhere
        // else. Bail loudly rather than rename real data away.
        //
        // CRITICAL: must use `default_data_dir_no_env()` not `data_dir()` —
        // `data_dir()` reads RUNE_DATA_DIR itself, so in the dangerous case
        // it returns the override and the comparison degenerates to "always
        // equal". Verified by physical e2e test: the original implementation
        // using `data_dir()` silently let the rename through.
        let default_skills = crate::core::paths::default_data_dir_no_env().join("skills");
        let active_data = paths.data_dir().to_path_buf();
        if actual_source.starts_with(&default_skills)
            && active_data != crate::core::paths::default_data_dir_no_env()
        {
            anyhow::bail!(
                "refused to adopt '{}': source path {} is inside the default \
                 data dir, but the active data dir is {} — adopting would \
                 std::fs::rename real user data out of the default location. \
                 If you really want to test scan in isolation, also set HOME \
                 to a tempdir so the scanner sees no real user skills.\n\
                 See ~/.claude/vault/50-playbook/symlink-safety.md",
                name,
                actual_source.display(),
                active_data.display(),
            );
        }

        // 断链保护：源目录不存在时
        //   - 如果同名 managed skill 已存在（带 SKILL.md），把这条死链重新指向管理目录（自愈）
        //   - 否则静默跳过：孤儿 symlink 不是我们能处理的，没必要每次 scan 都报错刷屏
        if !actual_source.exists() {
            if Linker::is_symlink(entry_path) && managed_dir.join("SKILL.md").exists() {
                Linker::remove_link(entry_path)?;
                Linker::create_link(&managed_dir, entry_path)?;
                return Ok(AdoptOutcome::Healed);
            }
            return Ok(AdoptOutcome::Orphaned);
        }
        if !actual_source.join("SKILL.md").exists() && actual_source.is_dir() {
            // 检查是否有子目录包含 SKILL.md（如 cc-switch 的嵌套结构）
            let has_skill = std::fs::read_dir(&actual_source)
                .map(|entries| {
                    entries
                        .filter_map(|e| e.ok())
                        .any(|e| e.file_name() == "SKILL.md")
                })
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
            usage_count: 0,
            last_used_at: None,
        };

        db.insert_resource(&resource)?;
        Ok(AdoptOutcome::Adopted)
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
            };
            match db.insert_resource(&resource) {
                Ok(_) => result.adopted += 1,
                Err(e) => result
                    .errors
                    .push(format!("{}: {e}", entry.file_name().to_string_lossy())),
            }
        }
        result
    }

    /// True if a cached description is effectively useless and should be
    /// re-extracted. Includes the `"|"` / `">"` sentinel captured by the
    /// pre-fix parser — those DB rows get healed next time the user runs
    /// `runai scan` / `runai discover`.
    pub fn is_stale_description(d: &str) -> bool {
        let t = d.trim();
        t.is_empty() || matches!(t, "---" | "|" | ">" | "|-" | ">-" | "|+" | ">+")
    }

    pub fn extract_description(skill_dir: &Path) -> String {
        let skill_md = skill_dir.join("SKILL.md");
        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(_) => return String::new(),
        };

        // Collect all lines into an indexable vec so we can look ahead for block scalars.
        let all: Vec<&str> = content.lines().collect();

        if all.first().map(|s| s.trim()) == Some("---") {
            // ── YAML frontmatter ──
            let mut idx = 1;
            let mut fm_description = String::new();
            let mut fm_end: Option<usize> = None;

            while idx < all.len() {
                let line = all[idx];
                let trimmed = line.trim();
                if trimmed == "---" {
                    fm_end = Some(idx);
                    break;
                }
                if let Some(rest) = trimmed.strip_prefix("description:") {
                    let rest = rest.trim();
                    // Detect block scalar markers: `|`, `>`, each optionally with
                    // chomping indicator `-` or `+` (e.g. `|-`, `>+`).
                    let marker = rest.chars().next();
                    let is_literal = matches!(marker, Some('|'));
                    let is_folded = matches!(marker, Some('>'));
                    if is_literal || is_folded {
                        // Read subsequent indented lines until we hit a line
                        // that isn't indented (or hit frontmatter end).
                        let mut collected: Vec<String> = Vec::new();
                        let mut j = idx + 1;
                        let mut base_indent: Option<usize> = None;
                        while j < all.len() {
                            let l = all[j];
                            let lt = l.trim();
                            if lt == "---" {
                                fm_end = Some(j);
                                break;
                            }
                            if lt.is_empty() {
                                collected.push(String::new());
                                j += 1;
                                continue;
                            }
                            let indent = l.len() - l.trim_start().len();
                            match base_indent {
                                None => {
                                    if indent == 0 {
                                        // Non-indented, non-empty line → block scalar ended.
                                        break;
                                    }
                                    base_indent = Some(indent);
                                    collected.push(l[indent..].to_string());
                                }
                                Some(bi) => {
                                    if indent < bi {
                                        // Dedent below base → block ended. Let outer
                                        // loop re-process this line as normal frontmatter.
                                        break;
                                    }
                                    collected.push(l[bi..].to_string());
                                }
                            }
                            j += 1;
                        }
                        fm_description = if is_folded {
                            // Folded: blank line → paragraph break (newline),
                            // non-blank lines → joined with space.
                            let mut out = String::new();
                            let mut prev_blank = false;
                            for (i, s) in collected.iter().enumerate() {
                                if s.is_empty() {
                                    if !prev_blank && !out.is_empty() {
                                        out.push('\n');
                                    }
                                    prev_blank = true;
                                } else {
                                    if !out.is_empty() && !out.ends_with('\n') {
                                        out.push(' ');
                                    }
                                    out.push_str(s);
                                    prev_blank = false;
                                }
                                let _ = i;
                            }
                            out.trim().to_string()
                        } else {
                            collected.join("\n").trim().to_string()
                        };
                        idx = j;
                        continue;
                    } else if rest.is_empty() {
                        // `description:` with nothing after → skip (treat as empty)
                        fm_description = String::new();
                    } else {
                        // Plain scalar on same line
                        fm_description = rest.trim_matches('"').trim_matches('\'').to_string();
                    }
                }
                idx += 1;
            }

            if !fm_description.is_empty() {
                return fm_description.chars().take(200).collect();
            }

            let body_start = match fm_end {
                Some(e) => e + 1,
                None => return String::new(), // malformed frontmatter (no closing ---)
            };

            // No description in frontmatter — fall through to body text
            for line in &all[body_start..] {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                return trimmed.chars().take(200).collect();
            }
            String::new()
        } else {
            // No frontmatter — first non-empty non-heading line is the description.
            for line in &all {
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
        assert_eq!(
            desc,
            "Explores user intent and design before implementation."
        );
    }

    #[test]
    fn extract_description_no_frontmatter_reads_first_text_line() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("simple-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# My Skill\n\nThis skill does something useful.\n\nMore details here.\n",
        )
        .unwrap();

        let desc = Scanner::extract_description(&skill_dir);
        assert_eq!(desc, "This skill does something useful.");
    }

    #[test]
    fn extract_description_frontmatter_without_description_reads_body() {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("no-desc");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: no-desc\n---\n\n# No Description Skill\n\nBut this line explains it.\n",
        )
        .unwrap();

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
        let plugin = root
            .join("plugins")
            .join("marketplaces")
            .join("x")
            .join("skills")
            .join("foo");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(plugin.join("SKILL.md"), "# Plugin skill").unwrap();

        // Backup dir — should be filtered out
        let backup = root
            .join("backups")
            .join("20260325")
            .join("skills")
            .join("bar");
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

    /// A dangling symlink in a CLI skills dir whose basename matches an already-managed
    /// skill should be healed (redirected to the managed copy), not reported as an error.
    #[test]
    fn adopt_entry_heals_dangling_symlink_matching_managed_skill() {
        use crate::core::cli_target::CliTarget;
        use crate::core::db::Database;
        use crate::core::paths::AppPaths;

        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().join("data"));
        std::fs::create_dir_all(paths.skills_dir()).unwrap();
        let db = Database::open(&paths.data_dir().join("runai.db")).unwrap();

        // Managed skill already exists on disk.
        let name = "wt-sync";
        let managed = paths.skills_dir().join(name);
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::write(managed.join("SKILL.md"), "---\nname: wt-sync\n---\n").unwrap();

        // CLI dir has a dangling symlink with the same name.
        let cli_dir = tmp.path().join("cli").join("skills");
        std::fs::create_dir_all(&cli_dir).unwrap();
        let link = cli_dir.join(name);
        let dead_target = tmp.path().join("ghost/worktree-skill/skills/wt-sync");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dead_target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&dead_target, &link).unwrap();

        // Sanity: baseline state matches the bug's input.
        assert!(Linker::is_symlink(&link));
        assert!(!link.exists(), "target is supposed to be dangling");

        let outcome = Scanner::adopt_entry(&link, name, &paths, &db, CliTarget::Claude).unwrap();
        assert_eq!(outcome, AdoptOutcome::Healed);

        // After healing, the symlink must resolve to the managed dir.
        assert!(link.exists(), "symlink should now resolve");
        let resolved = std::fs::read_link(&link).unwrap();
        assert_eq!(resolved, managed, "link should point at managed dir");
    }

    /// A dangling symlink without a matching managed skill is an orphan. It should be
    /// left alone (not removed) and reported as skipped, not as an error.
    #[test]
    fn adopt_entry_skips_dangling_symlink_without_managed_match() {
        use crate::core::cli_target::CliTarget;
        use crate::core::db::Database;
        use crate::core::paths::AppPaths;

        let tmp = tempfile::tempdir().unwrap();
        let paths = AppPaths::with_base(tmp.path().join("data"));
        std::fs::create_dir_all(paths.skills_dir()).unwrap();
        let db = Database::open(&paths.data_dir().join("runai.db")).unwrap();

        let cli_dir = tmp.path().join("cli").join("skills");
        std::fs::create_dir_all(&cli_dir).unwrap();
        let link = cli_dir.join("unknown-skill");
        let dead_target = tmp.path().join("nowhere/unknown-skill");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dead_target, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&dead_target, &link).unwrap();

        let outcome =
            Scanner::adopt_entry(&link, "unknown-skill", &paths, &db, CliTarget::Claude).unwrap();
        assert_eq!(outcome, AdoptOutcome::Orphaned);

        // Orphan untouched: still a dangling symlink, still pointing at the dead target.
        assert!(Linker::is_symlink(&link));
        assert_eq!(std::fs::read_link(&link).unwrap(), dead_target);
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

    // ── YAML block-scalar description tests ──

    fn write_skill(dir: &Path, frontmatter: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), frontmatter).unwrap();
    }

    #[test]
    fn extract_description_handles_literal_block_scalar() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        write_skill(
            &dir,
            "---\nname: s\ndescription: |\n  First line of description.\n  Second line here.\nallowed-tools:\n  - Bash\n---\n",
        );
        let desc = Scanner::extract_description(&dir);
        // `|` preserves newlines between lines
        assert_eq!(desc, "First line of description.\nSecond line here.");
    }

    #[test]
    fn extract_description_handles_folded_block_scalar() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        write_skill(
            &dir,
            "---\nname: s\ndescription: >\n  First line of description.\n  Second line here.\n---\n",
        );
        let desc = Scanner::extract_description(&dir);
        // `>` joins consecutive lines with spaces
        assert_eq!(desc, "First line of description. Second line here.");
    }

    #[test]
    fn extract_description_handles_strip_chomp_indicator() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        write_skill(&dir, "---\nname: s\ndescription: |-\n  One\n  Two\n---\n");
        let desc = Scanner::extract_description(&dir);
        assert_eq!(desc, "One\nTwo");
    }

    #[test]
    fn extract_description_block_scalar_stops_at_dedent() {
        // When the following field is at the same or lesser indent, the block ends.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        write_skill(
            &dir,
            "---\nname: s\ndescription: |\n  Kept line.\nallowed-tools:\n  - Bash\n---\n",
        );
        let desc = Scanner::extract_description(&dir);
        assert_eq!(desc, "Kept line.");
    }

    #[test]
    fn extract_description_folded_handles_blank_line_paragraph_break() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        write_skill(
            &dir,
            "---\nname: s\ndescription: >\n  Para one line one.\n  Para one line two.\n\n  Para two.\n---\n",
        );
        let desc = Scanner::extract_description(&dir);
        // Blank line → paragraph break (newline); non-blank lines within a paragraph → joined by spaces
        assert_eq!(desc, "Para one line one. Para one line two.\nPara two.");
    }

    #[test]
    fn extract_description_quoted_inline_still_works() {
        // Regression: make sure we didn't break the existing quoted-string path.
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("s");
        write_skill(
            &dir,
            "---\nname: s\ndescription: \"Simple one-line description.\"\n---\n",
        );
        assert_eq!(
            Scanner::extract_description(&dir),
            "Simple one-line description."
        );
    }

    #[test]
    fn is_stale_description_catches_block_scalar_markers() {
        assert!(Scanner::is_stale_description(""));
        assert!(Scanner::is_stale_description("---"));
        assert!(Scanner::is_stale_description("|"));
        assert!(Scanner::is_stale_description(">"));
        assert!(Scanner::is_stale_description("|-"));
        assert!(Scanner::is_stale_description(">-"));
        assert!(Scanner::is_stale_description("|+"));
        assert!(Scanner::is_stale_description(">+"));
        assert!(Scanner::is_stale_description("  | "));
        assert!(!Scanner::is_stale_description("Actual description."));
    }
}

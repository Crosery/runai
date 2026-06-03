use super::Scanner;
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

impl Scanner {
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
}

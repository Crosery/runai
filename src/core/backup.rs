use std::path::{Path, PathBuf};
use anyhow::{Result, Context};

use crate::core::linker::Linker;
use crate::core::paths::AppPaths;

/// Backup format: ~/.skill-manager/backups/{timestamp}/
///   claude-skills/   <- copy of ~/.claude/skills/
///   claude.json      <- copy of ~/.claude.json
///   gemini-settings.json
///   codex-settings.json
///   opencode-settings.json
///   timestamp        <- file containing the timestamp string

const BACKUPS_DIR: &str = "backups";

/// Copy a directory, preserving symlinks as symlinks (not following them).
fn copy_dir_preserving_symlinks(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        let ft = entry.metadata()?.file_type();

        if ft.is_symlink() || Linker::is_symlink(&entry.path()) {
            // Copy the symlink itself
            let target = std::fs::read_link(entry.path())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &dest)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_dir(&target, &dest)?;
        } else if ft.is_dir() {
            copy_dir_preserving_symlinks(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

/// Create a timestamped backup of all CLI skill directories and config files.
/// Returns the backup directory path.
pub fn create_backup(paths: &AppPaths) -> Result<PathBuf> {
    create_backup_impl(paths, &dirs::home_dir().unwrap_or_default())
}

fn create_backup_impl(paths: &AppPaths, home: &Path) -> Result<PathBuf> {
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let backup_dir = paths.data_dir().join(BACKUPS_DIR).join(&ts);
    std::fs::create_dir_all(&backup_dir)?;

    // Backup each CLI's skills directory
    let cli_skill_dirs: &[(&str, &str)] = &[
        ("claude", ".claude/skills"),
        ("codex", ".codex/skills"),
        ("gemini", ".gemini/skills"),
        ("opencode", ".opencode/skills"),
    ];
    for (name, rel) in cli_skill_dirs {
        let skills_dir = home.join(rel);
        if skills_dir.exists() {
            let dest = backup_dir.join(format!("{name}-skills"));
            copy_dir_preserving_symlinks(&skills_dir, &dest)?;
        }
    }

    // Backup CLI config files
    let configs: &[(&str, &str)] = &[
        (".claude.json", "claude.json"),
        (".claude/settings.json", "claude-settings.json"),
        (".claude/settings.local.json", "claude-settings-local.json"),
        (".gemini/settings.json", "gemini-settings.json"),
        (".codex/settings.json", "codex-settings.json"),
        (".opencode/settings.json", "opencode-settings.json"),
    ];
    for (src_rel, dest_name) in configs {
        let src = home.join(src_rel);
        if src.exists() {
            let _ = std::fs::copy(&src, backup_dir.join(dest_name));
        }
    }

    // Backup MCP configs directory
    let mcp_configs = home.join(".claude/mcp-configs");
    if mcp_configs.exists() {
        copy_dir_preserving_symlinks(&mcp_configs, &backup_dir.join("claude-mcp-configs"))?;
    }

    // Write timestamp marker
    std::fs::write(backup_dir.join("timestamp"), &ts)?;

    Ok(backup_dir)
}

/// List all backups, newest first.
pub fn list_backups(paths: &AppPaths) -> Vec<String> {
    let dir = paths.data_dir().join(BACKUPS_DIR);
    if !dir.exists() { return Vec::new(); }
    let mut timestamps: Vec<String> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .collect();
    timestamps.sort_unstable();
    timestamps.reverse(); // newest first
    timestamps
}

/// Restore from a specific backup timestamp.
/// Copies skill dirs back, restores config files.
pub fn restore_backup(paths: &AppPaths, timestamp: &str) -> Result<usize> {
    restore_backup_impl(paths, timestamp, &dirs::home_dir().unwrap_or_default())
}

fn restore_backup_impl(paths: &AppPaths, timestamp: &str, home: &Path) -> Result<usize> {
    let backup_dir = paths.data_dir().join(BACKUPS_DIR).join(timestamp);
    if !backup_dir.exists() {
        anyhow::bail!("Backup not found: {timestamp}");
    }

    let mut restored = 0;

    let cli_skill_dirs: &[(&str, &str)] = &[
        ("claude", ".claude/skills"),
        ("codex", ".codex/skills"),
        ("gemini", ".gemini/skills"),
        ("opencode", ".opencode/skills"),
    ];
    for (name, rel) in cli_skill_dirs {
        let backup_skills = backup_dir.join(format!("{name}-skills"));
        if !backup_skills.exists() { continue; }

        let cli_skills = home.join(rel);
        if cli_skills.exists() {
            std::fs::remove_dir_all(&cli_skills)
                .with_context(|| format!("failed to remove {}", cli_skills.display()))?;
        }

        copy_dir_preserving_symlinks(&backup_skills, &cli_skills)?;
        restored += 1;
    }

    // Restore config files
    let configs: &[(&str, &str)] = &[
        ("claude.json", ".claude.json"),
        ("claude-settings.json", ".claude/settings.json"),
        ("claude-settings-local.json", ".claude/settings.local.json"),
        ("gemini-settings.json", ".gemini/settings.json"),
        ("codex-settings.json", ".codex/settings.json"),
        ("opencode-settings.json", ".opencode/settings.json"),
    ];
    for (backup_name, dest_rel) in configs {
        let src = backup_dir.join(backup_name);
        let dest = home.join(dest_rel);
        if src.exists() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&src, &dest)?;
            restored += 1;
        }
    }

    // Restore MCP configs directory
    let mcp_backup = backup_dir.join("claude-mcp-configs");
    if mcp_backup.exists() {
        let mcp_dest = home.join(".claude/mcp-configs");
        if mcp_dest.exists() {
            std::fs::remove_dir_all(&mcp_dest)?;
        }
        copy_dir_preserving_symlinks(&mcp_backup, &mcp_dest)?;
        restored += 1;
    }

    Ok(restored)
}

/// Check if any backup exists.
pub fn has_backup(paths: &AppPaths) -> bool {
    !list_backups(paths).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (TempDir, AppPaths) {
        let tmp = TempDir::new().unwrap();
        let paths = AppPaths::with_base(tmp.path().join("data"));
        paths.ensure_dirs().unwrap();
        (tmp, paths)
    }

    #[test]
    fn has_backup_returns_false_initially() {
        let (_tmp, paths) = setup();
        assert!(!has_backup(&paths));
    }

    #[test]
    fn create_backup_creates_timestamped_dir() {
        let (tmp, paths) = setup();
        // Create a fake home with skill dir
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".claude/skills/my-skill")).unwrap();
        std::fs::write(home.join(".claude/skills/my-skill/SKILL.md"), "# Test").unwrap();
        std::fs::write(home.join(".claude.json"), r#"{"mcpServers":{}}"#).unwrap();

        // Can't easily test with real CliTarget dirs, so test the impl directly
        // Just verify backup dir structure
        let backup_dir = create_backup_impl(&paths, &home).unwrap();
        assert!(backup_dir.exists());
        assert!(backup_dir.join("timestamp").exists());
        assert!(backup_dir.join("claude.json").exists());
    }

    #[test]
    fn list_backups_returns_newest_first() {
        let (_tmp, paths) = setup();
        let backups_dir = paths.data_dir().join(BACKUPS_DIR);
        std::fs::create_dir_all(backups_dir.join("20260101_120000")).unwrap();
        std::fs::create_dir_all(backups_dir.join("20260102_120000")).unwrap();
        std::fs::create_dir_all(backups_dir.join("20260101_180000")).unwrap();

        let list = list_backups(&paths);
        assert_eq!(list, vec!["20260102_120000", "20260101_180000", "20260101_120000"]);
    }

    #[test]
    fn restore_fails_for_nonexistent_timestamp() {
        let (_tmp, paths) = setup();
        let result = restore_backup(&paths, "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn backup_and_restore_roundtrip() {
        let (tmp, paths) = setup();
        let home = tmp.path().join("home");

        // Setup: create skill dir and config
        let skills = home.join(".claude/skills");
        std::fs::create_dir_all(skills.join("brainstorming")).unwrap();
        std::fs::write(skills.join("brainstorming/SKILL.md"), "# Original").unwrap();
        std::fs::write(home.join(".claude.json"), r#"{"original":true}"#).unwrap();

        // Backup
        let backup_dir = create_backup_impl(&paths, &home).unwrap();
        let ts = std::fs::read_to_string(backup_dir.join("timestamp")).unwrap();

        // Simulate damage: delete skill, modify config
        std::fs::remove_dir_all(&skills).unwrap();
        std::fs::write(home.join(".claude.json"), r#"{"modified":true}"#).unwrap();

        // Restore
        let restored = restore_backup_impl(&paths, &ts, &home).unwrap();
        assert!(restored > 0);

        // Verify config restored
        let config = std::fs::read_to_string(home.join(".claude.json")).unwrap();
        assert!(config.contains("original"));
    }

    #[test]
    fn has_backup_returns_true_after_create() {
        let (tmp, paths) = setup();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let _ = create_backup_impl(&paths, &home).unwrap();
        assert!(has_backup(&paths));
    }
}

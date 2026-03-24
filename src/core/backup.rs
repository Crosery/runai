use std::path::{Path, PathBuf};
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};

use crate::core::cli_target::CliTarget;
use crate::core::linker::Linker;
use crate::core::paths::AppPaths;

const BACKUP_FILE: &str = "backup.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupEntry {
    pub name: String,
    pub target: String,
    pub cli: String,
}

/// Scan all CLI skill directories and save a snapshot of existing symlinks
/// to `~/.skill-manager/backup.json`. Returns the number of entries saved.
pub fn create_backup(paths: &AppPaths) -> Result<usize> {
    create_backup_with_dirs(paths, None)
}

/// Like `create_backup`, but accepts an optional map of CLI -> skills dir overrides.
/// Used for testing.
pub fn create_backup_with_dirs(
    paths: &AppPaths,
    cli_dirs: Option<&[(CliTarget, PathBuf)]>,
) -> Result<usize> {
    let mut entries = Vec::new();

    let cli_iter: Vec<(CliTarget, PathBuf)> = match cli_dirs {
        Some(dirs) => dirs.to_vec(),
        None => CliTarget::ALL.iter().map(|c| (*c, c.skills_dir())).collect(),
    };

    for (cli, cli_dir) in &cli_iter {
        if !cli_dir.exists() {
            continue;
        }

        let read_dir = match std::fs::read_dir(cli_dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };

        for entry in read_dir {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if !Linker::is_symlink(&path) {
                continue;
            }

            // Skip symlinks that already point into our managed dir
            if Linker::is_our_symlink(&path, paths.data_dir()) {
                continue;
            }

            let name = match entry.file_name().to_str() {
                Some(n) => n.to_string(),
                None => continue,
            };

            let link_target = std::fs::read_link(&path)
                .with_context(|| format!("failed to read symlink: {}", path.display()))?;

            // Resolve to absolute path
            let resolved = if link_target.is_absolute() {
                link_target
            } else {
                path.parent()
                    .unwrap_or(Path::new("."))
                    .join(&link_target)
            };

            entries.push(BackupEntry {
                name,
                target: resolved.to_string_lossy().to_string(),
                cli: cli.name().to_string(),
            });
        }
    }

    let count = entries.len();
    let backup_path = paths.data_dir().join(BACKUP_FILE);
    let json = serde_json::to_string_pretty(&entries)
        .context("failed to serialize backup")?;
    std::fs::write(&backup_path, json)
        .with_context(|| format!("failed to write backup to {}", backup_path.display()))?;

    Ok(count)
}

/// Restore from backup: remove managed skill directories, recreate original symlinks.
/// Returns the number of entries restored.
pub fn restore_backup(paths: &AppPaths) -> Result<usize> {
    restore_backup_with_dirs(paths, None)
}

/// Like `restore_backup`, but accepts an optional map of CLI -> skills dir overrides.
pub fn restore_backup_with_dirs(
    paths: &AppPaths,
    cli_dirs: Option<&[(CliTarget, PathBuf)]>,
) -> Result<usize> {
    let backup_path = paths.data_dir().join(BACKUP_FILE);
    let content = std::fs::read_to_string(&backup_path)
        .with_context(|| format!("failed to read backup from {}", backup_path.display()))?;
    let entries: Vec<BackupEntry> = serde_json::from_str(&content)
        .context("failed to parse backup.json")?;

    let mut restored = 0;

    for entry in &entries {
        let cli = match CliTarget::from_str(&entry.cli) {
            Some(c) => c,
            None => continue,
        };

        let cli_dir = match cli_dirs {
            Some(dirs) => match dirs.iter().find(|(c, _)| *c == cli) {
                Some((_, d)) => d.clone(),
                None => cli.skills_dir(),
            },
            None => cli.skills_dir(),
        };

        let link_path = cli_dir.join(&entry.name);
        let managed_dir = paths.skills_dir().join(&entry.name);

        // Remove managed directory if it exists
        if managed_dir.exists() && !Linker::is_symlink(&managed_dir) {
            std::fs::remove_dir_all(&managed_dir)
                .with_context(|| format!("failed to remove managed dir: {}", managed_dir.display()))?;
        }

        // Remove existing symlink at CLI location
        if Linker::is_symlink(&link_path) {
            Linker::remove_link(&link_path)?;
        } else if link_path.exists() {
            std::fs::remove_dir_all(&link_path)
                .with_context(|| format!("failed to remove: {}", link_path.display()))?;
        }

        // Recreate original symlink
        let target_path = PathBuf::from(&entry.target);
        if target_path.exists() {
            std::fs::create_dir_all(&cli_dir)?;
            Linker::create_link(&target_path, &link_path)?;
            restored += 1;
        }
    }

    Ok(restored)
}

/// Check if a backup file exists.
pub fn has_backup(paths: &AppPaths) -> bool {
    paths.data_dir().join(BACKUP_FILE).exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_env() -> (TempDir, AppPaths, Vec<(CliTarget, PathBuf)>) {
        let tmp = TempDir::new().unwrap();
        let paths = AppPaths::with_base(tmp.path().join("data"));
        paths.ensure_dirs().unwrap();

        // Create fake CLI skill dirs inside temp
        let cli_dirs: Vec<(CliTarget, PathBuf)> = CliTarget::ALL.iter().map(|c| {
            let dir = tmp.path().join(format!("{}-skills", c.name()));
            std::fs::create_dir_all(&dir).unwrap();
            (*c, dir)
        }).collect();

        (tmp, paths, cli_dirs)
    }

    #[test]
    fn has_backup_returns_false_when_no_backup_exists() {
        let (_tmp, paths, _cli_dirs) = setup_test_env();
        assert!(!has_backup(&paths));
    }

    #[test]
    fn has_backup_returns_true_after_create_backup() {
        let (_tmp, paths, cli_dirs) = setup_test_env();
        create_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();
        assert!(has_backup(&paths));
    }

    #[test]
    fn create_backup_returns_zero_when_no_symlinks_in_cli_dirs() {
        let (_tmp, paths, cli_dirs) = setup_test_env();
        let count = create_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn create_backup_writes_valid_json() {
        let (_tmp, paths, cli_dirs) = setup_test_env();
        create_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();

        let content = std::fs::read_to_string(paths.data_dir().join(BACKUP_FILE)).unwrap();
        let entries: Vec<BackupEntry> = serde_json::from_str(&content).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn create_backup_captures_foreign_symlinks() {
        let (tmp, paths, cli_dirs) = setup_test_env();

        // Create a "foreign" skill directory outside managed area
        let foreign_skill = tmp.path().join("external-skills").join("my-skill");
        std::fs::create_dir_all(&foreign_skill).unwrap();
        std::fs::write(foreign_skill.join("SKILL.md"), "# Test").unwrap();

        // Create a symlink in the claude CLI dir pointing to the foreign skill
        let claude_dir = &cli_dirs[0].1; // claude
        let link_path = claude_dir.join("my-skill");
        Linker::create_link(&foreign_skill, &link_path).unwrap();

        let count = create_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();
        assert_eq!(count, 1);

        // Verify content
        let content = std::fs::read_to_string(paths.data_dir().join(BACKUP_FILE)).unwrap();
        let entries: Vec<BackupEntry> = serde_json::from_str(&content).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "my-skill");
        assert_eq!(entries[0].cli, "claude");
        assert_eq!(entries[0].target, foreign_skill.to_string_lossy());
    }

    #[test]
    fn create_backup_skips_our_own_symlinks() {
        let (_tmp, paths, cli_dirs) = setup_test_env();

        // Create a managed skill
        let managed_skill = paths.skills_dir().join("managed-skill");
        std::fs::create_dir_all(&managed_skill).unwrap();
        std::fs::write(managed_skill.join("SKILL.md"), "# Managed").unwrap();

        // Create a symlink in claude CLI dir pointing INTO our managed area
        let claude_dir = &cli_dirs[0].1;
        let link_path = claude_dir.join("managed-skill");
        Linker::create_link(&managed_skill, &link_path).unwrap();

        let count = create_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();
        assert_eq!(count, 0, "should skip symlinks pointing into our managed dir");
    }

    #[test]
    fn create_backup_skips_real_directories() {
        let (_tmp, paths, cli_dirs) = setup_test_env();

        // Create a real directory (not a symlink) in the claude CLI dir
        let claude_dir = &cli_dirs[0].1;
        let real_dir = claude_dir.join("real-skill");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::write(real_dir.join("SKILL.md"), "# Real").unwrap();

        let count = create_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();
        assert_eq!(count, 0, "should skip real directories, only backup symlinks");
    }

    #[test]
    fn restore_backup_recreates_original_symlinks() {
        let (tmp, paths, cli_dirs) = setup_test_env();

        // Create an "original" skill directory
        let original_dir = tmp.path().join("original").join("brainstorm");
        std::fs::create_dir_all(&original_dir).unwrap();
        std::fs::write(original_dir.join("SKILL.md"), "# Original").unwrap();

        // Create a managed copy
        let managed = paths.skills_dir().join("brainstorm");
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::write(managed.join("SKILL.md"), "# Managed copy").unwrap();

        // Write backup
        let entries = vec![BackupEntry {
            name: "brainstorm".to_string(),
            target: original_dir.to_string_lossy().to_string(),
            cli: "claude".to_string(),
        }];
        let backup_path = paths.data_dir().join(BACKUP_FILE);
        std::fs::write(&backup_path, serde_json::to_string_pretty(&entries).unwrap()).unwrap();

        // Restore
        let restored = restore_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();
        assert_eq!(restored, 1);

        // Verify: managed dir should be removed
        assert!(!managed.exists(), "managed dir should be removed");

        // Verify: symlink in claude dir should point to original
        let claude_dir = &cli_dirs[0].1;
        let link = claude_dir.join("brainstorm");
        assert!(Linker::is_symlink(&link), "symlink should be recreated");
        let target = std::fs::read_link(&link).unwrap();
        assert_eq!(target, original_dir);
    }

    #[test]
    fn restore_backup_skips_missing_originals() {
        let (_tmp, paths, cli_dirs) = setup_test_env();

        // Write backup pointing to non-existent directory
        let entries = vec![BackupEntry {
            name: "gone-skill".to_string(),
            target: "/nonexistent/path/gone-skill".to_string(),
            cli: "claude".to_string(),
        }];
        let backup_path = paths.data_dir().join(BACKUP_FILE);
        std::fs::write(&backup_path, serde_json::to_string_pretty(&entries).unwrap()).unwrap();

        let restored = restore_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();
        assert_eq!(restored, 0, "should not restore when original target is missing");
    }

    #[test]
    fn restore_backup_fails_gracefully_when_no_backup() {
        let (_tmp, paths, _cli_dirs) = setup_test_env();
        let result = restore_backup(&paths);
        assert!(result.is_err());
    }

    #[test]
    fn backup_entry_serialization_roundtrip() {
        let entry = BackupEntry {
            name: "brainstorming".to_string(),
            target: "/home/user/skills/brainstorming".to_string(),
            cli: "claude".to_string(),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: BackupEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(entry, deserialized);
    }

    #[test]
    fn create_backup_captures_multiple_clis() {
        let (tmp, paths, cli_dirs) = setup_test_env();

        // Create two foreign skills in different CLI dirs
        let skill_a = tmp.path().join("ext").join("skill-a");
        let skill_b = tmp.path().join("ext").join("skill-b");
        std::fs::create_dir_all(&skill_a).unwrap();
        std::fs::create_dir_all(&skill_b).unwrap();

        // Symlink in claude
        Linker::create_link(&skill_a, &cli_dirs[0].1.join("skill-a")).unwrap();
        // Symlink in codex
        Linker::create_link(&skill_b, &cli_dirs[1].1.join("skill-b")).unwrap();

        let count = create_backup_with_dirs(&paths, Some(&cli_dirs)).unwrap();
        assert_eq!(count, 2);

        let content = std::fs::read_to_string(paths.data_dir().join(BACKUP_FILE)).unwrap();
        let entries: Vec<BackupEntry> = serde_json::from_str(&content).unwrap();
        assert_eq!(entries.len(), 2);

        let claude_entry = entries.iter().find(|e| e.cli == "claude").unwrap();
        assert_eq!(claude_entry.name, "skill-a");

        let codex_entry = entries.iter().find(|e| e.cli == "codex").unwrap();
        assert_eq!(codex_entry.name, "skill-b");
    }
}

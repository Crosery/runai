use super::Scanner;
use super::registration::ScanResult;
use crate::core::cli_target::CliTarget;
use crate::core::db::Database;
use crate::core::linker::{EntryType, Linker};
use crate::core::paths::AppPaths;
use crate::core::resource::{Resource, ResourceKind, Source};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;

/// What happened to a single entry during adoption.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AdoptOutcome {
    /// Newly moved into the managed dir and linked back.
    Adopted,
    /// Dangling symlink whose name matched an already-managed skill — link redirected.
    Healed,
    /// Dangling symlink with no managed counterpart — left alone, counted as skipped.
    Orphaned,
}

impl Scanner {
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
                        Ok(outcome @ (AdoptOutcome::Adopted | AdoptOutcome::Healed)) => {
                            result.adopted += 1;
                            // `Adopted` means a new skill row was created;
                            // `Healed` means we just re-pointed a dangling
                            // symlink at an already-managed skill. Only the
                            // first needs fresh enrichment — the healed case
                            // already has its summary from the prior adopt.
                            if outcome == AdoptOutcome::Adopted {
                                result.adopted_names.push(name.clone());
                            }
                        }
                        Ok(AdoptOutcome::Orphaned) => result.skipped += 1,
                        Err(e) => result.errors.push(format!("{name}: {e}")),
                    }
                }
                EntryType::NotExists => continue,
            }
        }

        Ok(result)
    }

    pub(super) fn adopt_entry(
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
                // Not a skill — could be a bundle container (e.g. codex's
                // `codex-primary-runtime/{slides,spreadsheets}/SKILL.md`),
                // metadata dir, or unrelated content. Silently skip rather
                // than erroring: scanner is supposed to ignore non-skills,
                // and surfacing this as `errors:` confuses users into
                // thinking something broke.
                return Ok(AdoptOutcome::Orphaned);
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
            owner_user_id: None,
            publish_status: "draft".to_string(),
        };

        db.insert_resource(&resource)?;
        Ok(AdoptOutcome::Adopted)
    }
}

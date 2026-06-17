//! Real-time skill enrichment watcher (PLANNING real-time enrichment).
//!
//! The dashboard `server` runs this over the runai data dir so that editing a
//! `SKILL.md`, or a new skill landing in the public pool / a user's private
//! pool, AUTO-triggers enrichment instead of waiting for the next SessionStart
//! `recommend enrich` pass. Until this existed nothing reacted to an in-place
//! SKILL.md edit — `config_watcher` is TUI-only, NonRecursive, and content-blind.
//!
//! ## What it watches
//! `<data>/skills/` (public pool) and `<data>/users/` (all per-user private
//! pools, present and future) — **Recursive**, so a `<name>/SKILL.md` content
//! change deep in the tree fires, not just top-level dir listing changes.
//!
//! ## Mechanism
//! `notify-debouncer-mini` with a 300 ms debounce collapses editor save bursts.
//! Each batch is mapped to the set of affected skill NAMES via
//! `skill_name_for_path`, deduped, and handed to the caller's `on_batch` once
//! per batch (NOT per file) — so a 50-file change becomes one
//! `recommend enrich --name … --name …` invocation, not 50 (the free-model
//! rate-limit guardrail; see project memory).
//!
//! ## Public surface
//! - `SkillWatcher::start(data_dir, on_batch) -> Result<Self>` — caller MUST hold
//!   the returned value; dropping it stops the watcher. `on_batch(&[String])`
//!   runs on the debouncer thread.
//! - `skill_name_for_path(path, public_root, users_root) -> Option<String>` —
//!   pure path→skill-name resolver, exposed for tests.
//!
//! ## Invariants
//! - Read-only: the watcher never mutates the FS; the only side effect is the
//!   `on_batch` callback (which the server uses to spawn enrichment).
//! - Does NOT watch `<data>/runai.db` (enrichment writes summaries there; that
//!   would be a feedback loop). Only the skill pools under `skills/` + `users/`.

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Holds the live notify Debouncer. Drop to stop watching.
pub struct SkillWatcher {
    _debouncer: Debouncer<RecommendedWatcher>,
    /// Roots actually registered (missing ones skipped), for diagnostics/tests.
    pub watched: Vec<PathBuf>,
}

impl SkillWatcher {
    pub fn start<F>(data_dir: PathBuf, on_batch: F) -> Result<Self>
    where
        F: Fn(&[String]) + Send + 'static,
    {
        let public = data_dir.join("skills");
        let users = data_dir.join("users");
        // Match against BOTH the raw roots and their canonicalized form. On
        // macOS FSEvents reports canonical paths (`/private/var/...`) while our
        // roots come from `$HOME` (`/var/...`, a symlink), so a raw-only
        // `strip_prefix` silently fails in tests / any symlinked HOME. On Linux
        // inotify echoes the raw watched path, so the raw root is what matches.
        // Trying both covers every platform.
        let raw = (public.clone(), users.clone());
        let canon = (
            public.canonicalize().unwrap_or_else(|_| public.clone()),
            users.canonicalize().unwrap_or_else(|_| users.clone()),
        );

        let mut debouncer = new_debouncer(
            Duration::from_millis(300),
            move |res: DebounceEventResult| {
                let Ok(events) = res else {
                    return;
                };
                let mut seen = HashSet::new();
                let mut names = Vec::new();
                for ev in events {
                    let name = skill_name_for_path(&ev.path, &canon.0, &canon.1)
                        .or_else(|| skill_name_for_path(&ev.path, &raw.0, &raw.1));
                    if let Some(name) = name
                        && seen.insert(name.clone())
                    {
                        names.push(name);
                    }
                }
                if !names.is_empty() {
                    on_batch(&names);
                }
            },
        )?;

        let mut watched = Vec::new();
        for root in [public, users] {
            if root.exists()
                && debouncer
                    .watcher()
                    .watch(&root, RecursiveMode::Recursive)
                    .is_ok()
            {
                watched.push(root);
            }
        }

        Ok(Self {
            _debouncer: debouncer,
            watched,
        })
    }
}

/// Resolve which skill a changed path belongs to.
/// - public pool: `<public_root>/<name>/...`            → `Some(name)`
/// - private pool: `<users_root>/<uid>/skills/<name>/...` → `Some(name)`
/// - anything else (e.g. a user's `trash/`, the `users/<uid>/` dir itself) → None
pub fn skill_name_for_path(path: &Path, public_root: &Path, users_root: &Path) -> Option<String> {
    if let Ok(rel) = path.strip_prefix(public_root) {
        return first_normal(rel);
    }
    if let Ok(rel) = path.strip_prefix(users_root) {
        // <uid>/skills/<name>/...
        let parts: Vec<String> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        if parts.len() >= 3 && parts[1] == "skills" {
            return Some(parts[2].clone());
        }
        return None;
    }
    None
}

fn first_normal(rel: &Path) -> Option<String> {
    rel.components().find_map(|c| match c {
        Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_public_skill_name() {
        let pubr = PathBuf::from("/data/skills");
        let usr = PathBuf::from("/data/users");
        assert_eq!(
            skill_name_for_path(Path::new("/data/skills/foo/SKILL.md"), &pubr, &usr),
            Some("foo".into())
        );
        // nested file under the skill dir still maps to the skill
        assert_eq!(
            skill_name_for_path(Path::new("/data/skills/foo/scripts/run.sh"), &pubr, &usr),
            Some("foo".into())
        );
    }

    #[test]
    fn resolves_private_skill_name() {
        let pubr = PathBuf::from("/data/skills");
        let usr = PathBuf::from("/data/users");
        assert_eq!(
            skill_name_for_path(
                Path::new("/data/users/usr_a/skills/bar/SKILL.md"),
                &pubr,
                &usr
            ),
            Some("bar".into())
        );
    }

    #[test]
    fn ignores_non_skill_paths() {
        let pubr = PathBuf::from("/data/skills");
        let usr = PathBuf::from("/data/users");
        // a user's trash, not a skill
        assert_eq!(
            skill_name_for_path(Path::new("/data/users/usr_a/trash/x/SKILL.md"), &pubr, &usr),
            None
        );
        // the users/<uid> dir itself
        assert_eq!(
            skill_name_for_path(Path::new("/data/users/usr_a"), &pubr, &usr),
            None
        );
        // unrelated path
        assert_eq!(
            skill_name_for_path(Path::new("/etc/passwd"), &pubr, &usr),
            None
        );
    }
}

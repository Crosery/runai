//! Filesystem-event watcher for CLI MCP configs and skill directories.
//!
//! Fires reload signals to the TUI whenever any watched path changes. Replaces
//! the v0.10.0 mtime-polling `tui/app.rs::poll_config_changes`, which had 4
//! hardcoded paths — two of them wrong (`.codex/settings.json`,
//! `.opencode/settings.json`), so Codex / OpenCode edits never refreshed.
//!
//! ## Watched paths (via `watch_targets`)
//! 4 CLI MCP config files (`~/.claude.json`, `~/.codex/config.toml`,
//! `~/.gemini/settings.json`, `~/.config/opencode/opencode.json`), 4 skill dirs
//! (`~/.{claude,codex,gemini,opencode}/skills/`), and the runai backup dir
//! `~/.runai/mcps/` (catches cross-shell disable/enable). All paths come from
//! `CliTarget::mcp_config_path()` / `skills_dir()` — single source of truth, no
//! hand-coded duplicates. Missing paths are silently skipped (an uninstalled
//! CLI doesn't break the watcher).
//!
//! ## Mechanism
//! `notify-debouncer-mini` over `RecommendedWatcher` (FSEvents/inotify/
//! ReadDirectoryChangesW). A 200 ms debounce collapses editor save bursts; on
//! any event the callback sends `()` on the caller's `mpsc::Sender`. TUI's main
//! loop drains the receiver before each redraw and triggers one `App::reload()`
//! per batch.
//!
//! ## Public surface
//! - `ConfigWatcher::start(Sender<()>) -> Result<Self>` — caller MUST hold the
//!   returned value; dropping it tears down the watcher.
//! - `ConfigWatcher::watched: Vec<PathBuf>` — paths actually registered (missing
//!   excluded), for diagnostics.
//! - `watch_targets() -> Vec<PathBuf>` — full intent list regardless of existence.
//! - `is_watched(&Path) -> bool` — test helper.
//!
//! ## Invariants
//! - Read-only: the only side effect is `Sender::send`; it never mutates the FS.
//! - All paths use `NonRecursive` mode (a new `<name>/SKILL.md` still fires an
//!   event on its parent dir's listing).
//! - Receiver-side coalescing is the caller's job: drain the channel before
//!   reloading so N rapid changes collapse to 1 reload, not N.
//! - Does NOT watch `~/.runai/runai.db` or skill content files — that would
//!   re-render the TUI on every keystroke during scan / edit.

use crate::core::cli_target::CliTarget;
use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{Debouncer, new_debouncer};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

/// Holds the live notify Debouncer. Drop to stop watching.
pub struct ConfigWatcher {
    _debouncer: Debouncer<RecommendedWatcher>,
    pub watched: Vec<PathBuf>,
}

impl ConfigWatcher {
    /// Start watching: 4 CLI MCP config files + 4 skill directories + the runai
    /// data dir's `mcps/`. Missing paths are silently skipped — re-running on
    /// startup picks them up if user installs a CLI later.
    pub fn start(sender: Sender<()>) -> Result<Self> {
        let mut debouncer = new_debouncer(Duration::from_millis(200), move |res| {
            // Either Ok(_events) or Err(_errors); both signal "something happened".
            // We don't differentiate — TUI just reloads from disk.
            let _ = sender.send(());
            drop(res);
        })?;

        // NonRecursive everywhere: for files it's the only mode that makes sense;
        // for skill / mcp dirs we only need new-child events on the dir itself
        // (a new <name>/SKILL.md triggers an event on the parent dir's listing).
        let mut watched = Vec::new();
        for path in watch_targets() {
            if !path.exists() {
                continue;
            }
            if debouncer
                .watcher()
                .watch(&path, RecursiveMode::NonRecursive)
                .is_ok()
            {
                watched.push(path);
            }
        }

        Ok(Self {
            _debouncer: debouncer,
            watched,
        })
    }
}

/// All filesystem paths that should fire reload events.
pub fn watch_targets() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return out,
    };

    // 4 CLI MCP config files (use the same path resolver as manager → ground truth).
    for target in CliTarget::ALL {
        out.push(target.mcp_config_path());
    }

    // skills/ directories: watch each CLI's skills dir so a new SKILL.md
    // appearing under it triggers a reload (TUI shows it as adopted-pending).
    for target in CliTarget::ALL {
        out.push(target.skills_dir());
    }

    // runai's own MCP backup dir — disable/enable from another shell should refresh TUI.
    let mcps = home.join(".runai").join("mcps");
    out.push(mcps);

    out
}

/// True if `path` is one we register with notify. Used by tests.
pub fn is_watched(path: &Path) -> bool {
    watch_targets().iter().any(|p| p == path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    #[test]
    fn watch_targets_includes_four_cli_configs() {
        let targets = watch_targets();
        // Each CliTarget contributes one mcp_config_path. Verify all 4 are present.
        let configs: Vec<_> = CliTarget::ALL.iter().map(|t| t.mcp_config_path()).collect();
        for c in &configs {
            assert!(targets.contains(c), "missing {:?}", c);
        }
    }

    #[test]
    fn watch_targets_includes_four_skill_dirs() {
        let targets = watch_targets();
        for t in CliTarget::ALL {
            let s = t.skills_dir();
            assert!(targets.contains(&s));
        }
    }

    #[test]
    fn watcher_fires_on_file_modify() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("x.json");
        std::fs::write(&file, r#"{"a":1}"#).unwrap();

        let (tx, rx) = mpsc::channel();
        let mut deb = new_debouncer(Duration::from_millis(100), move |_res| {
            let _ = tx.send(());
        })
        .unwrap();
        deb.watcher()
            .watch(&file, RecursiveMode::NonRecursive)
            .unwrap();

        // Modify
        std::fs::write(&file, r#"{"a":2}"#).unwrap();

        // Wait up to 1 s for the debounced event
        let start = Instant::now();
        let mut got = false;
        while start.elapsed() < Duration::from_secs(1) {
            if rx.recv_timeout(Duration::from_millis(150)).is_ok() {
                got = true;
                break;
            }
        }
        assert!(got, "watcher did not emit an event for file modify");
    }
}

//! Filesystem-event watcher for CLI MCP configs and skill directories.
//!
//! Replaces TUI's old `poll_config_changes` mtime polling. Whenever any of the
//! watched paths fires a filesystem event (modify / create / remove), the
//! watcher debounces for 200 ms then emits a single `()` on its `mpsc::Sender`.
//! TUI's main loop drains the receiver before each redraw and triggers
//! `App::reload()` on any pending signal.
//!
//! Dropping the returned `ConfigWatcher` stops the watcher cleanly.

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

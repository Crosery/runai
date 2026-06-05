//! Hooks panel state + keyboard dispatch (PLANNING.md §1.5).
//!
//! Renders a 4-row table — one row per `CliTarget` — showing whether each
//! CLI's router hook is currently installed. Today only Claude has the
//! `UserPromptSubmit` contract that `runai recommend` plugs into; the
//! other three rows are intentionally rendered as **unsupported** so the
//! panel surfaces the full per-CLI matrix to the user. When Codex /
//! Gemini / OpenCode grow a router hook protocol the slot flips from
//! `HookSlot::unsupported` to `HookSlot::supported(installed)` and the
//! existing toggle path Just Works.
//!
//! Side effects flow through `crate::core::recommend::{install_claude_hook,
//! uninstall_claude_hook}` — the same code paths `runai recommend
//! install-hook` / `uninstall-hook` use. So toggling from the TUI lands at
//! exactly the same `~/.claude/settings.json` shape the CLI produces;
//! there is no second writer.
//!
//! Reload contract: `App::reload_hook_status` is called from
//! `data::reload()` only when the Hooks tab is active (cheap — one
//! `read + parse` of `~/.claude/settings.json`). Switching to the tab
//! triggers reload via the existing tab-switch path so the panel never
//! shows stale state.

use super::model::{App, HookSlot, Tab};
use crate::core::cli_target::CliTarget;
use crate::core::recommend::{HookInstallStatus, install_claude_hook, uninstall_claude_hook};
use crossterm::event::{KeyCode, KeyEvent};

impl App {
    /// Re-read each target's hook state from disk. Currently only Claude
    /// is wired to a router hook; the other targets stay marked
    /// unsupported. Best-effort: errors degrade gracefully to "unknown =
    /// unsupported" so the panel never panics on a malformed
    /// settings.json.
    pub fn reload_hook_status(&mut self) {
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => return,
        };
        let claude_installed = claude_hook_installed(&home);
        self.hook_status
            .insert(CliTarget::Claude, HookSlot::supported(claude_installed));
        for t in CliTarget::ALL {
            if !matches!(t, CliTarget::Claude) {
                self.hook_status.insert(*t, HookSlot::unsupported());
            }
        }
        // Keep the cursor inside the bounds of the matrix.
        if self.hook_panel_idx >= CliTarget::ALL.len() {
            self.hook_panel_idx = CliTarget::ALL.len().saturating_sub(1);
        }
    }

    /// Dispatch key events while the Hooks tab is active. Lives outside
    /// `normal_actions.rs` so the panel-specific logic (toggle install,
    /// message text) stays grouped with the side-effect functions.
    pub(super) fn handle_hooks_key(&mut self, key: KeyEvent) -> bool {
        if self.tab != Tab::Hooks {
            return false;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.hook_panel_idx + 1 < CliTarget::ALL.len() {
                    self.hook_panel_idx += 1;
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.hook_panel_idx > 0 {
                    self.hook_panel_idx -= 1;
                }
                true
            }
            KeyCode::Char('g') => {
                self.hook_panel_idx = 0;
                true
            }
            KeyCode::Char('G') => {
                self.hook_panel_idx = CliTarget::ALL.len().saturating_sub(1);
                true
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                self.toggle_selected_hook();
                true
            }
            _ => false,
        }
    }

    /// Apply install / uninstall for the highlighted target.
    fn toggle_selected_hook(&mut self) {
        let target = match CliTarget::ALL.get(self.hook_panel_idx) {
            Some(t) => *t,
            None => return,
        };
        let slot = self
            .hook_status
            .get(&target)
            .copied()
            .unwrap_or_else(HookSlot::unsupported);
        if !slot.supported {
            self.message = Some(format!(
                "Hook 不支持 {}: 仅 Claude 已对接 runai router",
                target.name()
            ));
            return;
        }
        let home = match dirs::home_dir() {
            Some(h) => h,
            None => {
                self.message = Some("无法解析 home 目录".into());
                return;
            }
        };
        let outcome = if slot.installed {
            match uninstall_claude_hook(&home) {
                Ok(HookInstallStatus::Removed) => Ok(("已卸载", false)),
                Ok(HookInstallStatus::NotPresent) => Ok(("未安装", false)),
                Ok(_) => Ok(("无变化", slot.installed)),
                Err(e) => Err(e),
            }
        } else {
            match install_claude_hook(&home) {
                Ok(HookInstallStatus::Installed) => Ok(("已安装", true)),
                Ok(HookInstallStatus::AlreadyPresent) => Ok(("已存在", true)),
                Ok(_) => Ok(("无变化", slot.installed)),
                Err(e) => Err(e),
            }
        };
        match outcome {
            Ok((label, new_state)) => {
                self.hook_status
                    .insert(target, HookSlot::supported(new_state));
                self.message = Some(format!("{} hook on {}", label, target.name()));
            }
            Err(e) => self.message = Some(format!("操作失败: {e}")),
        }
    }
}

/// Detect whether the `runai recommend` UserPromptSubmit hook is present
/// in `~/.claude/settings.json`. Returns false on every failure mode
/// (missing file, JSON parse error, missing keys) — the TUI is allowed to
/// re-install on top, that path is idempotent.
fn claude_hook_installed(home: &std::path::Path) -> bool {
    let path = home.join(".claude").join("settings.json");
    let txt = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return false,
    };
    if txt.trim().is_empty() {
        return false;
    }
    let value: serde_json::Value = match serde_json::from_str(&txt) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let ups = match value
        .get("hooks")
        .and_then(|h| h.get("UserPromptSubmit"))
        .and_then(|a| a.as_array())
    {
        Some(a) => a,
        None => return false,
    };
    ups.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|arr| {
                arr.iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some("runai recommend"))
            })
    })
}

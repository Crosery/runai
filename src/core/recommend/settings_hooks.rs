//! Claude Code `~/.claude/settings.json` hook install / uninstall.
//!
//! Manages the `UserPromptSubmit` router hook (and a `SessionStart` hook for
//! `runai server --ensure`). All mutations are identified by command-string
//! equality so re-running is idempotent and uninstall only removes our entry;
//! unrelated user hooks and the rest of the file are preserved, with a
//! `.runai-bak` written before each write.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Result of attempting to install the UserPromptSubmit hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookInstallStatus {
    Installed,
    AlreadyPresent,
    Removed,
    NotPresent,
}

const HOOK_COMMAND: &str = "runai recommend";
/// Legacy command string kept for uninstall-time cleanup. Older runai
/// versions wrote this entry into PostToolUse; new installs no longer do.
const LEGACY_POST_TOOL_HOOK_COMMAND: &str = "runai recommend post-tool";

/// Install the UserPromptSubmit hook into `<home>/.claude/settings.json`.
/// The hook runs the router for each user prompt and injects the chosen
/// skills as additional context. Idempotent.
///
/// As a side-effect, any legacy `runai recommend post-tool` entry in the
/// PostToolUse array is removed: counting now flows exclusively through
/// `runai recommend get`, so the PostToolUse path is no longer wired.
pub fn install_claude_hook(home: &Path) -> Result<HookInstallStatus> {
    let claude_dir = home.join(".claude");
    let path = claude_dir.join("settings.json");
    let mut value = read_settings_json(&path)?;

    let ups_arr = ensure_user_prompt_submit_array(&mut value)?;
    let ups_already = hook_already_present(ups_arr);
    if !ups_already {
        ups_arr.push(serde_json::json!({
            "hooks": [
                {"type": "command", "command": HOOK_COMMAND}
            ]
        }));
    }

    let legacy_removed = remove_legacy_post_tool_hook(&mut value);

    if ups_already && !legacy_removed {
        return Ok(HookInstallStatus::AlreadyPresent);
    }
    write_settings_json(&path, &value)?;
    Ok(HookInstallStatus::Installed)
}

/// Strip any historical `runai recommend post-tool` entry from
/// settings.json. Returns true if something was actually removed.
fn remove_legacy_post_tool_hook(value: &mut serde_json::Value) -> bool {
    let arr = match get_named_hook_array(value, "PostToolUse") {
        Some(a) => a,
        None => return false,
    };
    let before = arr.len();
    arr.retain(|group| {
        let hooks = match group.get("hooks").and_then(|h| h.as_array()) {
            Some(h) => h,
            None => return true,
        };
        let all_legacy = !hooks.is_empty()
            && hooks.iter().all(|h| {
                h.get("command").and_then(|c| c.as_str()) == Some(LEGACY_POST_TOOL_HOOK_COMMAND)
            });
        !all_legacy
    });
    arr.len() != before
}

/// Remove the runai-installed hook from settings.json. Leaves unrelated hook
/// entries (and the rest of the file) untouched.
pub fn uninstall_claude_hook(home: &Path) -> Result<HookInstallStatus> {
    let path = home.join(".claude").join("settings.json");
    if !path.exists() {
        return Ok(HookInstallStatus::NotPresent);
    }
    let mut value = read_settings_json(&path)?;
    let ups_arr = match get_user_prompt_submit_array(&mut value) {
        Some(arr) => arr,
        None => return Ok(HookInstallStatus::NotPresent),
    };
    let before = ups_arr.len();
    ups_arr.retain(|group| {
        let arr = match group.get("hooks").and_then(|h| h.as_array()) {
            Some(a) => a,
            None => return true,
        };
        // Drop the whole group only if every hook inside it is ours.
        let all_ours = !arr.is_empty()
            && arr
                .iter()
                .all(|h| h.get("command").and_then(|c| c.as_str()) == Some(HOOK_COMMAND));
        !all_ours
    });
    if ups_arr.len() == before {
        return Ok(HookInstallStatus::NotPresent);
    }
    write_settings_json(&path, &value)?;
    Ok(HookInstallStatus::Removed)
}

/// Install or remove a `SessionStart` hook in `~/.claude/settings.json` that
/// runs `command_str` (e.g. `runai server --ensure`) every time Claude Code
/// starts a new session. The user's other SessionStart hooks are preserved.
///
/// Identification: we match by command-string equality so re-running the
/// installer is a no-op and uninstall only removes our entry.
pub fn install_session_start_hook(home: &Path, command_str: &str) -> Result<HookInstallStatus> {
    let path = home.join(".claude").join("settings.json");
    let mut value = read_settings_json(&path)?;
    let arr = ensure_named_hook_array(&mut value, "SessionStart")?;
    if hook_command_present(arr, command_str) {
        return Ok(HookInstallStatus::AlreadyPresent);
    }
    arr.push(serde_json::json!({
        "hooks": [{"type": "command", "command": command_str}]
    }));
    write_settings_json(&path, &value)?;
    Ok(HookInstallStatus::Installed)
}

pub fn uninstall_session_start_hook(home: &Path, command_str: &str) -> Result<HookInstallStatus> {
    let path = home.join(".claude").join("settings.json");
    if !path.exists() {
        return Ok(HookInstallStatus::NotPresent);
    }
    let mut value = read_settings_json(&path)?;
    let arr = match get_named_hook_array(&mut value, "SessionStart") {
        Some(a) => a,
        None => return Ok(HookInstallStatus::NotPresent),
    };
    let before = arr.len();
    arr.retain(|group| {
        let h = match group.get("hooks").and_then(|h| h.as_array()) {
            Some(a) => a,
            None => return true,
        };
        let all_ours = !h.is_empty()
            && h.iter()
                .all(|x| x.get("command").and_then(|c| c.as_str()) == Some(command_str));
        !all_ours
    });
    if arr.len() == before {
        return Ok(HookInstallStatus::NotPresent);
    }
    write_settings_json(&path, &value)?;
    Ok(HookInstallStatus::Removed)
}

fn ensure_named_hook_array<'a>(
    value: &'a mut serde_json::Value,
    name: &str,
) -> Result<&'a mut Vec<serde_json::Value>> {
    let obj = value
        .as_object_mut()
        .context("settings.json root must be an object")?;
    let hooks = obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks
        .as_object_mut()
        .context("settings.json `hooks` field must be an object")?;
    let entry = hooks_obj
        .entry(name.to_string())
        .or_insert_with(|| serde_json::json!([]));
    entry
        .as_array_mut()
        .with_context(|| format!("settings.json `hooks.{name}` must be an array"))
}

fn get_named_hook_array<'a>(
    value: &'a mut serde_json::Value,
    name: &str,
) -> Option<&'a mut Vec<serde_json::Value>> {
    value
        .as_object_mut()?
        .get_mut("hooks")?
        .as_object_mut()?
        .get_mut(name)?
        .as_array_mut()
}

fn hook_command_present(arr: &[serde_json::Value], command_str: &str) -> bool {
    arr.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|hs| {
                hs.iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(command_str))
            })
    })
}

fn read_settings_json(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let txt = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    if txt.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&txt).with_context(|| format!("parse {} as JSON", path.display()))
}

fn ensure_user_prompt_submit_array(
    value: &mut serde_json::Value,
) -> Result<&mut Vec<serde_json::Value>> {
    let obj = value
        .as_object_mut()
        .context("settings.json root must be an object")?;
    let hooks_entry = obj
        .entry("hooks".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let hooks_obj = hooks_entry
        .as_object_mut()
        .context("settings.json `hooks` field must be an object")?;
    let ups = hooks_obj
        .entry("UserPromptSubmit".to_string())
        .or_insert_with(|| serde_json::json!([]));
    ups.as_array_mut()
        .context("settings.json `hooks.UserPromptSubmit` must be an array")
}

fn get_user_prompt_submit_array(
    value: &mut serde_json::Value,
) -> Option<&mut Vec<serde_json::Value>> {
    value
        .as_object_mut()?
        .get_mut("hooks")?
        .as_object_mut()?
        .get_mut("UserPromptSubmit")?
        .as_array_mut()
}

fn hook_already_present(ups_arr: &[serde_json::Value]) -> bool {
    ups_arr.iter().any(|group| {
        group
            .get("hooks")
            .and_then(|h| h.as_array())
            .is_some_and(|arr| {
                arr.iter()
                    .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(HOOK_COMMAND))
            })
    })
}

fn write_settings_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    if path.exists() {
        let bak = path.with_extension("json.runai-bak");
        let _ = fs::copy(path, &bak);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let pretty = serde_json::to_string_pretty(value)?;
    fs::write(path, pretty).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

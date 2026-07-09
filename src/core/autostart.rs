//! OS-level login auto-start for the runai dashboard server.
//!
//! - **macOS** → user LaunchAgent plist at `~/Library/LaunchAgents/cn.crosery.runai.plist`,
//!   loaded with `launchctl load -w` so it starts at every login and is
//!   relaunched on crash via `KeepAlive`.
//! - **Linux** → systemd user unit at `~/.config/systemd/user/runai.service`,
//!   enabled with `systemctl --user enable --now` so it starts at user
//!   session login and gets `Restart=on-failure`.
//! - **Windows** → not implemented; returns an error pointing to Task Scheduler.
//!
//! All install/uninstall is idempotent. The plist / service unit is
//! always rewritten with the current binary's absolute path so a
//! `cargo install --force runai` upgrade picks up the new path on next
//! `runai server --install-autostart` call.
//!
//! `install(host, port, mode)` accepts a `ServerMode` and injects
//! `--mode owner|team` into the relaunch command line so the boot-time
//! server uses the same identity model the operator opted into at install
//! time (PLANNING §1.1). Switching modes = re-run `--install-autostart`
//! with the new `--mode` flag.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;
use std::process::Command;

use crate::core::server_mode::ServerMode;

#[derive(Debug)]
pub enum AutostartStatus {
    /// Installed for the first time (file did not previously exist).
    Installed { path: PathBuf },
    /// Overwrote an existing autostart file (idempotent re-install).
    Reinstalled { path: PathBuf },
    /// Removed an existing autostart file.
    Uninstalled { path: PathBuf },
    /// Nothing was installed; uninstall was a no-op.
    NotInstalled,
}

pub fn install(host: &str, port: u16, mode: ServerMode) -> Result<AutostartStatus> {
    let exe = std::env::current_exe()
        .context("locate runai binary via current_exe")?
        .canonicalize()
        .context("canonicalize runai binary path")?;

    #[cfg(target_os = "macos")]
    {
        install_macos(&exe, host, port, mode)
    }
    #[cfg(target_os = "linux")]
    {
        install_linux(&exe, host, port, mode)
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (exe, host, port, mode);
        bail!(
            "Windows auto-start is not yet implemented. Use Task Scheduler:\n\
             1) Open `taskschd.msc`\n\
             2) Create Task → Triggers → At log on\n\
             3) Actions → Start a program → `<path to runai.exe>` with args `server --port {port} --host {host} --mode {mode}`"
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (exe, host, port, mode);
        bail!("auto-start unsupported on this platform")
    }
}

pub fn uninstall() -> Result<AutostartStatus> {
    #[cfg(target_os = "macos")]
    {
        uninstall_macos()
    }
    #[cfg(target_os = "linux")]
    {
        uninstall_linux()
    }
    #[cfg(target_os = "windows")]
    {
        bail!(
            "Windows auto-start is not managed by runai; remove the scheduled task manually via `taskschd.msc`"
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        bail!("auto-start unsupported on this platform")
    }
}

// ────────────────────────────── macOS (launchd) ──────────────────────────────

#[cfg(target_os = "macos")]
const MACOS_LABEL: &str = "cn.crosery.runai";

#[cfg(target_os = "macos")]
fn macos_plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolve home dir")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{MACOS_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn install_macos(
    exe: &std::path::Path,
    host: &str,
    port: u16,
    mode: ServerMode,
) -> Result<AutostartStatus> {
    let plist_path = macos_plist_path()?;
    let already = plist_path.exists();
    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }

    // Best-effort unload of any older copy so the new ProgramArguments
    // (binary path / port / host) take effect immediately.
    if already {
        let _ = Command::new("launchctl")
            .arg("unload")
            .arg("-w")
            .arg(&plist_path)
            .output();
    }

    let plist = build_macos_plist(exe, host, port, mode);
    std::fs::write(&plist_path, plist)
        .with_context(|| format!("write {}", plist_path.display()))?;

    let out = Command::new("launchctl")
        .arg("load")
        .arg("-w")
        .arg(&plist_path)
        .output()
        .with_context(|| "spawn launchctl load")?;
    if !out.status.success() {
        bail!(
            "launchctl load failed: {}\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    Ok(if already {
        AutostartStatus::Reinstalled { path: plist_path }
    } else {
        AutostartStatus::Installed { path: plist_path }
    })
}

#[cfg(target_os = "macos")]
fn uninstall_macos() -> Result<AutostartStatus> {
    let plist_path = macos_plist_path()?;
    if !plist_path.exists() {
        return Ok(AutostartStatus::NotInstalled);
    }
    let _ = Command::new("launchctl")
        .arg("unload")
        .arg("-w")
        .arg(&plist_path)
        .output();
    std::fs::remove_file(&plist_path)
        .with_context(|| format!("remove {}", plist_path.display()))?;
    Ok(AutostartStatus::Uninstalled { path: plist_path })
}

#[cfg(target_os = "macos")]
fn build_macos_plist(exe: &std::path::Path, host: &str, port: u16, mode: ServerMode) -> String {
    // Stdout/stderr land in /tmp so users can `tail -f` to debug autostart
    // failures without having to run `launchctl print` manually.
    let exe_str = xml_escape(&exe.display().to_string());
    let host_str = xml_escape(host);
    let mode_str = mode.as_str();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{MACOS_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe_str}</string>
    <string>server</string>
    <string>--port</string>
    <string>{port}</string>
    <string>--host</string>
    <string>{host_str}</string>
    <string>--mode</string>
    <string>{mode_str}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardOutPath</key>
  <string>/tmp/runai-server.out</string>
  <key>StandardErrorPath</key>
  <string>/tmp/runai-server.err</string>
  <key>EnvironmentVariables</key>
  <dict>
    <key>RUNAI_NO_AUTOSPAWN</key>
    <string>1</string>
  </dict>
</dict>
</plist>
"#,
    )
}

// ───────────────────────────── Linux (systemd user) ──────────────────────────

#[cfg(target_os = "linux")]
const LINUX_UNIT: &str = "runai.service";

#[cfg(target_os = "linux")]
fn linux_unit_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolve home dir")?;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join(LINUX_UNIT))
}

#[cfg(target_os = "linux")]
fn install_linux(
    exe: &std::path::Path,
    host: &str,
    port: u16,
    mode: ServerMode,
) -> Result<AutostartStatus> {
    let unit_path = linux_unit_path()?;
    let already = unit_path.exists();
    if let Some(parent) = unit_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let unit = build_linux_unit(exe, host, port, mode);
    std::fs::write(&unit_path, unit).with_context(|| format!("write {}", unit_path.display()))?;

    let reload = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .with_context(|| "spawn systemctl --user daemon-reload")?;
    if !reload.status.success() {
        bail!(
            "systemctl --user daemon-reload failed: {}\nstderr: {}",
            reload.status,
            String::from_utf8_lossy(&reload.stderr)
        );
    }
    let enable = Command::new("systemctl")
        .args(["--user", "enable", "--now", LINUX_UNIT])
        .output()
        .with_context(|| "spawn systemctl --user enable --now")?;
    if !enable.status.success() {
        bail!(
            "systemctl --user enable --now {LINUX_UNIT} failed: {}\nstderr: {}",
            enable.status,
            String::from_utf8_lossy(&enable.stderr)
        );
    }

    Ok(if already {
        AutostartStatus::Reinstalled { path: unit_path }
    } else {
        AutostartStatus::Installed { path: unit_path }
    })
}

#[cfg(target_os = "linux")]
fn uninstall_linux() -> Result<AutostartStatus> {
    let unit_path = linux_unit_path()?;
    if !unit_path.exists() {
        return Ok(AutostartStatus::NotInstalled);
    }
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", LINUX_UNIT])
        .output();
    std::fs::remove_file(&unit_path).with_context(|| format!("remove {}", unit_path.display()))?;
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    Ok(AutostartStatus::Uninstalled { path: unit_path })
}

#[cfg(target_os = "linux")]
fn build_linux_unit(exe: &std::path::Path, host: &str, port: u16, mode: ServerMode) -> String {
    let exe_str = exe.display();
    let mode_str = mode.as_str();
    format!(
        "[Unit]\n\
         Description=runai dashboard server\n\
         After=default.target\n\
         \n\
         [Service]\n\
         ExecStart={exe_str} server --port {port} --host {host} --mode {mode_str}\n\
         Environment=RUNAI_NO_AUTOSPAWN=1\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         StandardOutput=journal\n\
         StandardError=journal\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

// ─────────────────────────────── shared helpers ──────────────────────────────

#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_plist_contains_program_args_and_label() {
        use crate::core::server_mode::ServerMode;
        let exe = std::path::PathBuf::from("/usr/local/bin/runai");
        let plist = super::build_macos_plist(&exe, "127.0.0.1", 17888, ServerMode::Owner);
        assert!(plist.contains("<string>cn.crosery.runai</string>"));
        assert!(plist.contains("<string>/usr/local/bin/runai</string>"));
        assert!(plist.contains("<string>server</string>"));
        assert!(plist.contains("<string>17888</string>"));
        assert!(plist.contains("<string>127.0.0.1</string>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("RUNAI_NO_AUTOSPAWN"));
        assert!(plist.contains("<string>--mode</string>"));
        assert!(plist.contains("<string>owner</string>"));
        // The dashboard is user-facing: without ProcessType=Interactive,
        // launchd files the agent under background QoS and macOS starves it
        // after idle periods on a loaded machine (first requests crawl for
        // tens of seconds, then speed up once promoted).
        assert!(plist.contains("<key>ProcessType</key>"));
        assert!(plist.contains("<string>Interactive</string>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_plist_emits_team_mode() {
        use crate::core::server_mode::ServerMode;
        let exe = std::path::PathBuf::from("/usr/local/bin/runai");
        let plist = super::build_macos_plist(&exe, "127.0.0.1", 17888, ServerMode::Team);
        assert!(plist.contains("<string>--mode</string>"));
        assert!(plist.contains("<string>team</string>"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_plist_escapes_xml_metachars_in_host() {
        use crate::core::server_mode::ServerMode;
        let exe = std::path::PathBuf::from("/usr/local/bin/runai");
        let plist = super::build_macos_plist(&exe, "a<b&c\">d", 1, ServerMode::Owner);
        assert!(plist.contains("a&lt;b&amp;c&quot;&gt;d"));
        assert!(!plist.contains("a<b&c\">d"), "raw metachars must not leak");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_unit_contains_exec_start_and_restart() {
        use crate::core::server_mode::ServerMode;
        let exe = std::path::PathBuf::from("/usr/local/bin/runai");
        let unit = super::build_linux_unit(&exe, "127.0.0.1", 17888, ServerMode::Owner);
        assert!(unit.contains(
            "ExecStart=/usr/local/bin/runai server --port 17888 --host 127.0.0.1 --mode owner"
        ));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("Environment=RUNAI_NO_AUTOSPAWN=1"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_unit_emits_team_mode() {
        use crate::core::server_mode::ServerMode;
        let exe = std::path::PathBuf::from("/usr/local/bin/runai");
        let unit = super::build_linux_unit(&exe, "127.0.0.1", 17888, ServerMode::Team);
        assert!(unit.contains("--mode team"));
    }
}

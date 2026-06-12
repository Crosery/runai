//! Windows physical e2e for the remote PowerShell install/uninstall scripts.
//!
//! This file is `cfg(windows)` on purpose. The macOS/Linux suite covers the
//! bash installer physically and pins the PowerShell template statically; this
//! test is the gate a Windows runner uses to prove `/install.ps1` can install,
//! run a second time without prompting, and `/uninstall.ps1` can clean only the
//! runai-owned surfaces.

#![cfg(target_os = "windows")]

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

static SERVER_START_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

fn wait_for_dashboard(port: u16, timeout: Duration) -> bool {
    let url = format!("http://127.0.0.1:{port}/");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(300))
        .build()
        .expect("reqwest readiness client");
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if client
            .get(&url)
            .send()
            .map(|resp| resp.status().is_success())
            .unwrap_or(false)
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

struct ServerGuard {
    child: Child,
    _home: TempDir,
    port: u16,
}

impl ServerGuard {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn isolated_profile() -> TempDir {
    tempfile::tempdir().expect("create isolated Windows profile")
}

fn spawn_server(home: TempDir, port: u16) -> ServerGuard {
    std::fs::create_dir_all(home.path().join(".runai").join("skills"))
        .expect("pre-create .runai/skills");
    let _start_guard = SERVER_START_LOCK
        .lock()
        .expect("install_script_windows_e2e server start lock poisoned");
    let mut cmd = runai_cmd();
    cmd.arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mode")
        .arg("team")
        .env("USERPROFILE", home.path())
        .env("HOME", home.path())
        .env("APPDATA", home.path().join("AppData").join("Roaming"))
        .env("LOCALAPPDATA", home.path().join("AppData").join("Local"))
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn runai server");
    if !wait_for_dashboard(port, Duration::from_secs(15)) {
        let _ = child.kill();
        let output = child
            .wait_with_output()
            .expect("collect failed runai server output");
        panic!(
            "runai server did not answer HTTP readiness at 127.0.0.1:{port}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    ServerGuard {
        child,
        _home: home,
        port,
    }
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

fn rewrite_ps_server_url(body: &str, new_url: &str) -> String {
    body.lines()
        .map(|line| {
            if line.trim_start().starts_with("$ServerUrl = ") {
                format!(r#"$ServerUrl = "{new_url}""#)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}

fn fetch_script(server: &ServerGuard, path: &str) -> String {
    http_client()
        .get(format!("{}{}", server.base_url(), path))
        .send()
        .unwrap_or_else(|e| panic!("GET {path}: {e}"))
        .text()
        .unwrap_or_else(|e| panic!("read {path} body: {e}"))
}

fn run_powershell_script(
    script_body: &str,
    client_home: &Path,
    envs: &[(&str, &str)],
) -> std::process::Output {
    let script_dir = tempfile::tempdir().expect("script tempdir");
    let script_path = script_dir.path().join("runai-script.ps1");
    std::fs::write(&script_path, script_body).expect("write ps1 script");

    let mut cmd = Command::new("powershell.exe");
    cmd.arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&script_path)
        .env("USERPROFILE", client_home)
        .env("HOME", client_home)
        .env("APPDATA", client_home.join("AppData").join("Roaming"))
        .env("LOCALAPPDATA", client_home.join("AppData").join("Local"))
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .env_remove("RUNAI_USERNAME")
        .env_remove("RUNAI_PASSWORD")
        .env_remove("RUNAI_PHASE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        cmd.env(key, value);
    }
    cmd.output().expect("run powershell script")
}

fn settings_hook_commands(path: &Path) -> Vec<String> {
    let raw = std::fs::read_to_string(path).expect("read settings.json");
    let json: serde_json::Value = serde_json::from_str(&raw).expect("settings.json is valid JSON");
    json.pointer("/hooks/UserPromptSubmit")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .flat_map(|group| {
            group
                .get("hooks")
                .and_then(|hooks| hooks.as_array())
                .into_iter()
                .flatten()
        })
        .filter_map(|hook| {
            hook.get("command")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect()
}

#[test]
fn powershell_install_second_run_and_uninstall_round_trip() {
    let server_home = isolated_profile();
    let port = free_port();
    let server = spawn_server(server_home, port);
    let install_body =
        rewrite_ps_server_url(&fetch_script(&server, "/install.ps1"), &server.base_url());
    let uninstall_body = fetch_script(&server, "/uninstall.ps1");

    let client_home = isolated_profile();
    let settings_path = client_home.path().join(".claude").join("settings.json");
    std::fs::create_dir_all(settings_path.parent().unwrap()).expect("create .claude dir");
    std::fs::write(
        &settings_path,
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"Write-Host unrelated"}]}]},"theme":"dark"}"#,
    )
    .expect("seed settings.json");

    let claude_json_path = client_home.path().join(".claude.json");
    std::fs::write(
        &claude_json_path,
        r#"{"mcpServers":{"other-mcp":{"command":"other","args":["x"]}},"someTopLevel":42}"#,
    )
    .expect("seed .claude.json");

    let first = run_powershell_script(
        &install_body,
        client_home.path(),
        &[
            ("RUNAI_USERNAME", "windows-e2e"),
            ("RUNAI_PASSWORD", "windows-password"),
        ],
    );
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    let first_stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        first.status.success(),
        "install.ps1 failed:\nstdout=\n{first_stdout}\nstderr=\n{first_stderr}"
    );
    assert!(
        !first_stdout.contains("e[38;5") && !first_stderr.contains("e[38;5"),
        "PowerShell installer must not print raw ANSI escape text:\nstdout=\n{first_stdout}\nstderr=\n{first_stderr}"
    );
    assert!(client_home.path().join(".runai-identity").is_file());
    assert!(client_home.path().join(".runai-server.json").is_file());
    assert!(client_home.path().join(".runai-hook.ps1").is_file());
    assert!(
        client_home
            .path()
            .join(".local")
            .join("bin")
            .join("runai-client.ps1")
            .is_file()
    );
    assert!(
        client_home
            .path()
            .join(".local")
            .join("bin")
            .join("runai-client.cmd")
            .is_file()
    );

    let hook_commands = settings_hook_commands(&settings_path);
    assert!(
        hook_commands
            .iter()
            .any(|cmd| cmd == "Write-Host unrelated"),
        "install must preserve unrelated hook; commands={hook_commands:?}"
    );
    assert!(
        hook_commands
            .iter()
            .any(|cmd| cmd.contains(".runai-hook.ps1")),
        "install must add runai hook; commands={hook_commands:?}"
    );

    let identity: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(client_home.path().join(".runai-identity"))
            .expect("read identity"),
    )
    .expect("identity json");
    let api_key = identity
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(api_key.starts_with("rnai_"), "identity={identity}");

    let claude_json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&claude_json_path).expect("read .claude.json after install"),
    )
    .expect(".claude.json after install");
    assert!(claude_json["mcpServers"]["other-mcp"].is_object());
    assert_eq!(claude_json["someTopLevel"], 42);
    assert_eq!(claude_json["mcpServers"]["runai-client"]["type"], "http");
    assert_eq!(
        claude_json["mcpServers"]["runai-client"]["url"],
        serde_json::Value::String(format!("{}/mcp", server.base_url()))
    );
    assert_eq!(
        claude_json["mcpServers"]["runai-client"]["headers"]["Authorization"],
        serde_json::Value::String(format!("Bearer {api_key}"))
    );

    let second = run_powershell_script(&install_body, client_home.path(), &[]);
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    let second_stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        second.status.success(),
        "second install.ps1 failed:\nstdout=\n{second_stdout}\nstderr=\n{second_stderr}"
    );
    assert!(
        second_stdout.contains("server accepted stored api_key"),
        "second install must reuse verified identity; stdout=\n{second_stdout}"
    );
    let runai_hook_count = settings_hook_commands(&settings_path)
        .iter()
        .filter(|cmd| cmd.contains(".runai-hook.ps1"))
        .count();
    assert_eq!(runai_hook_count, 1, "second install duplicated hook");

    let uninstall = run_powershell_script(&uninstall_body, client_home.path(), &[]);
    let uninstall_stdout = String::from_utf8_lossy(&uninstall.stdout);
    let uninstall_stderr = String::from_utf8_lossy(&uninstall.stderr);
    assert!(
        uninstall.status.success(),
        "uninstall.ps1 failed:\nstdout=\n{uninstall_stdout}\nstderr=\n{uninstall_stderr}"
    );
    assert!(client_home.path().join(".runai-identity").is_file());
    assert!(!client_home.path().join(".runai-server.json").exists());
    assert!(!client_home.path().join(".runai-hook.ps1").exists());
    assert!(
        !client_home
            .path()
            .join(".local")
            .join("bin")
            .join("runai-client.ps1")
            .exists()
    );
    assert!(
        !client_home
            .path()
            .join(".local")
            .join("bin")
            .join("runai-client.cmd")
            .exists()
    );
    assert_eq!(
        settings_hook_commands(&settings_path),
        vec!["Write-Host unrelated".to_string()],
        "uninstall must remove only runai hook"
    );
    let claude_after: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&claude_json_path).expect("read .claude.json after uninstall"),
    )
    .expect(".claude.json after uninstall");
    assert!(claude_after["mcpServers"]["runai-client"].is_null());
    assert!(claude_after["mcpServers"]["other-mcp"].is_object());
    assert_eq!(claude_after["someTopLevel"], 42);
}

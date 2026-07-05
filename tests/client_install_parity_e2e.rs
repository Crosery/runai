//! Phase 4 e2e: install-body parity for the runai-client activation/
//! feedback protocol (PLANNING §1.3). Both `/install` (bash) and
//! `/install.ps1` (PowerShell) rendered bodies must:
//!   - name the new subcommands (activate / feedback / sync / flush / file)
//!   - reference the client-cache dir (NEVER ~/.runai/skills/ as cache)
//!   - NOT use bare `curl .../skills/get` / `Invoke-RestMethod .../skills/get`
//!     as the activation instruction (the hook output protocol now drives
//!     activation through runai-client)
//!
//! Unix runners cannot execute PowerShell, so the PS1 assertions are
//! static-body string checks. The Windows real-run suite
//! (`client_install_parity_windows_e2e`, gated `#[cfg(windows)]`) drives
//! the actual companion — that file is the live-verification counterpart.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary")
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn wait_for_port(port: u16, t: Duration) -> bool {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let d = Instant::now() + t;
    while Instant::now() < d {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

struct Server {
    child: Child,
    _home: TempDir,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_server() -> Server {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    let port = free_port();
    let data_dir = home.path().join(".runai");
    let child = runai_cmd()
        .args([
            "server",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--mode",
            "team",
        ])
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", &data_dir)
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(wait_for_port(port, Duration::from_secs(8)));
    Server {
        child,
        _home: home,
        port,
    }
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

fn install_body(server: &Server, path: &str) -> String {
    http()
        .get(format!("http://127.0.0.1:{}{}", server.port, path))
        .send()
        .unwrap()
        .text()
        .unwrap()
}

#[test]
fn bash_install_body_contains_new_subcommands() {
    let s = spawn_server();
    let body = install_body(&s, "/install");
    for needle in &["activate", "feedback", "sync", "flush", "file"] {
        assert!(
            body.contains(needle),
            "bash /install body missing subcommand `{needle}`"
        );
    }
}

#[test]
fn bash_install_body_contains_client_cache_dir() {
    let s = spawn_server();
    let body = install_body(&s, "/install");
    assert!(
        body.contains("client-cache"),
        "bash /install body must reference client-cache dir"
    );
}

#[test]
fn bash_install_body_hook_calls_runai_client_activate() {
    let s = spawn_server();
    let body = install_body(&s, "/install");
    assert!(
        body.contains("runai-client activate"),
        "bash install body must reference runai-client activate"
    );
}

#[test]
fn ps1_install_body_contains_new_subcommands() {
    let s = spawn_server();
    let body = install_body(&s, "/install.ps1");
    for needle in &["activate", "feedback", "sync", "flush", "file"] {
        assert!(
            body.contains(needle),
            "ps1 /install.ps1 body missing subcommand `{needle}`"
        );
    }
}

#[test]
fn ps1_install_body_contains_client_cache_dir() {
    let s = spawn_server();
    let body = install_body(&s, "/install.ps1");
    assert!(
        body.contains("client-cache"),
        "ps1 /install.ps1 body must reference client-cache dir"
    );
}

#[test]
fn ps1_install_body_writes_runai_client_companion() {
    let s = spawn_server();
    let body = install_body(&s, "/install.ps1");
    assert!(
        body.contains("runai-client.ps1"),
        "ps1 install body must write a runai-client.ps1 companion"
    );
    assert!(
        body.contains("Invoke-Activate"),
        "ps1 install body must define Invoke-Activate"
    );
    assert!(
        body.contains("Invoke-File"),
        "ps1 install body must define Invoke-File"
    );
}

#[test]
fn both_bodies_have_symmetric_subcommand_set() {
    let s = spawn_server();
    let bash = install_body(&s, "/install");
    let ps1 = install_body(&s, "/install.ps1");
    for sub in &["activate", "feedback", "sync", "flush", "file"] {
        assert!(
            bash.contains(sub) && ps1.contains(sub),
            "subcommand `{sub}` must appear in BOTH install bodies"
        );
    }
    // Both must reference the client-cache + outbox primitives.
    assert!(bash.contains("outbox"));
    assert!(ps1.contains("outbox"));
}

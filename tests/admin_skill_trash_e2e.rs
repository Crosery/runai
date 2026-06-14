//! PLANNING §1.6 Model B C7b — admin batch trash 端点.
//!
//! POST /api/admin/skills/trash {"names": ["foo", "bar", ...]} —
//! admin-only,把公共池里的 skill 批量移到 trash(走
//! manager::trash_resource,不是 hard delete,可 restore)。
//!
//! Contract:
//!   - 401 with no credential
//!   - 403 for non-admin
//!   - 200 + {trashed: N, failed: ["name: reason", ...]} for admin
//!   - Unknown skill name lands in `failed` rather than aborting the batch

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};
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
fn spawn_team_server() -> ServerGuard {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    let port = free_port();
    let child = runai_cmd()
        .arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mode")
        .arg("team")
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let g = ServerGuard {
        child,
        _home: home,
        port,
    };
    assert!(wait_for_port(port, Duration::from_secs(8)));
    g
}
fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}
fn register(s: &ServerGuard, u: &str, p: &str) -> String {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let b: Value = r.json().unwrap();
    b["api_key"].as_str().unwrap().to_string()
}

#[test]
fn admin_skills_trash_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/admin/skills/trash", s.base_url()))
        .json(&json!({"names": []}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn admin_skills_trash_non_admin_forbidden() {
    let s = spawn_team_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    let r = http()
        .post(format!("{}/api/admin/skills/trash", s.base_url()))
        .bearer_auth(&bob)
        .json(&json!({"names": ["anything"]}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

#[test]
fn admin_skills_trash_empty_batch_returns_zero() {
    let s = spawn_team_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/admin/skills/trash", s.base_url()))
        .bearer_auth(&admin)
        .json(&json!({"names": []}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["trashed"].as_u64().unwrap(), 0);
    assert!(b["failed"].is_array());
    assert_eq!(b["failed"].as_array().unwrap().len(), 0);
}

#[test]
fn admin_skills_trash_unknown_skills_go_to_failed() {
    let s = spawn_team_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/admin/skills/trash", s.base_url()))
        .bearer_auth(&admin)
        .json(&json!({"names": ["definitely-no-such-skill-xyz", "also-not-real"]}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    // Each unknown skill must produce a `failed` row instead of aborting
    // the whole batch — the dashboard needs to render per-row outcomes.
    assert_eq!(b["trashed"].as_u64().unwrap(), 0);
    let failed = b["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 2, "expected 2 failed entries, got {failed:?}");
    // Each row identifies the skill name that failed.
    let joined = failed
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(joined.contains("definitely-no-such-skill-xyz"));
    assert!(joined.contains("also-not-real"));
}

#[test]
fn admin_skills_trash_rejects_missing_names_field() {
    let s = spawn_team_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!("{}/api/admin/skills/trash", s.base_url()))
        .bearer_auth(&admin)
        .json(&json!({}))
        .send()
        .unwrap();
    // Missing `names` field is a malformed request — clap-style 422 or
    // 400 are both acceptable per axum extractor convention.
    assert!(
        matches!(r.status().as_u16(), 400 | 422),
        "expected 400/422 for missing names field, got {}",
        r.status()
    );
}

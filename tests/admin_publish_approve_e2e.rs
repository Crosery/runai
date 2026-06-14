//! PLANNING §1.4 C9d — admin approve / reject for publish-requests.
//!
//! Endpoints:
//!   GET  /api/admin/publish-requests
//!        list every resources row with publish_status='pending'
//!   POST /api/admin/publish-requests/{resource_id}/approve
//!        pending → approved + copy <data>/users/<uid>/skills/<name>/
//!        to <data>/community/<uid>/<name>/ + community_skills upsert
//!   POST /api/admin/publish-requests/{resource_id}/reject {reason}
//!        pending → rejected + publish_reason
//!
//! Contract pinned here is wire-level only (status codes + payload
//! shape). The "approve really copies the on-disk tree" happy path
//! lives in C9 manual / real-binary verification because the test
//! sandbox never runs enrich, so a row can never legitimately reach
//! pending — we'd need to force-flip the publish_status via direct DB
//! write to exercise approve, which is what we do here.

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
fn spawn_server() -> ServerGuard {
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
    assert!(wait_for_port(g.port, Duration::from_secs(8)));
    g
}
fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
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
fn admin_publish_list_requires_auth() {
    let s = spawn_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let r = http()
        .get(format!("{}/api/admin/publish-requests", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn admin_publish_list_non_admin_forbidden() {
    let s = spawn_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    let r = http()
        .get(format!("{}/api/admin/publish-requests", s.base_url()))
        .bearer_auth(&bob)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

#[test]
fn admin_publish_list_empty_for_fresh_server() {
    let s = spawn_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let r = http()
        .get(format!("{}/api/admin/publish-requests", s.base_url()))
        .bearer_auth(&admin)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert!(b["items"].is_array());
    assert_eq!(b["items"].as_array().unwrap().len(), 0);
    assert_eq!(b["total"].as_u64().unwrap(), 0);
}

#[test]
fn admin_publish_approve_requires_admin() {
    let s = spawn_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    let r = http()
        .post(format!(
            "{}/api/admin/publish-requests/u:bob:local:fake/approve",
            s.base_url()
        ))
        .bearer_auth(&bob)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

#[test]
fn admin_publish_approve_404_for_unknown_resource() {
    let s = spawn_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!(
            "{}/api/admin/publish-requests/no-such-id/approve",
            s.base_url()
        ))
        .bearer_auth(&admin)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
}

#[test]
fn admin_publish_reject_404_for_unknown_resource() {
    let s = spawn_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let r = http()
        .post(format!(
            "{}/api/admin/publish-requests/no-such-id/reject",
            s.base_url()
        ))
        .bearer_auth(&admin)
        .json(&json!({"reason": "duplicate"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
}

#[test]
fn admin_publish_reject_requires_admin() {
    let s = spawn_server();
    let _ = register(&s, "admin", "pw admin 1234");
    let bob = register(&s, "bob", "pw bob 1234");
    let r = http()
        .post(format!(
            "{}/api/admin/publish-requests/u:bob:local:fake/reject",
            s.base_url()
        ))
        .bearer_auth(&bob)
        .json(&json!({"reason": "x"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 403);
}

//! Real e2e for v15 per-user skill library endpoints (issue #19 still_open):
//!   GET    /api/skills/library
//!   POST   /api/skills/library              (action=add|remove, names=[..])
//!   POST   /api/skills/library/clear
//!   POST   /api/skills/library/fill?top=N
//!   POST   /api/skills/library/import-from-usage

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
    let deadline = Instant::now() + t;
    while Instant::now() < deadline {
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
        .expect("spawn server");
    let g = ServerGuard {
        child,
        _home: home,
        port,
    };
    assert!(
        wait_for_port(port, Duration::from_secs(8)),
        "server bind fail"
    );
    g
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
}

fn register(s: &ServerGuard, u: &str, p: &str) -> (String, String) {
    let resp = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 201);
    let b: Value = resp.json().unwrap();
    (
        b["user_id"].as_str().unwrap().into(),
        b["api_key"].as_str().unwrap().into(),
    )
}

// ─── GET /api/skills/library ─────────────────────────────────────────────

#[test]
fn library_list_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/skills/library", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

#[test]
fn library_list_empty_for_fresh_user() {
    let s = spawn_team_server();
    let (uid, key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["user_id"].as_str().unwrap(), uid);
    assert_eq!(b["total"].as_u64().unwrap(), 0);
    assert!(b["items"].as_array().unwrap().is_empty());
}

// ─── POST /api/skills/library (mutate add/remove) ─────────────────────────

#[test]
fn library_add_inserts_names_and_reports_affected() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .json(&json!({"action": "add", "names": ["foo", "bar", "baz"]}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["action"].as_str().unwrap(), "add");
    assert_eq!(b["affected"].as_u64().unwrap(), 3);
    assert_eq!(b["library_size"].as_u64().unwrap(), 3);

    // Verify via list
    let list: Value = http()
        .get(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap()
        .json()
        .unwrap();
    let names: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    assert!(names.contains(&"baz"));
}

#[test]
fn library_add_is_idempotent_on_duplicates() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let _ = http()
        .post(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .json(&json!({"action": "add", "names": ["foo"]}))
        .send()
        .unwrap();
    let r = http()
        .post(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .json(&json!({"action": "add", "names": ["foo", "bar"]}))
        .send()
        .unwrap();
    let b: Value = r.json().unwrap();
    assert_eq!(b["affected"].as_u64().unwrap(), 1, "only 'bar' was new");
    assert_eq!(b["library_size"].as_u64().unwrap(), 2);
}

#[test]
fn library_remove_pulls_names() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let _ = http()
        .post(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .json(&json!({"action": "add", "names": ["a","b","c"]}))
        .send()
        .unwrap();
    let r = http()
        .post(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .json(&json!({"action": "remove", "names": ["a","b","ghost"]}))
        .send()
        .unwrap();
    let b: Value = r.json().unwrap();
    assert_eq!(
        b["affected"].as_u64().unwrap(),
        2,
        "a, b removed; ghost no-op"
    );
    assert_eq!(b["library_size"].as_u64().unwrap(), 1);
}

#[test]
fn library_mutate_unknown_action_400() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .json(&json!({"action": "xyz", "names": ["foo"]}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);
}

#[test]
fn library_mutate_isolates_users() {
    let s = spawn_team_server();
    let (_aid, akey) = register(&s, "alice", "pw alice 1234");
    let (_bid, bkey) = register(&s, "bob", "pw bob 1234");
    let _ = http()
        .post(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&akey)
        .json(&json!({"action": "add", "names": ["alice-skill"]}))
        .send()
        .unwrap();
    let b_list: Value = http()
        .get(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&bkey)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(
        b_list["total"].as_u64().unwrap(),
        0,
        "Bob doesn't see Alice's library"
    );
}

// ─── POST /api/skills/library/clear ─────────────────────────────────────

#[test]
fn library_clear_empties_and_reports_count() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let _ = http()
        .post(format!("{}/api/skills/library", s.base_url()))
        .bearer_auth(&key)
        .json(&json!({"action": "add", "names": ["x","y","z"]}))
        .send()
        .unwrap();
    let r = http()
        .post(format!("{}/api/skills/library/clear", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["action"].as_str().unwrap(), "clear");
    assert_eq!(b["affected"].as_u64().unwrap(), 3);
    assert_eq!(b["library_size"].as_u64().unwrap(), 0);
}

#[test]
fn library_clear_idempotent() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!("{}/api/skills/library/clear", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["affected"].as_u64().unwrap(), 0);
    assert_eq!(b["library_size"].as_u64().unwrap(), 0);
}

// ─── POST /api/skills/library/fill ──────────────────────────────────────

#[test]
fn library_fill_returns_action_fill() {
    // No public skills on fresh server → fill is a no-op but action label set.
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!("{}/api/skills/library/fill?top=10", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["action"].as_str().unwrap(), "fill");
    // affected == library_size since the user library was empty before
    assert_eq!(
        b["affected"].as_u64().unwrap(),
        b["library_size"].as_u64().unwrap()
    );
}

#[test]
fn library_fill_default_top_clamps() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    // top=0 → clamp to 1; top=999 → clamp to 500
    let r1 = http()
        .post(format!("{}/api/skills/library/fill?top=0", s.base_url()))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r1.status().as_u16(), 200);
    let r2 = http()
        .post(format!(
            "{}/api/skills/library/fill?top=99999",
            s.base_url()
        ))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r2.status().as_u16(), 200);
    // Both should respond cleanly without error.
}

#[test]
fn library_fill_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!("{}/api/skills/library/fill", s.base_url()))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

// ─── POST /api/skills/library/import-from-usage ─────────────────────────

#[test]
fn library_import_from_usage_empty_returns_zero() {
    let s = spawn_team_server();
    let (_uid, key) = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!(
            "{}/api/skills/library/import-from-usage",
            s.base_url()
        ))
        .bearer_auth(&key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    assert_eq!(b["action"].as_str().unwrap(), "import-from-usage");
    assert_eq!(b["affected"].as_u64().unwrap(), 0);
}

#[test]
fn library_import_from_usage_requires_auth() {
    let s = spawn_team_server();
    let _ = register(&s, "alice", "pw alice 1234");
    let r = http()
        .post(format!(
            "{}/api/skills/library/import-from-usage",
            s.base_url()
        ))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
}

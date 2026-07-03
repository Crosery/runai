//! Phase 1 e2e: idempotency invariants for `POST /skills/use/{name}` —
//! the concurrent-same-event_id and canonical-payload-hash contracts.
//!
//! The single-event-id lifecycle is covered by `skills_use_endpoint_e2e.rs`;
//! this file locks the parts that only break under concurrency or subtle
//! canonicalization drift.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
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

struct Server {
    child: Child,
    home: TempDir,
    port: u16,
}

impl Server {
    fn spawn() -> Self {
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
            .expect("spawn runai server");
        let s = Self { child, home, port };
        assert!(wait_for_port(port, Duration::from_secs(8)));
        s
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn db_path(&self) -> std::path::PathBuf {
        self.home.path().join(".runai/runai.db")
    }

    fn plant(&self, name: &str) {
        let dir = self.home.path().join(".runai/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: x\n---\n\n# {name}\n"),
        )
        .unwrap();
        let out = runai_cmd()
            .arg("scan")
            .env("HOME", self.home.path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.home.path().join(".runai"))
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .unwrap();
        assert!(out.status.success(), "scan failed");
    }

    fn usage_count(&self, name: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.db_path()).unwrap();
        conn.query_row(
            "SELECT usage_count FROM resources WHERE name=?1",
            rusqlite::params![name],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }

    fn usage_event_count(&self, event_id: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.db_path()).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM usage_events WHERE event_id=?1",
            rusqlite::params![event_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn idempotency_concurrent_same_event_id_only_one_increment() {
    let s = Server::spawn();
    s.plant("foo");
    let base = s.base();
    let url = format!("{base}/skills/use/foo");
    // 5 concurrent threads, same event_id + payload. Exactly one increment,
    // exactly one usage_events row.
    let url = Arc::new(url);
    let handles: Vec<_> = (0..5)
        .map(|_| {
            let url = url.clone();
            std::thread::spawn(move || {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap();
                let resp = client
                    .post(url.as_str())
                    .header("X-Runai-Event-Id", "e-concurrent")
                    .json(&json!({}))
                    .send()
                    .unwrap();
                resp.status().as_u16()
            })
        })
        .collect();
    let statuses: Vec<u16> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    for st in &statuses {
        assert_eq!(
            *st, 200,
            "all concurrent calls should be 200 (first + no-ops)"
        );
    }
    assert_eq!(s.usage_count("foo"), 1, "exactly one increment");
    assert_eq!(
        s.usage_event_count("e-concurrent"),
        1,
        "exactly one event row"
    );
}

#[test]
fn idempotency_conflict_409_body_has_no_internal_paths() {
    let s = Server::spawn();
    s.plant("foo");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let _r1 = client
        .post(format!("{}/skills/use/foo", s.base()))
        .header("X-Runai-Event-Id", "e-c")
        .json(&json!({}))
        .send()
        .unwrap();
    let r2 = client
        .post(format!("{}/skills/use/foo", s.base()))
        .header("X-Runai-Event-Id", "e-c")
        .json(&json!({"include": ["other.md"]}))
        .send()
        .unwrap();
    assert_eq!(r2.status().as_u16(), 409);
    let body = r2.text().unwrap();
    for needle in ["/Users/", "/home/", ".runai/", "target/", "runai.db"] {
        assert!(!body.contains(needle), "409 leaks {needle}: {body}");
    }
}

#[test]
fn idempotency_payload_hash_ignores_field_order() {
    let s = Server::spawn();
    s.plant("foo");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let r1 = client
        .post(format!("{}/skills/use/foo", s.base()))
        .header("X-Runai-Event-Id", "e-ord")
        // body1: session_id first, include second
        .json(&json!({"session_id": "s", "include": ["a"]}))
        .send()
        .unwrap();
    assert_eq!(r1.status().as_u16(), 200);
    let after_first = s.usage_count("foo");
    let r2 = client
        .post(format!("{}/skills/use/foo", s.base()))
        .header("X-Runai-Event-Id", "e-ord")
        // body2: include first, session_id second — same logical payload
        .json(&json!({"include": ["a"], "session_id": "s"}))
        .send()
        .unwrap();
    assert_eq!(
        r2.status().as_u16(),
        200,
        "field-order change must be a no-op, not conflict"
    );
    let v: Value = r2.json().unwrap_or(Value::Null);
    let _ = v; // body shape validated in endpoint suite
    assert_eq!(
        s.usage_count("foo"),
        after_first,
        "no second bump on reordered payload"
    );
    assert_eq!(s.usage_event_count("e-ord"), 1);
}

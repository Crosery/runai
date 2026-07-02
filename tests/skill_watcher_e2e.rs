//! Real-time enrichment watcher: writing a SKILL.md under the data dir's skill
//! pool auto-triggers enrichment, flipping the skill to 富集中 on `/api/skills`.
//!
//! Proves the server-side file watcher (`core::skill_watcher`, wired in
//! `app::serve_with`) reacts to an in-place SKILL.md change WITHOUT waiting for
//! a SessionStart enrich pass. No LLM provider is configured, so the enrich
//! child is a no-op — but the in-process `mark_enriching` (fired by the
//! watcher's `spawn_enrich`) is observable as `enrich_status == "enriching"`.

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
    home: TempDir,
    port: u16,
}
impl ServerGuard {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn db_path(&self) -> std::path::PathBuf {
        self.home.path().join(".runai/runai.db")
    }
    fn skills_dir(&self) -> std::path::PathBuf {
        self.home.path().join(".runai/skills")
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
    let g = ServerGuard { child, home, port };
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
    b["api_key"].as_str().unwrap().into()
}
fn status_of(s: &ServerGuard, key: &str, name: &str) -> Option<String> {
    let r = http()
        .get(format!("{}/api/skills", s.base_url()))
        .bearer_auth(key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    b["skills"]
        .as_array()?
        .iter()
        .find(|x| x["name"] == name)
        .and_then(|x| x["enrich_status"].as_str())
        .map(String::from)
}

#[test]
fn writing_skill_md_marks_it_enriching() {
    use runai::core::db::Database;
    use runai::core::resource::{Resource, ResourceKind, Source};

    let s = spawn_server();
    let admin = register(&s, "admin", "pw admin 1234");

    // A public skill row whose directory lives under the watched pool, with no
    // summary yet → baseline "unenriched".
    let dir = s.skills_dir().join("watched");
    {
        let db = Database::open(&s.db_path()).unwrap();
        let r = Resource {
            id: "local:watched".into(),
            name: "watched".into(),
            kind: ResourceKind::Skill,
            description: "d".into(),
            directory: dir.clone(),
            source: Source::Local {
                path: std::path::PathBuf::from("/tmp"),
            },
            installed_at: 0,
            enabled: std::collections::HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: None,
            publish_status: "draft".into(),
        };
        db.insert_resource(&r).unwrap();
    }
    assert_eq!(
        status_of(&s, &admin, "watched").as_deref(),
        Some("unenriched"),
        "baseline: not touched, no summary → 未富集"
    );

    // Now write its SKILL.md into the watched pool. The server's recursive
    // watcher must fire → spawn_enrich → mark_enriching("watched").
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: watched\ndescription: x\n---\nbody\n",
    )
    .unwrap();

    // Poll for the watcher (300ms debounce + FSEvents latency).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw = None;
    while Instant::now() < deadline {
        saw = status_of(&s, &admin, "watched");
        if saw.as_deref() == Some("enriching") {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(
        saw.as_deref(),
        Some("enriching"),
        "editing SKILL.md under the watched pool must auto-mark the skill 富集中"
    );
}

//! 3-state enrichment status on `/api/skills` (已富集 / 富集中 / 未富集).
//!
//! Enrichment is async (a detached `recommend enrich` child). The server tracks
//! "富集中" in an in-memory registry (`enrich_state`) — a name is marked when an
//! enrich is triggered (upload / install / file-watch) and cleared once the
//! summary row appears. `/api/skills` derives `enrich_status` per skill:
//!   - summary non-empty            → "enriched"
//!   - in the in-flight registry    → "enriching"
//!   - otherwise                    → "unenriched"
//!
//! These transitions are observable WITHOUT an LLM provider: an upload marks
//! 富集中 (the child enrich is gated/no-op without a provider, so summary stays
//! empty), and we drive the enriched/unenriched states via the runai lib DB.

#![cfg(not(target_os = "windows"))]

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use flate2::write::GzEncoder;
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
struct Account {
    user_id: String,
    api_key: String,
}
fn register(s: &ServerGuard, u: &str, p: &str) -> Account {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let b: Value = r.json().unwrap();
    Account {
        user_id: b["user_id"].as_str().unwrap().into(),
        api_key: b["api_key"].as_str().unwrap().into(),
    }
}
fn make_bundle(name: &str, skill_md: &str) -> Vec<u8> {
    let buf: Vec<u8> = Vec::new();
    let gz = GzEncoder::new(buf, flate2::Compression::default());
    let mut tar = tar::Builder::new(gz);
    let mut dh = tar::Header::new_gnu();
    dh.set_entry_type(tar::EntryType::Directory);
    dh.set_mode(0o755);
    dh.set_size(0);
    dh.set_cksum();
    tar.append_data(&mut dh, format!("{name}/"), std::io::empty())
        .unwrap();
    let mut fh = tar::Header::new_gnu();
    fh.set_size(skill_md.len() as u64);
    fh.set_mode(0o644);
    fh.set_cksum();
    tar.append_data(&mut fh, format!("{name}/SKILL.md"), skill_md.as_bytes())
        .unwrap();
    let gz = tar.into_inner().unwrap();
    let mut out = gz.finish().unwrap();
    out.flush().unwrap();
    out
}
fn multipart_body(name: &str, bundle: &[u8], boundary: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    b.extend_from_slice(b"Content-Disposition: form-data; name=\"name\"\r\n\r\n");
    b.extend_from_slice(name.as_bytes());
    b.extend_from_slice(b"\r\n");
    b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    b.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"bundle\"; filename=\"{name}.tar.gz\"\r\n")
            .as_bytes(),
    );
    b.extend_from_slice(b"Content-Type: application/gzip\r\n\r\n");
    b.extend_from_slice(bundle);
    b.extend_from_slice(b"\r\n");
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    b
}
fn upload_private(s: &ServerGuard, actor: &Account, name: &str) {
    let bundle = make_bundle(name, "---\nname: x\ndescription: x\n---\nbody\n");
    let boundary = format!("----runai-{}-{name}", actor.user_id);
    let body = multipart_body(name, &bundle, &boundary);
    let r = http()
        .post(format!("{}/api/users/me/skills/upload", s.base_url()))
        .bearer_auth(&actor.api_key)
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "upload failed");
}
fn skills(s: &ServerGuard, key: &str) -> Vec<Value> {
    let r = http()
        .get(format!("{}/api/skills", s.base_url()))
        .bearer_auth(key)
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let b: Value = r.json().unwrap();
    b["skills"].as_array().cloned().unwrap_or_default()
}
fn status_of<'a>(rows: &'a [Value], name: &str) -> Option<&'a str> {
    rows.iter()
        .find(|s| s["name"] == name)
        .and_then(|s| s["enrich_status"].as_str())
}

#[test]
fn upload_marks_skill_enriching() {
    let s = spawn_server();
    let admin = register(&s, "admin", "pw admin 1234");
    let alice = register(&s, "alice", "pw alice 1234");
    upload_private(&s, &alice, "fresh");
    let rows = skills(&s, &admin.api_key);
    assert_eq!(
        status_of(&rows, "fresh"),
        Some("enriching"),
        "a freshly uploaded skill (summary empty, in-flight) must report 富集中"
    );
}

#[test]
fn summary_present_reports_enriched_absent_reports_unenriched() {
    use runai::core::db::Database;
    use runai::core::resource::{Resource, ResourceKind, Source};
    let s = spawn_server();
    let admin = register(&s, "admin", "pw admin 1234");

    // Insert a public skill directly via the lib DB — never marked enriching,
    // no summary → must be "unenriched".
    {
        let db = Database::open(&s.db_path()).unwrap();
        let dir = s.home.path().join(".runai/skills/lonely");
        let r = Resource {
            id: "local:lonely".into(),
            name: "lonely".into(),
            kind: ResourceKind::Skill,
            description: "d".into(),
            directory: dir,
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
    let rows = skills(&s, &admin.api_key);
    assert_eq!(
        status_of(&rows, "lonely"),
        Some("unenriched"),
        "a skill with no summary and not in-flight must report 未富集"
    );

    // Now give it a summary → must flip to "enriched".
    {
        let db = Database::open(&s.db_path()).unwrap();
        db.set_skill_ai_summary_scored("lonely", "task: do a thing\ntriggers: x", 7)
            .unwrap();
    }
    let rows = skills(&s, &admin.api_key);
    assert_eq!(
        status_of(&rows, "lonely"),
        Some("enriched"),
        "a skill with a non-empty summary must report 已富集"
    );
}

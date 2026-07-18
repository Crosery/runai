//! PLANNING §1.1 owner-mode dashboard cut: implicit-admin auth bypass +
//! `mode-owner` body class + `MeResp.mode` field.
//!
//! Pins the user-facing contract for the owner-mode dashboard:
//!   - `GET /api/me` succeeds with NO credential, returns synthetic
//!     `owner` user with `is_admin: true` and `mode: "owner"`.
//!   - `GET /api/skills` succeeds with NO credential (owner mode bypasses
//!     `private_data_locked` — local user is implicit admin).
//!   - `GET /` body carries the `mode-owner` class so CSS can hide the
//!     team-only chrome (account pill / login modal / userlib tab /
//!     scope selectors / community tab).
//!
//! Team-mode regression checks ensure the owner-mode short-circuits do
//! NOT leak into team mode:
//!   - `GET /api/me` without credential still returns 401.
//!   - `GET /` body does NOT carry the `mode-owner` class.
//!   - `GET /api/me` with bearer returns `mode: "team"`.
//!
//! All tests spawn the real binary inside an isolated HOME — the live
//! `~/.runai/runai.db` is never touched (AGENTS.md safety contract).

#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use runai::core::db::SkillAiIndex;
use runai::core::manager::SkillManager;
use runai::core::recommend::{Provider, RecommendConfig};
use serde_json::{Value, json};
use tempfile::TempDir;

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = Instant::now() + timeout;
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
    data_dir: std::path::PathBuf,
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

fn spawn_server(mode: &str) -> ServerGuard {
    let home = tempfile::tempdir().expect("create tmp HOME");
    let data_dir = home.path().join("runai-data");
    std::fs::create_dir_all(data_dir.join("skills")).expect("pre-create isolated skills");
    let port = free_port();
    let child = runai_cmd()
        .arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .arg("--mode")
        .arg(mode)
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", &data_dir)
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn runai server");
    let g = ServerGuard {
        child,
        _home: home,
        port,
        data_dir,
    };
    assert!(
        wait_for_port(g.port, Duration::from_secs(8)),
        "runai server (mode={mode}) did not bind 127.0.0.1:{} in 8s",
        g.port
    );
    g
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

struct MockLlm {
    base_url: String,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockLlm {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let calls_t = calls.clone();
        let stop_t = stop.clone();
        let handle = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                        let body = read_http_body(&mut stream);
                        let idx = calls_t.fetch_add(1, Ordering::SeqCst);
                        let content = if idx == 0 {
                            "intent: alpha task\ninclude_terms: alpha"
                        } else {
                            r#"{"mode":"exclusive","selected":["C01"]}"#
                        };
                        assert!(!body.is_empty());
                        let json = serde_json::json!({
                            "choices": [{"message": {"content": content}}],
                            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            json.len(),
                            json
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base_url,
            calls,
            stop,
            handle: Some(handle),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_http_body(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 8192];
    let header_end = loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return String::new(),
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
            }
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_lowercase();
    let len = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while bytes.len().saturating_sub(body_start) < len {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&bytes[body_start..]).to_string()
}

fn configure_owner_recommend(s: &ServerGuard, mock: &MockLlm) {
    let mgr = SkillManager::with_base(s.data_dir.clone()).expect("owner manager");
    let skill_dir = mgr.paths().skills_dir().join("alpha-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: alpha-skill\ndescription: alpha task\n---\n# alpha\n",
    )
    .unwrap();
    mgr.register_local_skill("alpha-skill").unwrap();
    mgr.db()
        .set_skill_ai_index(
            "alpha-skill",
            &SkillAiIndex {
                summary: "task: alpha task".into(),
                search_doc: "task: alpha task triggers: alpha".into(),
                router_card: "task: alpha task | triggers: alpha | inputs: text | outputs: result | not-for: unrelated".into(),
                llm_score: 8,
                ..SkillAiIndex::default()
            },
        )
        .unwrap();
    RecommendConfig {
        enabled: true,
        provider: Provider::OpenaiCompat,
        base_url: mock.base_url.clone(),
        model: "mock".into(),
        api_key: "test".into(),
        min_prompt_len: 0,
        summary_lang_confirmed: true,
        ..RecommendConfig::default()
    }
    .save(mgr.paths())
    .unwrap();
}

fn register_first(s: &ServerGuard, username: &str, password: &str) -> String {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": username, "password": password}))
        .send()
        .expect("POST /users/register");
    assert_eq!(r.status().as_u16(), 201);
    let body: Value = r.json().expect("register JSON");
    body["api_key"].as_str().expect("api_key").to_string()
}

// ─── owner-mode dashboard contract ───────────────────────────────────────

#[test]
fn owner_me_no_credential_returns_implicit_admin_200() {
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/api/me", s.base_url()))
        .send()
        .expect("GET /api/me");
    assert_eq!(
        r.status().as_u16(),
        200,
        "owner mode: /api/me must succeed without any credential"
    );
    let me: Value = r.json().expect("me JSON");
    assert_eq!(
        me["is_admin"].as_bool(),
        Some(true),
        "owner mode: synthetic owner must be implicit admin, got {me}"
    );
    assert_eq!(
        me["mode"].as_str(),
        Some("owner"),
        "owner mode: MeResp.mode must be \"owner\", got {me}"
    );
    assert!(
        me["user_id"].as_str().is_some(),
        "owner mode: synthetic owner must carry a user_id, got {me}"
    );
    assert!(
        me["username"].as_str().is_some(),
        "owner mode: synthetic owner must carry a username, got {me}"
    );
}

#[test]
fn owner_prefs_routing_mode_roundtrips_through_http_and_db() {
    let s = spawn_server("owner");
    let client = http();
    let initial: Value = client
        .get(format!("{}/api/prefs", s.base_url()))
        .send()
        .expect("GET owner prefs")
        .json()
        .expect("owner prefs JSON");
    assert_eq!(initial["routing_mode"], json!("fast"));

    let updated = client
        .post(format!("{}/api/prefs", s.base_url()))
        .json(&json!({"routing_mode":"precise","show_tradeoff":false}))
        .send()
        .expect("POST owner prefs");
    assert_eq!(updated.status().as_u16(), 200);
    let echoed: Value = updated.json().expect("updated prefs JSON");
    assert_eq!(echoed["routing_mode"], json!("precise"));
    assert_eq!(echoed["show_tradeoff"], json!(false));

    let invalid = client
        .post(format!("{}/api/prefs", s.base_url()))
        .json(&json!({"routing_mode":"turbo","show_tradeoff":true}))
        .send()
        .expect("POST invalid owner prefs");
    assert_eq!(invalid.status().as_u16(), 400);

    let read_back: Value = client
        .get(format!("{}/api/prefs", s.base_url()))
        .send()
        .expect("GET owner prefs after invalid patch")
        .json()
        .expect("owner prefs JSON");
    assert_eq!(read_back["routing_mode"], json!("precise"));
    assert_eq!(read_back["show_tradeoff"], json!(false));

    let conn = rusqlite::Connection::open(s.data_dir.join("runai.db")).expect("open owner db");
    let stored: String = conn
        .query_row(
            "SELECT value FROM app_settings WHERE key='owner_prefs'",
            [],
            |row| row.get(0),
        )
        .expect("owner prefs setting");
    let stored: Value = serde_json::from_str(&stored).expect("stored owner prefs JSON");
    assert_eq!(stored["routing_mode"], json!("precise"));
    assert_eq!(stored["show_tradeoff"], json!(false));
}

#[test]
fn owner_recommend_uses_persisted_routing_mode() {
    let s = spawn_server("owner");
    let mock = MockLlm::start();
    configure_owner_recommend(&s, &mock);
    let client = http();

    let precise = client
        .post(format!("{}/api/prefs", s.base_url()))
        .json(&json!({"routing_mode":"precise"}))
        .send()
        .expect("set owner precise");
    assert_eq!(precise.status().as_u16(), 200);
    let routed = client
        .post(format!("{}/recommend", s.base_url()))
        .json(&json!({"prompt":"alpha task","session_id":"owner-precise"}))
        .send()
        .expect("owner precise recommend");
    assert_eq!(routed.status().as_u16(), 200);
    assert_eq!(
        mock.calls(),
        2,
        "Precise owner request must call expansion + router"
    );

    let conn = rusqlite::Connection::open(s.data_dir.join("runai.db")).unwrap();
    let (mode, calls): (String, i64) = conn
        .query_row(
            "SELECT routing_mode, llm_call_count FROM router_events ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(mode, "precise");
    assert_eq!(calls, 2);

    let fast = client
        .post(format!("{}/api/prefs", s.base_url()))
        .json(&json!({"routing_mode":"fast"}))
        .send()
        .expect("set owner fast");
    assert_eq!(fast.status().as_u16(), 200);
    let routed = client
        .post(format!("{}/recommend", s.base_url()))
        .json(&json!({"prompt":"alpha task","session_id":"owner-fast"}))
        .send()
        .expect("owner fast recommend");
    assert_eq!(routed.status().as_u16(), 200);
    assert_eq!(
        mock.calls(),
        3,
        "Fast owner request adds exactly one router call"
    );
    let (mode, calls): (String, i64) = conn
        .query_row(
            "SELECT routing_mode, llm_call_count FROM router_events ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(mode, "fast");
    assert_eq!(calls, 1);
}

#[test]
fn owner_skills_no_credential_returns_200() {
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/api/skills", s.base_url()))
        .send()
        .expect("GET /api/skills");
    assert_eq!(
        r.status().as_u16(),
        200,
        "owner mode: /api/skills must succeed without any credential \
         (private_data_locked bypass)"
    );
}

#[test]
fn owner_root_body_carries_mode_owner_class() {
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/", s.base_url()))
        .send()
        .expect("GET /");
    assert_eq!(r.status().as_u16(), 200);
    let body = r.text().expect("HTML body");
    assert!(
        body.contains("mode-owner"),
        "owner mode: served HTML <body> must include the `mode-owner` class \
         so CSS can hide the team-only chrome. body excerpt: {}",
        body.lines().take(15).collect::<Vec<_>>().join("\n")
    );
}

// ─── team-mode regression ────────────────────────────────────────────────

#[test]
fn team_me_no_credential_returns_401_regression() {
    let s = spawn_server("team");
    // register one user so private_data_locked = true (mirrors realistic
    // team-mode deployment that already has an account).
    let _ = register_first(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/me", s.base_url()))
        .send()
        .expect("GET /api/me");
    assert_eq!(
        r.status().as_u16(),
        401,
        "team mode regression: /api/me without credential must still 401 \
         (owner-mode bypass must not leak)"
    );
}

#[test]
fn team_root_body_does_not_carry_mode_owner_class() {
    let s = spawn_server("team");
    let r = http()
        .get(format!("{}/", s.base_url()))
        .send()
        .expect("GET /");
    assert_eq!(r.status().as_u16(), 200);
    let body = r.text().expect("HTML body");
    assert!(
        !body.contains("mode-owner"),
        "team mode regression: served HTML <body> must NOT carry \
         `mode-owner` (would unhide team-only chrome inappropriately)"
    );
}

// ─── bundled CSS carries owner-mode rules ────────────────────────────────

#[test]
fn app_css_carries_mode_owner_rules() {
    // The owner-mode chrome cut only works if 13-owner-mode.css is wired
    // into APP_CSS concat (src/server/mod.rs). Verifying the served
    // /app.css bytes contain the load-bearing `.mode-owner` selectors
    // protects against an accidental drop from the concat list when the
    // dashboard is restructured.
    let s = spawn_server("owner");
    let r = http()
        .get(format!("{}/app.css", s.base_url()))
        .send()
        .expect("GET /app.css");
    assert_eq!(r.status().as_u16(), 200);
    let css = r.text().expect("css body");
    for needle in &[
        ".mode-owner #account-pill",
        ".mode-owner #auth-modal",
        ".mode-owner #library-scope-bar",
        ".mode-owner #market-community-pane",
        ":has(#admin-users-rows)",
    ] {
        assert!(
            css.contains(needle),
            "/app.css missing `{needle}` — 13-owner-mode.css must be in APP_CSS concat"
        );
    }
}

#[test]
fn team_me_with_bearer_returns_mode_team() {
    let s = spawn_server("team");
    let key = register_first(&s, "alice", "pw alice 1234");
    let r = http()
        .get(format!("{}/api/me", s.base_url()))
        .bearer_auth(&key)
        .send()
        .expect("GET /api/me");
    assert_eq!(r.status().as_u16(), 200);
    let me: Value = r.json().expect("me JSON");
    assert_eq!(
        me["mode"].as_str(),
        Some("team"),
        "team mode: MeResp.mode must be \"team\", got {me}"
    );
}

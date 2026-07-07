//! PLANNING §2.3 item 5: telemetry / skill browse endpoints must not
//! leak to anonymous callers once the server has held any user or any
//! router_event row. This regresses the owner-mode "users table empty
//! but history populated" carve-out that previously allowed any HTTP
//! client to read every router_event row, including user_prompt /
//! llm_input / hook_output.
//!
//! Pattern mirrors `tests/server_mode_e2e.rs` / `auth_uniform_error_e2e.rs`:
//! spawn the real binary inside an isolated HOME, seed the DB through
//! the public Rust API (no SQLite shell-out), and assert on wire format
//! after the server has bound a real TCP listener.

#![cfg(not(target_os = "windows"))]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
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
    let home = tempfile::tempdir().expect("create tmp HOME");
    std::fs::create_dir_all(home.path().join(".runai/skills")).expect("pre-create .runai/skills");

    let port = free_port();
    let mut cmd = runai_cmd();
    cmd.arg("server")
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
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn runai server");
    let guard = ServerGuard {
        child,
        _home: home,
        port,
    };
    assert!(
        wait_for_port(port, Duration::from_secs(8)),
        "runai team server did not bind 127.0.0.1:{port} within 8s"
    );
    guard
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("reqwest client")
}

/// Seed the server's DB with a single router_event row. Goes through the
/// public Rust API so the test stays valid across schema migrations.
fn seed_one_event(server: &ServerGuard) {
    use runai::core::db::Database;
    use runai::core::db::RouterEvent;
    let db_path = server._home.path().join(".runai/runai.db");
    let db = Database::open(&db_path).expect("open db");
    let now = chrono::Utc::now().timestamp();
    let ev = RouterEvent {
        id: None,
        ts: now,
        provider: "openai-compat".into(),
        model: "fake/model".into(),
        prompt_tokens: 1,
        completion_tokens: 1,
        reasoning_tokens: 0,
        total_tokens: 2,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        latency_ms: 1,
        chosen_skills_json: "[]".into(),
        candidate_count: 0,
        status: "ok".into(),
        error_msg: None,
        session_id: "synthetic".into(),
        mode: "exclusive".into(),
        user_prompt: "synthetic user prompt — must not leak".into(),
        cwd: "/tmp".into(),
        bm25_kept: 0,
        llm_raw_response: "synthetic raw response".into(),
        hook_output: "synthetic hook output".into(),
        llm_input: "synthetic llm input".into(),
        intent_llm_input: "synthetic intent input".into(),
        intent_llm_output: "intent: synthetic".into(),
        intent_status: "ok".into(),
        intent_error_msg: None,
        bm25_candidates_json: "[]".into(),
        user_id: None,
    };
    db.insert_router_event(&ev).expect("seed router event");
}

/// Anonymous reads of telemetry + skill browse on a non-cold server must
/// return 401, not 200 with PII. This is the regression the dashboard
/// had after a long owner-mode session accumulated router_events without
/// ever registering a user.
#[test]
fn anonymous_reads_rejected_when_telemetry_exists() {
    let server = spawn_team_server();
    seed_one_event(&server);
    let client = http_client();

    let events = client
        .get(format!("{}/api/events", server.base_url()))
        .send()
        .expect("GET /api/events");
    assert_eq!(
        events.status().as_u16(),
        401,
        "anonymous /api/events must 401 once router_events is non-empty"
    );

    let summary = client
        .get(format!("{}/api/summary", server.base_url()))
        .send()
        .expect("GET /api/summary");
    assert_eq!(
        summary.status().as_u16(),
        401,
        "anonymous /api/summary must 401 once router_events is non-empty"
    );

    let skills = client
        .get(format!("{}/api/skills", server.base_url()))
        .send()
        .expect("GET /api/skills");
    assert_eq!(
        skills.status().as_u16(),
        401,
        "anonymous /api/skills must 401 once router_events is non-empty"
    );

    let detail = client
        .get(format!("{}/api/skill/no-such-skill", server.base_url()))
        .send()
        .expect("GET /api/skill/<name>");
    assert_eq!(
        detail.status().as_u16(),
        401,
        "anonymous /api/skill/<name> must 401 once router_events is non-empty"
    );
}

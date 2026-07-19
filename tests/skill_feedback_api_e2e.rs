//! e2e tests for the skill-feedback-radar HTTP surface (follow-up to commit
//! 8843476, which added the `skill_feedback` table + `core::skill_metrics`
//! pure math but wired neither into the server):
//!
//!   - `POST /feedback` gains an optional `verdict` (+1/-1 or "good"/"bad"),
//!     `event_id`, `session_id`. A verdict-only request (no `note`) is a
//!     cheap DB insert and must NOT trigger the LLM `reevaluate_skill` call;
//!     a verdict + non-empty `note` does both. The pre-existing `{skill,
//!     note}` body (no `verdict` key at all) must behave byte-identically
//!     to before this change (pinned by the pre-existing
//!     `tests/feedback_auth_e2e.rs` / `tests/recommend_feedback_e2e_p1c0.rs`
//!     suites, which this file does not duplicate).
//!   - `GET /api/skill/{name}` gains `radar` / `radar_avg` / `feedback_stats`
//!     / `feedback_recent`, computed from real router_events / session
//!     adoptions / skill_feedback rows via `core::skill_metrics::compute_radar`.
//!
//! Fixtures are built by calling `runai`-the-library's `Database` methods
//! directly against the SAME sqlite file the spawned server process reads
//! (this crate ships both a bin and a lib target), rather than
//! reimplementing the schema in hand-rolled SQL.

#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};
use tempfile::TempDir;

use runai::core::db::{Database, RouterEvent};
use runai::core::manager::SkillManager;

// ─── generic server harness (mirrors tests/feedback_auth_e2e.rs) ──────────

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
        thread::sleep(Duration::from_millis(50));
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

    fn db(&self) -> Database {
        Database::open(&self.db_path()).expect("open test db")
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `runai server --mode team` against a caller-supplied HOME
/// (`spawn_team_server` below delegates here with a fresh tempdir). This suite
/// disables `SkillWatcher`: it verifies feedback-driven re-enrich claims, and
/// Linux notify backends can emit an initial event burst for skills planted
/// before server startup. Watcher behavior has dedicated coverage elsewhere.
fn spawn_team_server_with_home(home: TempDir) -> ServerGuard {
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
        .env("RUNAI_DISABLE_SKILL_WATCHER", "1")
        .env_remove("RUNE_DATA_DIR")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let g = ServerGuard { child, home, port };
    assert!(
        wait_for_port(g.port, Duration::from_secs(8)),
        "runai server (team mode) never came up"
    );
    g
}

fn spawn_team_server() -> ServerGuard {
    spawn_team_server_with_home(tempfile::tempdir().unwrap())
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

/// Poll `pred` every 50ms up to `timeout`, returning `true` on first
/// success. Re-enrich after `/feedback` runs on a detached background
/// thread (see `recommend.rs::spawn_reevaluate`), so tests that need to
/// observe its result (a written AI summary, an `enrich_status` flip) must
/// poll instead of asserting immediately after the HTTP response returns.
fn wait_for<F: FnMut() -> bool>(mut pred: F, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pred() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

struct Account {
    api_key: String,
    user_id: String,
}

fn register(s: &ServerGuard, u: &str, p: &str) -> Account {
    let r = http()
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(
        r.status().as_u16(),
        201,
        "register must succeed: {}",
        r.text().unwrap_or_default()
    );
    let b: Value = r.json().unwrap();
    Account {
        api_key: b["api_key"].as_str().unwrap().into(),
        user_id: b["user_id"].as_str().unwrap().into(),
    }
}

/// Plant a SKILL.md and register it in-process so the resource row exists
/// before the server opens the DB. Do not use `runai scan` here: its detached
/// enrich child can outlive setup and consume a mock config written nearby.
fn plant_and_register(home: &Path, name: &str, desc: &str) {
    let dir = home.join(".runai/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {desc}\n---\n\n# {name}\n\n{desc}\n"),
    )
    .unwrap();
    let manager = SkillManager::with_base(home.join(".runai")).expect("fixture manager");
    manager
        .register_local_skill(name)
        .expect("register fixture without auto-enrich");
}

fn write_recommend_config(home: &Path, base_url: &str) {
    let toml = format!(
        "[recommend]\n\
         enabled = true\n\
         provider = \"openai-compat\"\n\
         base_url = \"{base_url}\"\n\
         model = \"mock-model\"\n\
         api_key = \"mock-key\"\n\
         top_k = 8\n\
         min_prompt_len = 0\n\
         summary_lang = \"en\"\n"
    );
    std::fs::write(home.join(".runai/config.toml"), toml).unwrap();
}

/// Insert a router_events fixture row directly via the lib's `Database` API
/// so the test doesn't have to hand-maintain the full column list across
/// schema migrations.
fn insert_router_event(
    db: &Database,
    ts: i64,
    session: &str,
    candidates: &[&str],
    chosen: &[&str],
) {
    let ev = RouterEvent {
        id: None,
        ts,
        provider: "test".into(),
        model: "test-model".into(),
        prompt_tokens: 0,
        completion_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        latency_ms: 0,
        chosen_skills_json: serde_json::to_string(chosen).unwrap(),
        candidate_count: candidates.len() as i64,
        status: "ok".into(),
        error_msg: None,
        session_id: session.to_string(),
        mode: "exclusive".into(),
        user_prompt: String::new(),
        cwd: String::new(),
        bm25_kept: candidates.len() as i64,
        llm_raw_response: String::new(),
        hook_output: String::new(),
        llm_input: String::new(),
        intent_llm_input: String::new(),
        intent_llm_output: String::new(),
        intent_status: String::new(),
        intent_error_msg: None,
        bm25_candidates_json: serde_json::to_string(candidates).unwrap(),
        user_id: None,
        ..RouterEvent::default()
    };
    db.insert_router_event(&ev).unwrap();
}

// ─── tiny mock LLM (mirrors tests/feedback_auth_e2e.rs::MockLlm) ──────────

struct MockLlm {
    base_url: String,
    shutdown: Arc<AtomicBool>,
}

impl MockLlm {
    fn start(content: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock LLM");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let body_content = content.to_string();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(2000)))
                            .ok();
                        let mut buf = [0u8; 8192];
                        let mut total = Vec::new();
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) => break,
                                Ok(n) => {
                                    total.extend_from_slice(&buf[..n]);
                                    if total.windows(4).any(|w| w == b"\r\n\r\n") {
                                        let _ = stream.read(&mut buf);
                                        break;
                                    }
                                    if total.len() > 1 << 20 {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let escaped = serde_json::to_string(&body_content).unwrap();
                        let json_body = format!(
                            "{{\"id\":\"mock-1\",\"object\":\"chat.completion\",\"choices\":[{{\"index\":0,\"message\":{{\"role\":\"assistant\",\"content\":{escaped}}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}}}"
                        );
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            json_body.len(),
                            json_body
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self { base_url, shutdown }
    }

    fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

// ─── POST /feedback verdict extension ──────────────────────────────────────

#[test]
fn feedback_verdict_requires_auth_401_empty() {
    let s = spawn_team_server();
    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .json(&json!({"skill": "does-not-exist", "verdict": 1}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    assert!(r.text().unwrap().is_empty());
}

/// A verdict-only request (no `note`) must record the `skill_feedback` row
/// and return fast — the HTTP response comes back before any LLM call
/// starts, because re-enrich is queued on a detached background thread
/// (`recommend.rs::spawn_reevaluate`). This env deliberately has no
/// recommend config / mock LLM at all, so the background re-enrich fails
/// immediately ("runai recommend not configured") and never writes a
/// summary — proven by asserting `skill_ai_index` stays empty even after
/// waiting past the point the background thread must have already run and
/// failed. The response body still reports `reenrich: "queued"` because
/// the claim + spawn happen unconditionally before the client ever sees
/// the response; the ASYNC failure is invisible to the caller (only
/// `tracing::warn!`ed).
///
/// Plants + registers BEFORE the server starts (see
/// `api_skill_detail_enrich_status_unenriched_for_fresh_skill`'s doc
/// comment) so the file watcher's own `spawn_enrich` never races this
/// test's own claim of the `enrich_state` in-flight slot for "alpha".
#[test]
fn feedback_verdict_positive_records_row_without_reevaluate() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    plant_and_register(home.path(), "alpha", "alpha skill description");
    let s = spawn_team_server_with_home(home);
    let alice = register(&s, "alice", "pw alice correct horse");

    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .bearer_auth(&alice.api_key)
        .json(&json!({"skill": "alpha", "verdict": 1}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "{}", r.text().unwrap_or_default());
    let body: Value = r.json().unwrap();
    assert_eq!(
        body["reenrich"], "queued",
        "first vote on a skill must win the re-enrich claim: {body:?}"
    );

    let db = s.db();
    let recent = db.recent_skill_feedback("alpha", 10).unwrap();
    assert_eq!(recent.len(), 1, "exactly one feedback row must be written");
    assert_eq!(recent[0].skill_name, "alpha");
    assert_eq!(recent[0].verdict, 1);
    assert_eq!(recent[0].user_id.as_deref(), Some(alice.user_id.as_str()));
    assert_eq!(recent[0].owner_user_id, None, "alpha is a public skill");

    // Give the detached background thread time to run its (doomed, no
    // config) reevaluate_skill call and fail, then assert it never wrote a
    // summary — the failure is silent to the client by design.
    thread::sleep(Duration::from_millis(300));
    assert!(
        db.skill_ai_index("alpha").unwrap().is_none(),
        "unconfigured recommend must fail the background re-enrich without writing a summary"
    );

    // GET /api/skill/alpha shows enrich_status == "enriching": the mark was
    // set (in the request handler, before responding) and is never cleared
    // on a background failure — it just sits until the 300s TTL.
    let detail: Value = http()
        .get(format!("{}/api/skill/alpha", s.base_url()))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(
        detail["enrich_status"], "enriching",
        "in-flight mark must show 'enriching' even after the async reevaluate failed: {detail:?}"
    );

    // A second vote on the same (still in-flight) skill must NOT win a
    // fresh claim — the response says "already-running", not "queued".
    let r2 = http()
        .post(format!("{}/feedback", s.base_url()))
        .bearer_auth(&alice.api_key)
        .json(&json!({"skill": "alpha", "verdict": -1}))
        .send()
        .unwrap();
    assert_eq!(r2.status().as_u16(), 200);
    let body2: Value = r2.json().unwrap();
    assert_eq!(
        body2["reenrich"], "already-running",
        "a second vote while the first re-enrich claim is still fresh must not queue a duplicate: {body2:?}"
    );
}

#[test]
fn feedback_verdict_string_bad_maps_to_negative_one() {
    let s = spawn_team_server();
    let alice = register(&s, "alice", "pw alice correct horse");
    plant_and_register(s.home.path(), "alpha", "alpha skill description");

    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .bearer_auth(&alice.api_key)
        .json(&json!({"skill": "alpha", "verdict": "bad"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200, "{}", r.text().unwrap_or_default());

    let db = s.db();
    let recent = db.recent_skill_feedback("alpha", 10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].verdict, -1);
}

#[test]
fn feedback_verdict_zero_is_400() {
    let s = spawn_team_server();
    let alice = register(&s, "alice", "pw alice correct horse");
    plant_and_register(s.home.path(), "alpha", "alpha skill description");

    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .bearer_auth(&alice.api_key)
        .json(&json!({"skill": "alpha", "verdict": 0}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 400);

    let db = s.db();
    assert!(
        db.recent_skill_feedback("alpha", 10).unwrap().is_empty(),
        "an invalid verdict must not be recorded"
    );
}

#[test]
fn feedback_verdict_nonexistent_skill_404() {
    let s = spawn_team_server();
    let alice = register(&s, "alice", "pw alice correct horse");

    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .bearer_auth(&alice.api_key)
        .json(&json!({"skill": "ghost", "verdict": 1}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    assert!(r.text().unwrap().is_empty());
}

/// verdict + a non-empty note must do BOTH: record the structured feedback
/// row AND queue the existing LLM reevaluate flow. Re-enrich now runs on a
/// detached background thread (`recommend.rs::spawn_reevaluate`) rather
/// than inline before the response — the HTTP response comes back with
/// `reenrich: "queued"` immediately, and the resulting summary/score show
/// up asynchronously, so this test polls for it instead of asserting
/// right after the response (was: synchronous, asserted the score
/// immediately post-response).
///
/// Config + skill are written BEFORE the server starts (see
/// `api_skill_detail_enrich_status_unenriched_for_fresh_skill`'s doc
/// comment) so the file watcher never races this test's own claim of the
/// `enrich_state` in-flight slot for "alpha".
#[test]
fn feedback_verdict_and_note_together_records_feedback_and_reevaluates() {
    let mock = MockLlm::start(
        "task: refined task\ntriggers: alpha\ninputs: x\noutputs: y\nnot-for: z\nscore: 4\n",
    );
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    write_recommend_config(home.path(), mock.base_url());
    plant_and_register(home.path(), "alpha", "alpha skill description");
    let s = spawn_team_server_with_home(home);
    let alice = register(&s, "alice", "pw alice correct horse");

    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .bearer_auth(&alice.api_key)
        .json(&json!({"skill": "alpha", "verdict": -1, "note": "too narrow"}))
        .send()
        .unwrap();
    let status = r.status().as_u16();
    let body = r.text().unwrap();
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("feedback applied by alice"));
    assert!(
        body.contains("\"reenrich\":\"queued\""),
        "response must report the re-enrich as queued: {body}"
    );

    let db = s.db();
    let recent = db.recent_skill_feedback("alpha", 10).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].verdict, -1);
    assert_eq!(recent[0].note.as_deref(), Some("too narrow"));

    // The skill_feedback row above is written synchronously before the
    // response returns; the AI summary write happens asynchronously on
    // the background thread, so poll for it.
    assert!(
        wait_for(
            || db
                .skill_ai_index("alpha")
                .unwrap()
                .is_some_and(|idx| idx.llm_score == 4),
            Duration::from_secs(5)
        ),
        "background re-enrich must eventually write llm_score=4 (verdict+note case): {:?}",
        db.skill_ai_index("alpha").unwrap()
    );
}

/// The pre-existing `{skill, note}` body with NO `verdict` key at all must
/// still record no `skill_feedback` row (that table is new; a caller who
/// never sends `verdict` never triggers it) and must still attribute the
/// response to the authenticated user — but re-enrich now runs
/// asynchronously (was: synchronous, response text embedded the resulting
/// `llm_score`), so the score assertion polls instead of reading it
/// straight off the HTTP response.
///
/// Config + skill are written BEFORE the server starts (see
/// `api_skill_detail_enrich_status_unenriched_for_fresh_skill`'s doc
/// comment) so the file watcher never races this test's own claim of the
/// `enrich_state` in-flight slot for "alpha".
#[test]
fn feedback_legacy_body_without_verdict_field_unaffected() {
    let mock =
        MockLlm::start("task: t\ntriggers: alpha\ninputs: x\noutputs: y\nnot-for: z\nscore: 5\n");
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    write_recommend_config(home.path(), mock.base_url());
    plant_and_register(home.path(), "alpha", "alpha skill description");
    let s = spawn_team_server_with_home(home);
    let alice = register(&s, "alice", "pw alice correct horse");

    let r = http()
        .post(format!("{}/feedback", s.base_url()))
        .bearer_auth(&alice.api_key)
        .json(&json!({"skill": "alpha", "note": "legacy path"}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let body = r.text().unwrap();
    assert!(body.contains("feedback applied by alice"));
    assert!(
        body.contains("\"reenrich\":\"queued\""),
        "response must report the re-enrich as queued: {body}"
    );

    let db = s.db();
    assert!(
        db.recent_skill_feedback("alpha", 10).unwrap().is_empty(),
        "a request with no verdict field must never write a skill_feedback row"
    );
    assert!(
        wait_for(
            || db
                .skill_ai_index("alpha")
                .unwrap()
                .is_some_and(|idx| idx.llm_score == 5),
            Duration::from_secs(5)
        ),
        "background re-enrich must eventually write llm_score=5 (legacy note-only path): {:?}",
        db.skill_ai_index("alpha").unwrap()
    );
}

// ─── GET /api/skill/{name} radar + feedback_stats extension ───────────────

/// A skill with no AI summary and no in-flight enrich mark must report
/// `enrich_status: "unenriched"` on the detail endpoint — the third leg of
/// the 3-state contract (`"enriched"` is covered by
/// `api_skill_detail_includes_radar_and_feedback_stats_matching_fixture`,
/// `"enriching"` by `feedback_verdict_positive_records_row_without_reevaluate`).
///
/// Plants + registers before the server starts. The suite disables the skill
/// watcher so the assertion observes only feedback-owned enrich state, not
/// platform-specific notify startup events.
#[test]
fn api_skill_detail_enrich_status_unenriched_for_fresh_skill() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    plant_and_register(home.path(), "gamma", "gamma skill description");
    let s = spawn_team_server_with_home(home);
    let alice = register(&s, "alice", "pw alice correct horse");

    let body: Value = http()
        .get(format!("{}/api/skill/gamma", s.base_url()))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(body["enrich_status"], "unenriched", "{body:?}");
}

#[test]
fn api_skill_detail_includes_radar_and_feedback_stats_matching_fixture() {
    let s = spawn_team_server();
    let alice = register(&s, "alice", "pw alice correct horse");
    plant_and_register(s.home.path(), "alpha", "alpha skill description");
    plant_and_register(s.home.path(), "beta", "beta skill description");

    let db = s.db();
    db.set_skill_ai_summary_scored("alpha", "alpha summary text", 8)
        .unwrap();
    db.set_skill_ai_summary_scored("beta", "beta summary text", 4)
        .unwrap();
    db.conn_ref()
        .execute(
            "UPDATE resources SET usage_count=?1 WHERE name=?2",
            rusqlite::params![10i64, "alpha"],
        )
        .unwrap();
    db.conn_ref()
        .execute(
            "UPDATE resources SET usage_count=?1 WHERE name=?2",
            rusqlite::params![20i64, "beta"],
        )
        .unwrap();

    let now = chrono::Utc::now().timestamp();
    // alpha: candidate in all 3 events, chosen in 2, adopted in 1 of those 2.
    // beta: candidate in 2 of 3 events, never chosen.
    insert_router_event(&db, now, "s1", &["alpha", "beta"], &[]);
    insert_router_event(&db, now, "s2", &["alpha"], &["alpha"]);
    insert_router_event(&db, now, "s3", &["alpha", "beta"], &["alpha"]);
    db.record_session_adoption("s2", "alpha").unwrap();

    db.record_skill_feedback(
        now,
        "alpha",
        None,
        Some(&alice.user_id),
        None,
        None,
        1,
        None,
    )
    .unwrap();
    db.record_skill_feedback(
        now,
        "alpha",
        None,
        Some(&alice.user_id),
        None,
        None,
        1,
        None,
    )
    .unwrap();
    db.record_skill_feedback(
        now,
        "alpha",
        None,
        Some(&alice.user_id),
        None,
        None,
        -1,
        Some("meh"),
    )
    .unwrap();

    let r = http()
        .get(format!("{}/api/skill/alpha", s.base_url()))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap();
    let status = r.status().as_u16();
    let body: Value = r.json().unwrap();
    assert_eq!(status, 200, "{body:?}");

    let fb = &body["feedback_stats"];
    assert_eq!(fb["pos"], 2);
    assert_eq!(fb["neg"], 1);
    assert_eq!(fb["candidate_events"], 3);
    assert_eq!(fb["chosen_events"], 2);
    assert_eq!(fb["chosen_sessions"], 2);
    assert_eq!(fb["adopted_sessions"], 1);

    // alpha has a non-empty summary and no in-flight enrich_state mark (no
    // /feedback vote happened in this test) → "enriched".
    assert_eq!(body["enrich_status"], "enriched", "{body:?}");

    // radar: recompute via the SAME pure formulas the server uses (already
    // unit-pinned in skill_metrics.rs) — this test's job is to pin the
    // SERVER WIRING (which raw counts feed which axis), not re-derive the
    // math by hand.
    let expected = runai::core::skill_metrics::compute_radar(1, 2, 2, 3, 2, 1, Some(8), 10, 20);
    assert_eq!(expected.quality, 8.0);
    let radar = &body["radar"];
    let eps = 1e-9;
    assert!((radar["adoption"].as_f64().unwrap() - expected.adoption).abs() < eps);
    assert!((radar["precision"].as_f64().unwrap() - expected.precision).abs() < eps);
    assert!((radar["rating"].as_f64().unwrap() - expected.rating).abs() < eps);
    assert!((radar["quality"].as_f64().unwrap() - expected.quality).abs() < eps);
    assert!((radar["heat"].as_f64().unwrap() - expected.heat).abs() < eps);

    // radar_avg: mean of alpha's and beta's radar (both enriched skills in
    // the caller's owner scope).
    let beta_radar = runai::core::skill_metrics::compute_radar(0, 0, 0, 2, 0, 0, Some(4), 20, 20);
    let avg = &body["radar_avg"];
    let expect_avg = |a: f64, b: f64| (a + b) / 2.0;
    assert!(
        (avg["adoption"].as_f64().unwrap() - expect_avg(expected.adoption, beta_radar.adoption))
            .abs()
            < 1e-6
    );
    assert!(
        (avg["quality"].as_f64().unwrap() - expect_avg(expected.quality, beta_radar.quality)).abs()
            < 1e-6
    );
    assert!(
        (avg["heat"].as_f64().unwrap() - expect_avg(expected.heat, beta_radar.heat)).abs() < 1e-6
    );
}

/// `feedback_recent` shows the verdict/note to everyone but only shows the
/// AUTHOR's `user_id` to an admin viewer — a non-admin sees `null`.
#[test]
fn api_skill_detail_feedback_recent_hides_user_id_from_non_admin() {
    let s = spawn_team_server();
    // First registered user is auto-admin.
    let alice = register(&s, "alice", "pw alice correct horse");
    let bob = register(&s, "bob", "pw bob correct horse too");
    plant_and_register(s.home.path(), "alpha", "alpha skill description");

    let db = s.db();
    db.set_skill_ai_summary_scored("alpha", "alpha summary text", 6)
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    db.record_skill_feedback(
        now,
        "alpha",
        None,
        Some(&bob.user_id),
        None,
        None,
        1,
        Some("nice"),
    )
    .unwrap();

    let r_bob = http()
        .get(format!("{}/api/skill/alpha", s.base_url()))
        .bearer_auth(&bob.api_key)
        .send()
        .unwrap();
    assert_eq!(r_bob.status().as_u16(), 200);
    let body_bob: Value = r_bob.json().unwrap();
    let recent_bob = body_bob["feedback_recent"].as_array().unwrap();
    assert_eq!(recent_bob.len(), 1);
    assert!(
        recent_bob[0]["user_id"].is_null(),
        "non-admin must not see the feedback author's user_id: {recent_bob:?}"
    );
    assert_eq!(recent_bob[0]["verdict"], 1);
    assert_eq!(recent_bob[0]["note"], "nice");

    let r_admin = http()
        .get(format!("{}/api/skill/alpha", s.base_url()))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap();
    assert_eq!(r_admin.status().as_u16(), 200);
    let body_admin: Value = r_admin.json().unwrap();
    let recent_admin = body_admin["feedback_recent"].as_array().unwrap();
    assert_eq!(recent_admin.len(), 1);
    assert_eq!(recent_admin[0]["user_id"], bob.user_id);
}

/// `GET /api/skill/{name}` caches `Database::skill_router_stats` /
/// `skill_feedback_counts_all` process-wide with a TTL (`server::stats_cache`,
/// 2026-07 blocking-runtime audit) — a full-table scan plus an N+1
/// `router_session_adoptions` query per chosen session, which is expensive
/// enough on a busy install that recomputing it on every 5s poll of this
/// endpoint was a real contributor to a full-server hang. Prove the cache is
/// actually live at the HTTP boundary (not just in the unit tests colocated
/// with the cache module itself): a router_events row inserted directly into
/// the DB (bypassing the server, so the cache can't see it any other way)
/// must NOT move `feedback_stats.candidate_events` on an immediate follow-up
/// request within the TTL window — the accepted "stats can lag up to the TTL"
/// tradeoff, from the caller's point of view.
#[test]
fn api_skill_detail_router_stats_are_cached_within_ttl() {
    let s = spawn_team_server();
    let alice = register(&s, "alice", "pw alice correct horse");
    plant_and_register(s.home.path(), "alpha", "alpha skill description");

    let db = s.db();
    let now = chrono::Utc::now().timestamp();
    insert_router_event(&db, now, "cache-s1", &["alpha"], &["alpha"]);

    let r1 = http()
        .get(format!("{}/api/skill/alpha", s.base_url()))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap();
    assert_eq!(r1.status().as_u16(), 200);
    let body1: Value = r1.json().unwrap();
    assert_eq!(
        body1["feedback_stats"]["candidate_events"], 1,
        "priming request must see the one router_event fixture: {body1:?}"
    );

    // Bypass the server entirely — insert straight into the DB it reads.
    // Within the cache TTL, the next request must still report the STALE
    // count, proving the response came from the cache and not a fresh scan.
    insert_router_event(&db, now, "cache-s2", &["alpha"], &["alpha"]);

    let r2 = http()
        .get(format!("{}/api/skill/alpha", s.base_url()))
        .bearer_auth(&alice.api_key)
        .send()
        .unwrap();
    assert_eq!(r2.status().as_u16(), 200);
    let body2: Value = r2.json().unwrap();
    assert_eq!(
        body2["feedback_stats"]["candidate_events"], 1,
        "a request within the TTL must return the cached snapshot, not re-scan router_events: {body2:?}"
    );
}

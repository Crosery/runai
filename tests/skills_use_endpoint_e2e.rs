//! Phase 1 e2e: the new idempotent `POST /skills/use/{name}` endpoint.
//!
//! Contract under test (PLANNING §1.3 activation/feedback protocol):
//!   - First event with a given `X-Runai-Event-Id` records usage
//!     (resources.usage_count +1) + a `usage_events` row, and returns
//!     `{ok:true, skill, usage_count, files:[...]}`.
//!   - Same event_id + same payload (canonical hash) is a 200 no-op:
//!     usage_count does NOT bump again.
//!   - Same event_id + different payload is 409 conflict, body has no
//!     internal absolute paths.
//!   - Missing `X-Runai-Event-Id` is 422.
//!   - Unknown skill is 404, no DB write.
//!   - Traversal-flavored name in path is 404 (reuses is_safe_skill_name).
//!   - Anonymous (no Authorization) works; a stale Bearer fails closed 401
//!     empty body (matches /recommend + /feedback anti-enumeration style).
//!   - Response body never contains `curl` or `/skills/file/` (the
//!     sibling-file appendix is a /skills/get-compat-only concern).
//!   - Idempotency is persisted in runai.db, so a server restart treats
//!     a repeated event_id + payload as a no-op (not a fresh increment).
//!
//! All tests run against an isolated HOME tempdir + RUNE_DATA_DIR — the
//! real `~/.runai/` is never touched.

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
        assert!(
            wait_for_port(port, Duration::from_secs(8)),
            "server never came up on {port}"
        );
        s
    }

    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn db_path(&self) -> std::path::PathBuf {
        self.home.path().join(".runai/runai.db")
    }

    /// Plant a skill on disk + run `runai scan` (subprocess, same HOME /
    /// RUNE_DATA_DIR) so the resources row exists.
    fn plant(&self, name: &str, body: &str, extra: &[(&str, &str)]) {
        let dir = self.home.path().join(".runai/skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {body}\n---\n\n# {name}\n\n{body}\n"),
        )
        .unwrap();
        for (rel, content) in extra {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        let out = runai_cmd()
            .arg("scan")
            .env("HOME", self.home.path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.home.path().join(".runai"))
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .expect("runai scan");
        assert!(
            out.status.success(),
            "scan failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
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

    fn session_adoption_count(&self, session_id: &str, skill: &str) -> i64 {
        let conn = rusqlite::Connection::open(self.db_path()).unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM router_session_adoptions WHERE session_id=?1 AND skill_name=?2",
            rusqlite::params![session_id, skill],
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

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}

fn post_use(
    s: &Server,
    name: &str,
    event_id: Option<&str>,
    body: &Value,
    bearer: Option<&str>,
) -> (u16, String) {
    let mut req = http().post(format!("{}/skills/use/{}", s.base(), name));
    if let Some(eid) = event_id {
        req = req.header("X-Runai-Event-Id", eid);
    }
    if let Some(b) = bearer {
        req = req.bearer_auth(b);
    }
    let resp = req.json(body).send().unwrap();
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    (status, text)
}

#[test]
fn skills_use_first_event_records_usage_and_returns_ack() {
    let s = Server::spawn();
    s.plant("foo", "foo skill", &[("references/ref-a.md", "ref body")]);
    let (status, body) = post_use(&s, "foo", Some("e1"), &json!({}), None);
    assert_eq!(status, 200, "body: {body}");
    let v: Value = serde_json::from_str(&body).expect("parse json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["skill"], "foo");
    let uc = v["usage_count"].as_i64().expect("usage_count int");
    assert!(uc >= 1, "usage_count >= 1, got {uc}");
    let files = v["files"].as_array().expect("files array");
    assert!(
        files.iter().any(|f| f == "references/ref-a.md"),
        "files should list sibling refs: {files:?}"
    );
    assert_eq!(s.usage_count("foo"), uc);
    assert_eq!(s.usage_event_count("e1"), 1);
}

#[test]
fn skills_use_same_event_id_same_payload_is_noop() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[]);
    let (st1, b1) = post_use(&s, "foo", Some("e1"), &json!({}), None);
    assert_eq!(st1, 200);
    let v1: Value = serde_json::from_str(&b1).unwrap();
    let after_first = s.usage_count("foo");
    let (st2, b2) = post_use(&s, "foo", Some("e1"), &json!({}), None);
    assert_eq!(st2, 200, "dup should be 200: {b2}");
    let v2: Value = serde_json::from_str(&b2).unwrap();
    assert_eq!(v2["usage_count"], v1["usage_count"], "no second bump");
    assert_eq!(s.usage_count("foo"), after_first, "DB unchanged on dup");
    assert_eq!(s.usage_event_count("e1"), 1);
}

#[test]
fn skills_use_same_event_id_different_payload_returns_409() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[]);
    let (st1, _) = post_use(&s, "foo", Some("e1"), &json!({}), None);
    assert_eq!(st1, 200);
    let after_first = s.usage_count("foo");
    let (st2, b2) = post_use(&s, "foo", Some("e1"), &json!({"include": ["x.md"]}), None);
    assert_eq!(st2, 409, "conflict should be 409: {b2}");
    assert!(
        b2.to_lowercase().contains("conflict"),
        "409 body must mention conflict: {b2}"
    );
    // No internal absolute paths leak.
    for needle in ["/Users/", "/home/", ".runai/", "target/"] {
        assert!(!b2.contains(needle), "409 body leaks {needle}: {b2}");
    }
    assert_eq!(s.usage_count("foo"), after_first, "no bump on conflict");
    assert_eq!(s.usage_event_count("e1"), 1, "no second event row");
}

#[test]
fn skills_use_missing_event_id_returns_422() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[]);
    let (status, body) = post_use(&s, "foo", None, &json!({}), None);
    assert!(
        status == 422 || status == 400,
        "missing event_id should be 422/400, got {status}: {body}"
    );
    assert!(
        body.contains("event_id"),
        "body must mention event_id: {body}"
    );
}

#[test]
fn skills_use_session_id_propagates_to_session_adoptions() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[]);
    let sid = "rnai_sess_0123456789abcdef0123456789abcdef";
    let (status, body) = post_use(&s, "foo", Some("e-sess"), &json!({"session_id": sid}), None);
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(s.session_adoption_count(sid, "foo"), 1);
}

#[test]
fn skills_use_rejects_non_runai_session_id() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[]);
    let (status, body) = post_use(
        &s,
        "foo",
        Some("e-bad-sess"),
        &json!({"session_id": "sess-xyz"}),
        None,
    );
    assert_eq!(status, 422, "bad session_id should be rejected: {body}");
    assert_eq!(s.usage_event_count("e-bad-sess"), 0);
    assert_eq!(s.session_adoption_count("sess-xyz", "foo"), 0);
}

#[test]
fn skills_use_traversal_event_id_in_path_is_rejected() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[]);
    // Encoded `..` — axum decodes path segments; the handler must reject
    // via is_safe_skill_name. We send a valid event_id so the rejection
    // is on the name, not on the missing event_id.
    let url = format!("{}/skills/use/..%2fbar", s.base());
    let resp = http()
        .post(&url)
        .header("X-Runai-Event-Id", "e-trav")
        .json(&json!({}))
        .send()
        .unwrap();
    let status = resp.status().as_u16();
    assert_eq!(status, 404, "traversal name must be 404, got {status}");
    assert_eq!(
        s.usage_event_count("e-trav"),
        0,
        "no event row on traversal reject"
    );
}

#[test]
fn skills_use_unknown_skill_returns_404_no_db_write() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[]);
    let (status, _body) = post_use(&s, "missing", Some("e2"), &json!({}), None);
    assert_eq!(status, 404, "unknown skill must be 404");
    assert_eq!(s.usage_event_count("e2"), 0, "no event row on 404");
}

#[test]
fn skills_use_anonymous_works_but_stale_bearer_401() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[]);
    // Anonymous works (A1: aligned with /skills/get).
    let (st, _b) = post_use(&s, "foo", Some("e-anon"), &json!({}), None);
    assert_eq!(st, 200, "anonymous /skills/use must work");
    // Stale Bearer fails closed, empty body.
    let (st2, b2) = post_use(
        &s,
        "foo",
        Some("e-stale"),
        &json!({}),
        Some("rnai_live_stale"),
    );
    assert_eq!(st2, 401, "stale bearer must 401");
    assert!(b2.is_empty(), "401 body must be empty, got: {b2:?}");
    assert_eq!(s.usage_event_count("e-stale"), 0, "no event row on 401");
}

#[test]
fn skills_use_does_not_return_sibling_curl_appendix() {
    let s = Server::spawn();
    s.plant("foo", "foo", &[("references/ref-a.md", "x")]);
    let (st, body) = post_use(&s, "foo", Some("e-ax"), &json!({}), None);
    assert_eq!(st, 200);
    assert!(
        !body.contains("curl"),
        "response must not contain curl appendix: {body}"
    );
    assert!(
        !body.contains("/skills/file/"),
        "response must not contain /skills/file/ appendix: {body}"
    );
}

#[test]
fn skills_use_survives_server_restart_idempotency_persisted() {
    // Idempotency lives in runai.db, not memory: a fresh server process
    // against the same RUNE_DATA_DIR must treat a repeated event_id +
    // payload as a no-op rather than a second increment.
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
    let data_dir = home.path().join(".runai");
    // plant skill
    {
        let dir = data_dir.join("skills/foo");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: foo\ndescription: x\n---\n\n# foo\n",
        )
        .unwrap();
        let out = runai_cmd()
            .arg("scan")
            .env("HOME", home.path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", &data_dir)
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .output()
            .unwrap();
        assert!(out.status.success(), "scan failed");
    }
    let port = free_port();
    let mut child = runai_cmd()
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
    let base = format!("http://127.0.0.1:{port}");
    let resp = http()
        .post(format!("{base}/skills/use/foo"))
        .header("X-Runai-Event-Id", "e-persist")
        .json(&json!({}))
        .send()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
    let after_first = {
        let conn = rusqlite::Connection::open(data_dir.join("runai.db")).unwrap();
        conn.query_row(
            "SELECT usage_count FROM resources WHERE name='foo'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    // kill + restart
    let _ = child.kill();
    let _ = child.wait();
    let port2 = free_port();
    let mut child2 = runai_cmd()
        .args([
            "server",
            "--host",
            "127.0.0.1",
            "--port",
            &port2.to_string(),
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
    assert!(wait_for_port(port2, Duration::from_secs(8)));
    let base2 = format!("http://127.0.0.1:{port2}");
    let resp2 = http()
        .post(format!("{base2}/skills/use/foo"))
        .header("X-Runai-Event-Id", "e-persist")
        .json(&json!({}))
        .send()
        .unwrap();
    assert_eq!(
        resp2.status().as_u16(),
        200,
        "repeated event_id should be 200 no-op after restart"
    );
    let after_second = {
        let conn = rusqlite::Connection::open(data_dir.join("runai.db")).unwrap();
        conn.query_row(
            "SELECT usage_count FROM resources WHERE name='foo'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert_eq!(
        after_second, after_first,
        "no second increment after restart"
    );
    let _ = child2.kill();
    let _ = child2.wait();
}

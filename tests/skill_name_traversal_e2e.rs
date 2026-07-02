//! C1: the disk-fallback in `resolve_skill_dir_scoped` must reject a route
//! `name` that is a path-traversal token (`..`, embedded separators, control
//! chars) BEFORE it is joined onto `skills_dir()`.
//!
//! Live bug this pins: `paths.skills_dir().join(name)` joined the raw route
//! `name` with no sanitization. `name=".."` (sendable as the percent-encoded
//! `%2e%2e`, which axum's Path extractor decodes to `..` after matchit routed
//! on the raw segment) made `skills_dir().join("..")` resolve to the whole
//! `<data>` directory, which `.exists()`, so the three anonymous content routes
//! served it:
//!   - `GET /skills/bundle/%2e%2e` tarred the ENTIRE data dir (runai.db with
//!     argon2 password + api_key hashes, every user's private pool, trash,
//!     backups) over HTTP.
//!   - `GET /skills/file/%2e%2e/runai.db` read the raw SQLite DB file.
//!   - `POST /skills/get/%2e%2e` resolved above the public skills dir.
//!
//! Contract: any traversal-flavored `name` → 404 (empty), while a genuine
//! public skill name still resolves 200. The requests are sent over a RAW TCP
//! socket because reqwest/`url` normalizes `%2e%2e` path segments away before
//! the bytes ever reach the wire — the whole point is to smuggle the literal
//! `%2e%2e` segment to the server.

#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::json;
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
    fn data(&self) -> std::path::PathBuf {
        self.home.path().join(".runai")
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

/// Send a request with the target string byte-for-byte (no URL normalization)
/// and return `(status_code, body_bytes)`. This is the only way to deliver a
/// literal `%2e%2e` path segment — reqwest collapses it client-side.
fn raw_request(port: u16, method: &str, target: &str) -> (u16, Vec<u8>) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let req = format!("{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    // status line: "HTTP/1.1 404 Not Found\r\n"
    let head_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let first = head.lines().next().unwrap_or("");
    let status = first
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body = if head_end + 4 <= buf.len() {
        buf[head_end + 4..].to_vec()
    } else {
        Vec::new()
    };
    (status, body)
}

/// The SQLite file header magic — every `runai.db` file starts with this. A
/// traversal response must NEVER contain it, even if a future regression
/// returns `200`-with-error-body (or a bundle tar) that pattern-matches "pass"
/// on status code alone.
const SQLITE_MAGIC: &[u8] = b"SQLite format 3";

fn assert_no_db_leak(body: &[u8], ctx: &str) {
    assert!(
        !body.windows(SQLITE_MAGIC.len()).any(|w| w == SQLITE_MAGIC),
        "{ctx}: response body must not contain the SQLite header magic (runai.db leaked)"
    );
}

fn register(s: &ServerGuard, u: &str, p: &str) {
    let c = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let r = c
        .post(format!("{}/users/register", s.base_url()))
        .json(&json!({"username": u, "password": p}))
        .send()
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
}
fn plant_skill(dir: &std::path::Path, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("SKILL.md"), body).unwrap();
}

#[test]
fn traversal_name_cannot_escape_public_skills_dir() {
    let s = spawn_server();
    // Registering mints runai.db (argon2 hashes + api_key hashes) — the exact
    // asset the traversal bundle would have leaked.
    register(&s, "admin", "pw admin 1234");
    assert!(s.data().join("runai.db").exists());

    // A genuine public skill so we can prove normal resolution still works.
    plant_skill(
        &s.data().join("skills/normal-skill"),
        "---\nname: normal-skill\n---\nPUBLIC_BODY\n",
    );

    // ── traversal must be refused on every anonymous content route ──
    // /skills/bundle/{name}: name=".." previously tarred the whole data dir.
    let (st, body) = raw_request(s.port, "GET", "/skills/bundle/%2e%2e");
    assert_eq!(st, 404, "bundle traversal must 404, not tar the data dir");
    assert_no_db_leak(&body, "bundle %2e%2e");

    // /skills/file/{name}/{*path}: name=".." + path=runai.db read the raw DB.
    let (st, body) = raw_request(s.port, "GET", "/skills/file/%2e%2e/runai.db");
    assert_eq!(st, 404, "file traversal must 404, not read runai.db");
    assert_no_db_leak(&body, "file %2e%2e/runai.db");

    // /skills/get/{name} (POST): name=".." resolved above the public dir.
    let (st, _body) = raw_request(s.port, "POST", "/skills/get/%2e%2e");
    assert_eq!(st, 404, "get traversal must 404");

    // A literal (un-encoded) `..` segment on the file route too — some clients
    // don't normalize. Deeper escape attempt to a well-known system file.
    let (st, body) = raw_request(s.port, "GET", "/skills/bundle/%2e%2e%2f%2e%2e");
    assert_eq!(st, 404, "nested traversal must 404");
    assert_no_db_leak(&body, "bundle nested %2e%2e");

    // ── genuine public skill still resolves ──
    let (st, body) = raw_request(s.port, "POST", "/skills/get/normal-skill");
    assert_eq!(st, 200, "a real public skill must still resolve");
    assert!(!body.is_empty(), "real skill body must be non-empty");
    let (st, _body) = raw_request(s.port, "GET", "/skills/bundle/normal-skill");
    assert_eq!(st, 200, "a real public skill bundle must still resolve");
}

/// Hardening cases (F6 test-completeness pass, per the C1 adversarial eval):
/// these pin traversal variants the first fn didn't wire-level cover, plus the
/// path-side `{*path}` guard and the "single-decode" invariant.
#[test]
fn traversal_hardening_variants() {
    let s = spawn_server();
    register(&s, "admin", "pw admin 1234");
    assert!(s.data().join("runai.db").exists());
    plant_skill(
        &s.data().join("skills/normal-skill"),
        "---\nname: normal-skill\n---\nPUBLIC_BODY\n",
    );

    // (1) bare `.` / `%2e` single-dot: `skills_dir().join(".")` is the public
    //     skills dir ITSELF, so an unguarded bundle would tar the entire public
    //     pool. Must 404. (`is_safe_skill_name` rejects `.`; `%2e` decodes to
    //     `.` and is likewise rejected.)
    for target in [
        "/skills/bundle/%2e",
        "/skills/get/%2e", // note: get is POST below
    ] {
        let method = if target.contains("/get/") {
            "POST"
        } else {
            "GET"
        };
        let (st, body) = raw_request(s.port, method, target);
        assert_eq!(st, 404, "single-dot name must 404: {target}");
        assert_no_db_leak(&body, target);
    }

    // (2) legit name + path-SIDE traversal on `/skills/file/{name}/{*path}`.
    //     The name is valid, so `is_safe_skill_name` passes and resolution
    //     succeeds — this exercises the SEPARATE canonicalize + starts_with
    //     guard on the wildcard `{*path}` (skills.rs handle_skill_file), which
    //     the first fn never touched. `../../runai.db` must not escape the
    //     resolved skill dir.
    let (st, body) = raw_request(
        s.port,
        "GET",
        "/skills/file/normal-skill/%2e%2e%2f%2e%2e%2frunai.db",
    );
    assert_eq!(st, 404, "path-side traversal must 404 (path guard)");
    assert_no_db_leak(&body, "path-side ../../runai.db");

    // (3a) overlong-UTF-8 `%c0%ae` (a non-canonical encoding of `.`): must NOT
    //      be silently folded to `.` and served. axum decodes the two bytes to
    //      invalid UTF-8, so this is rejected (400) rather than 200 — the point
    //      is it never resolves to the skills dir.
    let (st, body) = raw_request(s.port, "GET", "/skills/bundle/%c0%ae%c0%ae");
    assert_ne!(st, 200, "overlong-UTF-8 dot must not resolve (got 200)");
    assert_no_db_leak(&body, "overlong %c0%ae");

    // (3b) double-encoded `%252e%252e`: decoded ONCE this is the literal string
    //      "%2e%2e" (a harmless, non-existent single-segment name) → 404. If a
    //      future change ever added a SECOND decode pass, it would collapse to
    //      ".." and re-open the hole. Pins the single-decode invariant.
    let (st, body) = raw_request(s.port, "GET", "/skills/bundle/%252e%252e");
    assert_eq!(st, 404, "double-encoded name must 404 (single-decode only)");
    assert_no_db_leak(&body, "double-encoded %252e%252e");

    // (4) sanity: the legit skill still resolves through the same routes.
    let (st, _body) = raw_request(s.port, "GET", "/skills/file/normal-skill/SKILL.md");
    assert_eq!(st, 200, "legit skill file must still resolve");
}

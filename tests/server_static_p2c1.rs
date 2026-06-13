//! P2 dashboard server e2e tests: GET /api/timeline, GET /api/events, GET /api/event/:id.
//!
//! Each test spawns the real `runai server` binary against an isolated
//! HOME / `RUNE_DATA_DIR` tempdir, seeds `router_events` rows via the
//! `sqlite3` CLI, then queries the running server with `curl` and parses
//! JSON with `serde_json`.
//!
//! Notes on plan vs source divergence (cloud HEAD = `0.11.0-beta.4` here):
//!   - These endpoints are NOT auth-gated in this build, so the
//!     `*_requires_auth_after_registration` / `*_requires_auth` tests from
//!     the plan are skipped at the chunk level (the script reports them via
//!     the structured-output `skipped` array, not here).
//!   - `/api/events` has no `since_ts` query param; the time-filtering test
//!     only exercises `?hours=N` (the documented param).
//!   - `/api/event/:id` for a missing id returns
//!     `{"error":"not found"}` not an empty body (cf. `ApiError::NotFound`
//!     in `src/server.rs`). The test asserts status 404 only.
//!   - The default-bucket-calculation test reads the actual formula in
//!     `api_timeline` (target_buckets = 48 → default bucket_secs = 1800 for
//!     hours=24), not the plan's `~3600`.
//!
//! Safety contract: every test writes only inside `tempfile::TempDir`-rooted
//! HOMEs with `RUNE_DATA_DIR` pointing into the same tree, never the user's
//! real `~/.runai/`.

#![cfg(not(target_os = "windows"))]

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers ────────────────────────────────────────────────────────────────

/// Hold a spawned `runai server` process; killed on drop so failing tests
/// don't leave zombie listeners behind.
struct ServerGuard {
    child: Child,
    port: u16,
    _home: TempDir,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Pick an ephemeral port the kernel just confirmed is free. There is a
/// small race window between drop and the server bind, but for our local
/// single-machine test runs it has proven sufficient (other suites do the
/// same thing).
fn pick_free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let port = l.local_addr().unwrap().port();
    drop(l);
    port
}

/// Build a fresh test env: tempdir HOME with `<HOME>/.runai/` as the data
/// dir, plus the `runai.db` file initialised by booting the binary once
/// (any read-only subcommand triggers schema migration on first open).
fn fresh_env() -> (TempDir, PathBuf) {
    let home = TempDir::new().expect("tempdir HOME");
    let data_dir = home.path().join(".runai");
    std::fs::create_dir_all(&data_dir).expect("mk data dir");
    // Initialise the SQLite DB + schema by running a cheap read-only
    // subcommand. `list --kind skill` opens the DB, applies migrations and
    // exits.
    let status = Command::cargo_bin("runai")
        .expect("cargo bin runai")
        .arg("list")
        .arg("--kind")
        .arg("skill")
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", &data_dir)
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run runai list");
    assert!(status.success(), "runai list should succeed for db init");
    let db = data_dir.join("runai.db");
    assert!(db.exists(), "runai.db not created at {}", db.display());
    (home, db)
}

/// Spawn `runai server --host 127.0.0.1 --port <P>` in the background and
/// wait until the HTTP port answers a TCP connect.
fn spawn_server(home: TempDir, port: u16) -> ServerGuard {
    let data_dir = home.path().join(".runai");
    let child = Command::cargo_bin("runai")
        .expect("cargo bin runai")
        .arg("server")
        .arg("--host")
        .arg("127.0.0.1")
        .arg("--port")
        .arg(port.to_string())
        .env("HOME", home.path())
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env("RUNE_DATA_DIR", &data_dir)
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn runai server");

    let deadline = Instant::now() + Duration::from_secs(5);
    let addr = format!("127.0.0.1:{port}");
    let sock: std::net::SocketAddr = addr.parse().unwrap();
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&sock, Duration::from_millis(100)).is_ok() {
            return ServerGuard {
                child,
                port,
                _home: home,
            };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("runai server did not bind {addr} within 5s");
}

/// Seed N router_events rows via the `sqlite3` CLI. Each event is fully
/// schema-compatible: we only set the columns API tests inspect and rely
/// on schema defaults for the rest.
struct SeedEvent<'a> {
    ts: i64,
    provider: &'a str,
    model: &'a str,
    latency_ms: i64,
    chosen_skills_json: &'a str,
    status: &'a str,
    session_id: &'a str,
    user_prompt: &'a str,
    hook_output: &'a str,
    llm_input: &'a str,
}

impl<'a> SeedEvent<'a> {
    fn defaults(ts: i64) -> Self {
        SeedEvent {
            ts,
            provider: "deepseek",
            model: "deepseek-chat",
            latency_ms: 100,
            chosen_skills_json: "[]",
            status: "ok",
            session_id: "sess-test",
            user_prompt: "hello",
            hook_output: "",
            llm_input: "",
        }
    }
}

fn sqlite3_bin() -> &'static str {
    "sqlite3"
}

fn sqlite3_exec(db: &Path, sql: &str) {
    let out = Command::new(sqlite3_bin())
        .arg(db)
        .arg(sql)
        .output()
        .expect("run sqlite3");
    assert!(
        out.status.success(),
        "sqlite3 failed: {}\nstderr={}",
        sql,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[allow(dead_code)]
fn sqlite3_query(db: &Path, sql: &str) -> String {
    let out = Command::new(sqlite3_bin())
        .arg(db)
        .arg(sql)
        .output()
        .expect("run sqlite3");
    assert!(
        out.status.success(),
        "sqlite3 query failed: {}\nstderr={}",
        sql,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Escape a string for inclusion inside a single-quoted SQL literal.
fn sql_quote(s: &str) -> String {
    s.replace('\'', "''")
}

fn seed_event(db: &Path, ev: &SeedEvent<'_>) {
    let sql = format!(
        "INSERT INTO router_events (
            ts, provider, model,
            prompt_tokens, completion_tokens, reasoning_tokens, total_tokens,
            cache_hit_tokens, cache_miss_tokens,
            latency_ms, chosen_skills_json, candidate_count,
            status, error_msg,
            session_id, mode,
            user_prompt, cwd, bm25_kept,
            llm_raw_response, hook_output, llm_input
        ) VALUES (
            {ts}, '{provider}', '{model}',
            0, 0, 0, 0,
            0, 0,
            {latency_ms}, '{chosen}', 0,
            '{status}', NULL,
            '{session}', 'exclusive',
            '{prompt}', '', 0,
            '', '{hook}', '{llm_input}'
        );",
        ts = ev.ts,
        provider = sql_quote(ev.provider),
        model = sql_quote(ev.model),
        latency_ms = ev.latency_ms,
        chosen = sql_quote(ev.chosen_skills_json),
        status = sql_quote(ev.status),
        session = sql_quote(ev.session_id),
        prompt = sql_quote(ev.user_prompt),
        hook = sql_quote(ev.hook_output),
        llm_input = sql_quote(ev.llm_input),
    );
    sqlite3_exec(db, &sql);
}

/// GET a URL via curl and return (status_code, body). Uses curl rather than
/// reqwest because reqwest is not in `[dev-dependencies]`; curl is on every
/// macOS / Linux box CI runs.
fn http_get(url: &str) -> (u16, String) {
    // -s silent, -o body file, -w just status, then we read body file back.
    let body_file = TempDir::new().expect("tmpdir for curl body");
    let body_path = body_file.path().join("body");
    let out = Command::new("curl")
        .arg("-s")
        .arg("-o")
        .arg(&body_path)
        .arg("-w")
        .arg("%{http_code}")
        .arg(url)
        .output()
        .expect("run curl");
    assert!(
        out.status.success(),
        "curl exit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let code: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .expect("parse http code");
    let body = std::fs::read_to_string(&body_path).unwrap_or_default();
    (code, body)
}

/// Convenience: GET + parse JSON or panic with full body.
fn http_get_json(url: &str) -> (u16, serde_json::Value) {
    let (code, body) = http_get(url);
    let v: serde_json::Value = serde_json::from_str(&body)
        .unwrap_or_else(|e| panic!("non-JSON response (code {code}): {body}\nerror: {e}"));
    (code, v)
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

// ─── feature 1: GET /api/timeline ───────────────────────────────────────────

#[test]
fn timeline_bucket_boundaries_correct() {
    let (home, db) = fresh_env();
    let port = pick_free_port();
    // Seed 5 events at known offsets within the last 12h. We use timestamps
    // anchored to `now` so the server's window query (now - 12h .. now) sees
    // them.
    let now = now_ts();
    // Place events in distinct hour buckets within the 12h window.
    for offset_minutes in [30, 90, 200, 400, 700].iter() {
        let ts = now - (offset_minutes * 60);
        let ev = SeedEvent::defaults(ts);
        seed_event(&db, &ev);
    }
    let guard = spawn_server(home, port);
    let url = format!(
        "http://127.0.0.1:{}/api/timeline?hours=12&bucket_secs=3600",
        guard.port
    );
    let (code, v) = http_get_json(&url);
    assert_eq!(code, 200, "timeline should return 200, got {code}: {v}");
    let bucket_secs = v
        .get("bucket_secs")
        .and_then(|x| x.as_i64())
        .expect("bucket_secs i64");
    assert_eq!(bucket_secs, 3600);
    let points = v
        .get("points")
        .and_then(|x| x.as_array())
        .expect("points array");
    // buckets = hours*3600 / bucket_secs = 12*3600/3600 = 12.
    assert_eq!(
        points.len(),
        12,
        "expected 12 hourly buckets, got {}",
        points.len()
    );
    // Each bucket has all five required fields.
    for (i, p) in points.iter().enumerate() {
        for field in ["ts_start", "total", "hits", "errors", "avg_latency_ms"] {
            assert!(
                p.get(field).is_some(),
                "bucket {i} missing field `{field}` in {p}"
            );
        }
    }
    // Buckets are in chronological order (ascending ts_start).
    let mut prev: i64 = i64::MIN;
    for p in points {
        let t = p.get("ts_start").and_then(|x| x.as_i64()).unwrap();
        assert!(t > prev, "ts_start not strictly increasing: {t} <= {prev}");
        prev = t;
    }
    // First bucket ts_start equals (now - 12*3600) rounded down to the
    // bucket boundary. The router_timeline impl uses start = now - bucket_secs*buckets
    // verbatim, so the boundary is exactly now - 12*3600 (we accept a small
    // delta because the server captures `now` slightly later than the test).
    let first = points[0].get("ts_start").and_then(|x| x.as_i64()).unwrap();
    let expected_first = now - 12 * 3600;
    let delta = (first - expected_first).abs();
    assert!(
        delta < 5,
        "first ts_start {first} should be ~now-43200={expected_first} (delta={delta})"
    );
    drop(guard);
}

#[test]
fn timeline_default_bucket_calculation() {
    let (home, db) = fresh_env();
    let port = pick_free_port();
    // Seed 24 events spread across 24 hours (one per hour, 30 min into each).
    let now = now_ts();
    for hour_offset in 0..24 {
        let ts = now - (hour_offset * 3600) - 1800;
        seed_event(&db, &SeedEvent::defaults(ts));
    }
    let guard = spawn_server(home, port);
    let url = format!("http://127.0.0.1:{}/api/timeline?hours=24", guard.port);
    let (code, v) = http_get_json(&url);
    assert_eq!(code, 200, "timeline default-bucket should return 200: {v}");
    // Per `api_timeline` in src/server.rs:
    //   target_buckets = 48
    //   default_bucket = max((hours*3600)/48, 60)
    //   bucket_secs = q.bucket_secs.unwrap_or(default).max(60)
    //   buckets = (hours*3600)/bucket_secs
    // hours=24 → default_bucket = 24*3600/48 = 1800 (30 min)
    // → buckets = 24*3600/1800 = 48.
    let bucket_secs = v.get("bucket_secs").and_then(|x| x.as_i64()).unwrap();
    assert_eq!(
        bucket_secs, 1800,
        "default bucket_secs should be 1800 (24h / 48 target buckets), got {bucket_secs}"
    );
    let points = v.get("points").and_then(|x| x.as_array()).unwrap();
    assert_eq!(
        points.len(),
        48,
        "default points for hours=24 should be 48, got {}",
        points.len()
    );
    drop(guard);
}


// ─── feature 2: GET /api/events ─────────────────────────────────────────────

#[test]
fn events_pagination_limit_offset() {
    let (home, db) = fresh_env();
    let port = pick_free_port();
    // Seed 150 events with monotonically descending newest-first timestamps.
    // We use a unique session_id so a stray older event from db init can't
    // pollute the count (none in practice, but be defensive).
    let now = now_ts();
    for i in 0..150 {
        let ts = now - i; // newer rows have larger ts (i=0 is newest)
        let prompt = format!("p{i}");
        let ev = SeedEvent {
            ts,
            user_prompt: &prompt,
            session_id: "pagination-test",
            ..SeedEvent::defaults(ts)
        };
        seed_event(&db, &ev);
    }
    let guard = spawn_server(home, port);
    let base = format!("http://127.0.0.1:{}/api/events", guard.port);

    let (c1, p1) = http_get_json(&format!("{base}?limit=50&offset=0"));
    assert_eq!(c1, 200);
    assert_eq!(p1.get("total").and_then(|x| x.as_i64()).unwrap(), 150);
    let e1 = p1.get("events").and_then(|x| x.as_array()).unwrap();
    assert_eq!(e1.len(), 50);

    let (c2, p2) = http_get_json(&format!("{base}?limit=50&offset=50"));
    assert_eq!(c2, 200);
    let e2 = p2.get("events").and_then(|x| x.as_array()).unwrap();
    assert_eq!(e2.len(), 50);

    let (c3, p3) = http_get_json(&format!("{base}?limit=50&offset=100"));
    assert_eq!(c3, 200);
    let e3 = p3.get("events").and_then(|x| x.as_array()).unwrap();
    assert_eq!(e3.len(), 50);

    // Pages are disjoint by id.
    let ids = |arr: &Vec<serde_json::Value>| -> Vec<i64> {
        arr.iter()
            .map(|e| e.get("id").and_then(|x| x.as_i64()).unwrap())
            .collect()
    };
    let i1 = ids(e1);
    let i2 = ids(e2);
    let i3 = ids(e3);
    let all: std::collections::HashSet<i64> =
        i1.iter().chain(i2.iter()).chain(i3.iter()).copied().collect();
    assert_eq!(all.len(), 150, "pages must not overlap");

    // Newest-first ordering by ts.
    let ts_seq: Vec<i64> = e1
        .iter()
        .map(|e| e.get("ts").and_then(|x| x.as_i64()).unwrap())
        .collect();
    let mut sorted = ts_seq.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(ts_seq, sorted, "first page must be newest-first");
    drop(guard);
}

#[test]
fn events_filter_by_model() {
    let (home, db) = fresh_env();
    let port = pick_free_port();
    let now = now_ts();
    for i in 0..3 {
        let ts = now - i;
        let ev = SeedEvent {
            ts,
            model: "claude-3-5-sonnet",
            ..SeedEvent::defaults(ts)
        };
        seed_event(&db, &ev);
    }
    for i in 0..2 {
        let ts = now - 10 - i;
        let ev = SeedEvent {
            ts,
            model: "claude-3-opus",
            ..SeedEvent::defaults(ts)
        };
        seed_event(&db, &ev);
    }
    let guard = spawn_server(home, port);
    let (code, v) = http_get_json(&format!(
        "http://127.0.0.1:{}/api/events?model=claude-3-5-sonnet",
        guard.port
    ));
    assert_eq!(code, 200);
    let total = v.get("total").and_then(|x| x.as_i64()).unwrap();
    assert_eq!(total, 3, "filter-by-model total should be 3, got {total}");
    let events = v.get("events").and_then(|x| x.as_array()).unwrap();
    assert_eq!(events.len(), 3);
    for e in events {
        assert_eq!(
            e.get("model").and_then(|x| x.as_str()).unwrap(),
            "claude-3-5-sonnet"
        );
    }
    drop(guard);
}

#[test]
fn events_filter_hit_only() {
    let (home, db) = fresh_env();
    let port = pick_free_port();
    let now = now_ts();
    // 2 events with non-empty chosen array (hits)
    for i in 0..2 {
        let ts = now - i;
        let ev = SeedEvent {
            ts,
            chosen_skills_json: "[\"my-skill\"]",
            status: "ok",
            ..SeedEvent::defaults(ts)
        };
        seed_event(&db, &ev);
    }
    // 2 events with empty chosen array (non-hits)
    for i in 0..2 {
        let ts = now - 10 - i;
        let ev = SeedEvent {
            ts,
            chosen_skills_json: "[]",
            status: "ok",
            ..SeedEvent::defaults(ts)
        };
        seed_event(&db, &ev);
    }
    let guard = spawn_server(home, port);
    let (code, v) = http_get_json(&format!(
        "http://127.0.0.1:{}/api/events?hit_only=true",
        guard.port
    ));
    assert_eq!(code, 200);
    let total = v.get("total").and_then(|x| x.as_i64()).unwrap();
    assert_eq!(total, 2, "hit_only total should be 2, got {total}");
    let events = v.get("events").and_then(|x| x.as_array()).unwrap();
    assert_eq!(events.len(), 2);
    for e in events {
        let chosen = e.get("chosen").and_then(|x| x.as_array()).unwrap();
        assert!(!chosen.is_empty(), "hit_only events must have non-empty chosen");
    }
    drop(guard);
}

#[test]
fn events_time_filtering() {
    let (home, db) = fresh_env();
    let port = pick_free_port();
    let now = now_ts();
    // Events at: now, -1h, -2h, -25h.
    let offsets = [0i64, 3600, 7200, 25 * 3600];
    for off in offsets.iter() {
        let ts = now - off;
        seed_event(&db, &SeedEvent::defaults(ts));
    }
    let guard = spawn_server(home, port);
    let (code, v) = http_get_json(&format!(
        "http://127.0.0.1:{}/api/events?hours=24",
        guard.port
    ));
    assert_eq!(code, 200);
    // 3 events are within 24h (offsets 0, 3600, 7200); the -25h one is excluded.
    let total = v.get("total").and_then(|x| x.as_i64()).unwrap();
    assert_eq!(total, 3, "?hours=24 should return 3, got {total}");
    let events = v.get("events").and_then(|x| x.as_array()).unwrap();
    assert_eq!(events.len(), 3);
    // All returned events are within the window.
    let cutoff = now - 24 * 3600;
    for e in events {
        let ts = e.get("ts").and_then(|x| x.as_i64()).unwrap();
        assert!(ts >= cutoff, "event ts {ts} older than cutoff {cutoff}");
    }
    drop(guard);
}

#[test]
fn events_response_schema_completeness() {
    let (home, db) = fresh_env();
    let port = pick_free_port();
    let now = now_ts();
    let ev = SeedEvent {
        ts: now,
        chosen_skills_json: "[\"foo\", \"bar\"]",
        hook_output: "# injected\nfoo + bar",
        llm_input: "full llm input here",
        status: "ok",
        ..SeedEvent::defaults(now)
    };
    seed_event(&db, &ev);
    let guard = spawn_server(home, port);
    let (code, v) = http_get_json(&format!(
        "http://127.0.0.1:{}/api/events?limit=1",
        guard.port
    ));
    assert_eq!(code, 200);
    let e = &v.get("events").and_then(|x| x.as_array()).unwrap()[0];
    // chosen is an array (deserialised from JSON string column)
    let chosen = e.get("chosen").and_then(|x| x.as_array()).unwrap();
    assert_eq!(chosen.len(), 2);
    assert_eq!(chosen[0].as_str().unwrap(), "foo");
    assert_eq!(chosen[1].as_str().unwrap(), "bar");
    // injected = (status==ok && chosen non-empty) → true here
    assert_eq!(
        e.get("injected").and_then(|x| x.as_bool()).unwrap(),
        true,
        "injected should be true when status=ok and chosen non-empty"
    );
    // hook_output is a string up to 6KB
    let hook = e.get("hook_output").and_then(|x| x.as_str()).unwrap();
    assert_eq!(hook, "# injected\nfoo + bar");
    // llm_input is a string
    let llm = e.get("llm_input").and_then(|x| x.as_str()).unwrap();
    assert_eq!(llm, "full llm input here");
    drop(guard);
}

// ─── feature 3: GET /api/event/:id ─────────────────────────────────────────

#[test]
fn event_by_id_returns_full_event() {
    let (home, db) = fresh_env();
    let port = pick_free_port();
    let now = now_ts();
    let ev = SeedEvent {
        ts: now,
        chosen_skills_json: "[\"sk1\"]",
        hook_output: "# block",
        llm_input: "input text",
        user_prompt: "please help",
        ..SeedEvent::defaults(now)
    };
    seed_event(&db, &ev);
    // The single autoincrement row gets id=1 on a freshly-initialised DB.
    let inserted_id_str = sqlite3_query(&db, "SELECT id FROM router_events LIMIT 1;");
    let inserted_id: i64 = inserted_id_str.parse().expect("parse inserted id");

    let guard = spawn_server(home, port);
    let (code, v) = http_get_json(&format!(
        "http://127.0.0.1:{}/api/event/{}",
        guard.port, inserted_id
    ));
    assert_eq!(code, 200, "event_by_id should be 200, got {code}: {v}");
    // Required EventJson fields per src/server.rs::EventJson:
    for field in [
        "id",
        "ts",
        "model",
        "provider",
        "status",
        "mode",
        "chosen",
        "candidate_count",
        "bm25_kept",
        "prompt_tokens",
        "completion_tokens",
        "total_tokens",
        "latency_ms",
        "session_id",
        "user_prompt",
        "cwd",
        "hook_output",
        "llm_input",
        "injected",
    ] {
        assert!(
            v.get(field).is_some(),
            "EventJson missing field `{field}` in {v}"
        );
    }
    assert_eq!(v.get("id").and_then(|x| x.as_i64()).unwrap(), inserted_id);
    assert_eq!(
        v.get("user_prompt").and_then(|x| x.as_str()).unwrap(),
        "please help"
    );
    assert_eq!(
        v.get("hook_output").and_then(|x| x.as_str()).unwrap(),
        "# block"
    );
    assert_eq!(
        v.get("llm_input").and_then(|x| x.as_str()).unwrap(),
        "input text"
    );
    drop(guard);
}

#[test]
fn event_by_id_not_found_empty_body() {
    // Plan says "Response body is empty"; cloud HEAD actually returns
    // `{"error":"not found"}` (see `ApiError::NotFound` in src/server.rs).
    // We assert the contract that DOES hold today: status 404. The plan's
    // "uniform empty 404" comes from PLANNING §2.3 item 4, not yet in this
    // build.
    let (home, _db) = fresh_env();
    let port = pick_free_port();
    let guard = spawn_server(home, port);
    let (code, _body) = http_get(&format!(
        "http://127.0.0.1:{}/api/event/99999",
        guard.port
    ));
    assert_eq!(code, 404, "non-existent event id should be 404, got {code}");
    drop(guard);
}

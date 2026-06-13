//! P2 e2e regression for dashboard tabs (plan §4.43):
//!   - Overview tab (/api/summary, /api/timeline)
//!   - Activity tab (/api/events with hours/limit/offset/model/hit_only)
//!
//! The plan §4.43 specifies six "browser_playwright" tests for the dashboard
//! tabs. Four of those (library mutate / market browse+install / admin user
//! CRUD / provider CRUD) reach endpoints (`POST /api/skills/library/mutate`,
//! `/api/market`, `/api/market/refresh`, `/api/admin/users`, provider config
//! routes) that do **not** exist in the cloud HEAD this worktree is built
//! against — `grep -n "library/mutate\|admin/users\|/api/market\|/api/provider"
//! src/server.rs` returns 0 hits. Those four are reported as `src_missing`
//! in the StructuredOutput.
//!
//! The two that DO have backing endpoints (overview + activity) are covered
//! here as cargo-level integration tests that:
//!  1. spawn the real `runai server` binary in an isolated HOME (tempdir)
//!     with `RUNE_DATA_DIR` pointing inside that home, so AGENTS.md
//!     safety contract holds (we never touch `~/.runai/`)
//!  2. open the same DB file via the lib API to seed `router_events`
//!  3. make raw HTTP/1.1 requests over a TCP socket (no `reqwest` in
//!     dev-deps) and assert on the JSON shape the dashboard tabs consume
//!
//! Skipped on Windows: HOME env var mocking is unix-only (AGENTS.md key
//! constraints) and the existing `safety_e2e.rs` is gated identically.

#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── binary + port helpers ──────────────────────────────────────────────────

/// Resolve the runai binary the test will spawn. Prefer the cargo-built
/// worktree binary (via `assert_cmd`) so the binary's DB schema matches the
/// lib this test links against; fall back to the installed path.
fn bin_path() -> PathBuf {
    Command::cargo_bin("runai")
        .ok()
        .and_then(|c| c.get_program().to_os_string().into_string().ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Users/crosery/.cargo/bin/runai"))
}

fn ensure_bin() {
    let p = bin_path();
    assert!(
        Path::new(&p).exists(),
        "runai binary expected at {} (cargo build it first)",
        p.display()
    );
}

/// Pick an unused ephemeral port; bind+release to find one, then hand the
/// number to the spawned server. Keeps parallel test runs from colliding.
fn pick_port() -> u16 {
    static OFFSET: AtomicU16 = AtomicU16::new(0);
    let _bump = OFFSET.fetch_add(1, Ordering::SeqCst);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    let p = listener.local_addr().unwrap().port();
    drop(listener);
    p
}

// ─── server handle (spawn + HTTP/1.1 GET) ───────────────────────────────────

struct ServerHandle {
    child: Child,
    port: u16,
    home: TempDir,
}

impl ServerHandle {
    fn start() -> Self {
        let home = tempfile::tempdir().expect("tmp HOME");
        std::fs::create_dir_all(home.path().join(".runai")).unwrap();
        let port = pick_port();
        let child = Command::new(bin_path())
            .args(["server", "--port", &port.to_string(), "--host", "127.0.0.1"])
            .env("HOME", home.path())
            .env("RUNE_DATA_DIR", home.path().join(".runai"))
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn runai server");
        let handle = ServerHandle { child, port, home };
        handle.wait_ready();
        handle
    }

    fn wait_ready(&self) {
        let addr = format!("127.0.0.1:{}", self.port);
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(stream) =
                TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(200))
            {
                drop(stream);
                // axum may accept TCP before routes are mounted in race-y
                // conditions; confirm a 200 from a known static endpoint.
                if let Ok((status, _, _)) = self.get("/app.css") {
                    if status == 200 {
                        return;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("runai server on 127.0.0.1:{} never became ready", self.port);
    }

    fn get(&self, path: &str) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
        let mut stream = TcpStream::connect(format!("127.0.0.1:{}", self.port))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nAccept: */*\r\n\r\n",
            path, self.port
        );
        stream.write_all(req.as_bytes())?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf)?;
        Ok(parse_response(&buf))
    }

    fn db_path(&self) -> PathBuf {
        self.home.path().join(".runai").join("runai.db")
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_response(raw: &[u8]) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response has header/body split");
    let head = std::str::from_utf8(&raw[..split]).expect("headers are utf-8");
    let body = raw[split + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().expect("status line");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .expect("status code field")
        .parse()
        .expect("status code is u16");
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| {
            l.find(':')
                .map(|i| (l[..i].to_ascii_lowercase(), l[i + 1..].trim().to_string()))
        })
        .collect();
    (status, headers, body)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

// ─── test data seeding ──────────────────────────────────────────────────────

/// Wait for the DB file to materialize after server boot (axum runs the
/// migrations on first request), then return an open lib-level handle so we
/// can seed `router_events` directly.
fn open_seeded_db(server: &ServerHandle) -> runai::core::db::Database {
    let path = server.db_path();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        path.exists(),
        "expected DB file at {} after server boot",
        path.display()
    );
    // Also touch /api/summary so server-side migrations have finished.
    let _ = server.get("/api/summary");
    runai::core::db::Database::open(&path).expect("open db via lib")
}

fn ev(
    id_seed: usize,
    ts: i64,
    model: &str,
    chosen: &[&str],
    status: &str,
    latency_ms: i64,
    prompt_tokens: i64,
) -> runai::core::db::RouterEvent {
    let chosen_json = serde_json::to_string(chosen).unwrap();
    runai::core::db::RouterEvent {
        id: None,
        ts,
        provider: "test".to_string(),
        model: model.to_string(),
        prompt_tokens,
        completion_tokens: 5,
        reasoning_tokens: 0,
        total_tokens: prompt_tokens + 5,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        latency_ms,
        chosen_skills_json: chosen_json,
        candidate_count: chosen.len() as i64,
        status: status.to_string(),
        error_msg: None,
        session_id: format!("sess-{id_seed}"),
        mode: "exclusive".to_string(),
        user_prompt: format!("prompt-{id_seed}"),
        cwd: "/tmp".to_string(),
        bm25_kept: chosen.len() as i64,
        llm_raw_response: String::new(),
        hook_output: String::new(),
        llm_input: String::new(),
    }
}

// ─────────────────────────────────────────────────────────────────────
// §4.43 (1) dashboard_overview_tab_renders
//   Plan asserts hero numbers (total, hit rate, latency, tokens) come from
//   /api/summary; the Chart placeholder is populated from /api/timeline.
//   No browser available here, so we assert the JSON contract that the
//   tab consumes.
// ─────────────────────────────────────────────────────────────────────

#[test]
fn overview_summary_endpoint_provides_hero_numbers() {
    ensure_bin();
    let server = ServerHandle::start();

    // Seed three events: 2 hits, 1 error. Shift slightly into the past so
    // the events fall strictly inside any time-of-call window the server
    // computes for filtering.
    let now = chrono::Utc::now().timestamp();
    {
        let db = open_seeded_db(&server);
        let events = [
            ev(
                0,
                now - 60,
                "deepseek-v4-flash",
                &["skill-a"],
                "ok",
                100,
                20,
            ),
            ev(
                1,
                now - 3600,
                "deepseek-v4-flash",
                &["skill-b"],
                "ok",
                200,
                30,
            ),
            ev(2, now - 7200, "deepseek-v4-flash", &[], "error", 50, 10),
        ];
        for e in &events {
            db.insert_router_event(e).expect("insert event");
        }
    }

    let (status, headers, body) = server
        .get("/api/summary?hours=24")
        .expect("GET /api/summary");
    assert_eq!(status, 200, "summary status");
    let ct = header(&headers, "content-type").expect("content-type");
    assert!(
        ct.starts_with("application/json"),
        "content-type starts with application/json, got {ct}"
    );
    let v: serde_json::Value = serde_json::from_slice(&body).expect("summary body is JSON");

    // The hero strip in the overview tab reads these four numbers.
    assert_eq!(v["total"].as_i64(), Some(3), "hero #hero-total = total");
    assert_eq!(v["hits"].as_i64(), Some(2), "hits count for hit_rate");
    assert_eq!(v["errors"].as_i64(), Some(1), "errors count");
    let hr = v["hit_rate"].as_f64().expect("hit_rate numeric");
    assert!(
        (hr - (2.0 / 3.0)).abs() < 1e-9,
        "hero #hero-strip-hit = hits / total, expected 2/3, got {hr}"
    );
    let lat = v["avg_latency_ms"]
        .as_f64()
        .expect("avg_latency_ms numeric");
    // router_stats_summary computes avg_latency_ms over status='ok' rows ONLY
    // (see db::router_stats_summary). The error row is excluded.
    // (100 + 200) / 2 = 150.
    assert!(
        (lat - 150.0).abs() < 1e-9,
        "hero #hero-strip-lat = avg latency ms over ok rows, expected 150, got {lat}"
    );
    // Total tokens column populates #hero-strip-tok.
    let tt = v["total_tokens"].as_i64().expect("total_tokens i64");
    assert_eq!(
        tt,
        (20 + 5) + (30 + 5) + (10 + 5),
        "hero #hero-strip-tok = sum of total_tokens"
    );

    // The overview tab's per-model breakdown sources from `per_model`.
    let pm = v["per_model"].as_array().expect("per_model array");
    assert_eq!(pm.len(), 1, "single test model");
    assert_eq!(pm[0]["model"].as_str(), Some("deepseek-v4-flash"));
    assert_eq!(pm[0]["calls"].as_i64(), Some(3));
}

#[test]
fn overview_timeline_endpoint_populates_chart() {
    ensure_bin();
    let server = ServerHandle::start();

    // Seed two events strictly in the past (router_timeline filters `ts < now`
    // where `now` is sampled at server-call time; events at exactly `now`
    // get excluded by the race between seed and call).
    let now = chrono::Utc::now().timestamp();
    {
        let db = open_seeded_db(&server);
        db.insert_router_event(&ev(0, now - 60, "m", &["a"], "ok", 100, 20))
            .unwrap();
        db.insert_router_event(&ev(1, now - 600, "m", &[], "ok", 150, 30))
            .unwrap();
    }

    let (status, headers, body) = server.get("/api/timeline?hours=24").expect("GET timeline");
    assert_eq!(status, 200, "timeline status");
    let ct = header(&headers, "content-type").expect("content-type");
    assert!(
        ct.starts_with("application/json"),
        "content-type starts with application/json, got {ct}"
    );
    let v: serde_json::Value = serde_json::from_slice(&body).expect("timeline JSON");

    // Chart needs `bucket_secs` (axis label) + `points` (chart data).
    let bucket = v["bucket_secs"].as_i64().expect("bucket_secs i64");
    assert!(bucket >= 60, "bucket_secs floor 60s, got {bucket}");
    let points = v["points"].as_array().expect("points array");
    assert!(!points.is_empty(), "points non-empty; chart needs data");

    // Sum across all buckets must include the two seeded events.
    let mut total_in_buckets: i64 = 0;
    for p in points {
        total_in_buckets += p["total"].as_i64().expect("point.total i64");
        // every TimelinePoint emits ts_start + total + hits + errors + avg_latency_ms
        assert!(p["ts_start"].is_i64(), "point.ts_start present");
        assert!(p["hits"].is_i64(), "point.hits present");
        assert!(p["errors"].is_i64(), "point.errors present");
        assert!(
            p["avg_latency_ms"].is_number(),
            "point.avg_latency_ms numeric"
        );
    }
    assert!(
        total_in_buckets >= 2,
        "sum across buckets covers seeded rows, got {total_in_buckets}"
    );

    // 6h window yields a smaller default bucket than 24h does — the chart
    // gets finer granularity for shorter windows.
    let (st6, _, body6) = server
        .get("/api/timeline?hours=6")
        .expect("GET timeline 6h");
    assert_eq!(st6, 200);
    let v6: serde_json::Value = serde_json::from_slice(&body6).unwrap();
    let bucket6 = v6["bucket_secs"].as_i64().unwrap();
    assert!(
        bucket6 <= bucket,
        "6h timeline bucket ({bucket6}) <= 24h bucket ({bucket})"
    );
}

// ─────────────────────────────────────────────────────────────────────
// §4.43 (2) dashboard_activity_tab_works
//   Plan asserts: events table populated, hour dropdown changes data,
//   pagination works, click row → detail modal.
//   Backed by /api/events (paged, hours/limit/offset/model/hit_only) and
//   /api/event/{id} (detail).
// ─────────────────────────────────────────────────────────────────────

#[test]
fn activity_events_endpoint_paginates_and_filters() {
    ensure_bin();
    let server = ServerHandle::start();

    let now = chrono::Utc::now().timestamp();
    let two_hrs_ago = now - 2 * 3600;
    let three_days_ago = now - 3 * 86400;
    {
        let db = open_seeded_db(&server);
        // 3 recent (now) hit events with model "fast"
        for i in 0..3 {
            db.insert_router_event(&ev(
                i,
                now - i as i64 * 10,
                "fast",
                &["skill-a"],
                "ok",
                100,
                20,
            ))
            .unwrap();
        }
        // 2 older (two_hrs_ago) miss events with model "slow"
        for i in 3..5 {
            db.insert_router_event(&ev(
                i,
                two_hrs_ago - (i - 3) as i64 * 10,
                "slow",
                &[],
                "ok",
                500,
                40,
            ))
            .unwrap();
        }
        // 1 very old event outside 24h window
        db.insert_router_event(&ev(99, three_days_ago, "fast", &["x"], "ok", 100, 20))
            .unwrap();
    }

    // 1) Table populated: GET /api/events returns total + events array.
    let (status, headers, body) = server.get("/api/events").expect("GET /api/events");
    assert_eq!(status, 200, "events status");
    let ct = header(&headers, "content-type").unwrap();
    assert!(ct.starts_with("application/json"));
    let v: serde_json::Value = serde_json::from_slice(&body).expect("events JSON");
    let total = v["total"].as_i64().expect("total");
    assert_eq!(total, 6, "all 6 rows visible without hour filter");
    let rows = v["events"].as_array().expect("events array");
    assert_eq!(rows.len(), 6, "rows returned");
    // Default sort is newest-first (router_events_paged ORDER BY ts DESC).
    let ts0 = rows[0]["ts"].as_i64().unwrap();
    let ts5 = rows[5]["ts"].as_i64().unwrap();
    assert!(ts0 >= ts5, "rows ordered newest-first: {ts0} >= {ts5}");

    // 2) Hour dropdown filter: ?hours=1 strips events older than now-1h.
    let (st1, _, body1) = server.get("/api/events?hours=1").expect("GET hours=1");
    assert_eq!(st1, 200);
    let v1: serde_json::Value = serde_json::from_slice(&body1).unwrap();
    assert_eq!(
        v1["total"].as_i64(),
        Some(3),
        "hours=1 keeps only the 3 events from `now`"
    );

    // 3) Pagination: limit=2 + offset=0 returns first 2, offset=2 next 2.
    let (_, _, body_p0) = server.get("/api/events?limit=2&offset=0").unwrap();
    let (_, _, body_p1) = server.get("/api/events?limit=2&offset=2").unwrap();
    let p0: serde_json::Value = serde_json::from_slice(&body_p0).unwrap();
    let p1: serde_json::Value = serde_json::from_slice(&body_p1).unwrap();
    assert_eq!(p0["events"].as_array().unwrap().len(), 2, "page 0 size");
    assert_eq!(p1["events"].as_array().unwrap().len(), 2, "page 1 size");
    assert_eq!(p0["total"].as_i64(), p1["total"].as_i64(), "total stable");
    let s0_ids: Vec<i64> = p0["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap_or(-1))
        .collect();
    let s1_ids: Vec<i64> = p1["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_i64().unwrap_or(-1))
        .collect();
    assert!(
        s0_ids.iter().all(|id| !s1_ids.contains(id)),
        "page0 and page1 ids disjoint: {s0_ids:?} ∩ {s1_ids:?}"
    );

    // 4) Model filter narrows to one model.
    let (_, _, body_m) = server.get("/api/events?model=slow").unwrap();
    let vm: serde_json::Value = serde_json::from_slice(&body_m).unwrap();
    assert_eq!(vm["total"].as_i64(), Some(2), "model=slow has 2 rows");
    for row in vm["events"].as_array().unwrap() {
        assert_eq!(row["model"].as_str(), Some("slow"));
    }

    // 5) hit_only=true filter drops empty-chosen rows.
    let (_, _, body_h) = server.get("/api/events?hit_only=true").unwrap();
    let vh: serde_json::Value = serde_json::from_slice(&body_h).unwrap();
    // hits = 3 recent fast hits + 1 very old fast hit = 4
    assert_eq!(vh["total"].as_i64(), Some(4), "hit_only=true keeps hits");
    for row in vh["events"].as_array().unwrap() {
        let chosen = row["chosen"].as_array().unwrap();
        assert!(!chosen.is_empty(), "hit_only row has non-empty chosen");
        assert_eq!(
            row["injected"].as_bool(),
            Some(true),
            "hit + ok status → injected=true"
        );
    }
}

#[test]
fn activity_event_by_id_opens_detail_modal() {
    ensure_bin();
    let server = ServerHandle::start();

    let now = chrono::Utc::now().timestamp();
    {
        let db = open_seeded_db(&server);
        db.insert_router_event(&ev(0, now, "fast", &["skill-a", "skill-b"], "ok", 100, 25))
            .unwrap();
    }

    // 1) List endpoint hands us the id the dashboard will pass to detail.
    let (st, _, body) = server.get("/api/events?limit=1").unwrap();
    assert_eq!(st, 200);
    let listing: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let row = &listing["events"][0];
    let id = row["id"].as_i64().expect("row.id");
    assert_eq!(row["model"].as_str(), Some("fast"));

    // 2) Detail endpoint serves the same row.
    let (st_d, _, body_d) = server.get(&format!("/api/event/{id}")).unwrap();
    assert_eq!(st_d, 200, "/api/event/<id> status");
    let detail: serde_json::Value = serde_json::from_slice(&body_d).unwrap();
    assert_eq!(detail["id"].as_i64(), Some(id), "id round-trips");
    assert_eq!(detail["model"].as_str(), Some("fast"));
    let chosen = detail["chosen"].as_array().expect("chosen array");
    assert_eq!(chosen.len(), 2, "chosen has both skills");
    assert_eq!(chosen[0].as_str(), Some("skill-a"));
    assert_eq!(chosen[1].as_str(), Some("skill-b"));
    // The detail modal displays the prompt + cwd + llm_input fields too.
    assert_eq!(detail["user_prompt"].as_str(), Some("prompt-0"));
    assert_eq!(detail["cwd"].as_str(), Some("/tmp"));
    assert!(
        detail["injected"].as_bool().unwrap(),
        "ok + non-empty chosen → injected"
    );

    // 3) Unknown id returns 404 (dashboard surfaces "not found").
    let (st_404, _, _) = server.get("/api/event/9999999").unwrap();
    assert_eq!(
        st_404, 404,
        "unknown event id is 404 so the modal can show not-found"
    );
}

//! Real-coverage e2e for `api_timeline` (src/server.rs::api_timeline).
//!
//! Spawns the real `runai server` binary in an isolated HOME tempdir,
//! seeds `router_events` directly via rusqlite, then issues HTTP GETs
//! against `/api/timeline` and asserts the response JSON shape matches
//! the documented contract:
//!
//! ```text
//! { "bucket_secs": i64, "points": [{ ts_start, total, hits, errors, avg_latency_ms }] }
//! ```
//!
//! Scenarios covered:
//! - `api_timeline_aggregation_by_model`: rows from multiple models all
//!   counted in totals (no per-model filter in this endpoint).
//! - `api_timeline_hourly_binning`: 24-hour window with default bucket
//!   produces 48 buckets (target_buckets in src) of 1800s each.
//! - `api_timeline_empty_response`: empty DB returns all-zero points but
//!   correct bucket count + width.
//! - `api_timeline_timezone_handling`: ts_start values are in UNIX epoch
//!   seconds (UTC), not local time; consecutive bucket deltas equal
//!   `bucket_secs` regardless of host TZ.
#![cfg(not(target_os = "windows"))]

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use rusqlite::{Connection, params};
use serde_json::Value;
use tempfile::TempDir;

// ─── Helpers ────────────────────────────────────────────────────────────────

fn runai() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

/// Bind+drop a TCP port to ask the kernel for a free one. Returns the port.
/// There IS a TOCTOU race between drop and runai binding, but for serial
/// `--test-threads=1` tests under CI this is the well-known idiom and
/// hard-coding ports caused the P1/P2 conflicts the task brief warned about.
fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

struct Env {
    home: TempDir,
}

impl Env {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
        let env = Self { home };
        // Bootstrap the DB by running a cheap subcommand. `runai list` opens
        // the DB which runs init_schema(), creating the router_events table.
        let out = env.exec(&["list"]);
        assert!(
            out.status.success(),
            "list must bootstrap DB schema (stderr={})",
            String::from_utf8_lossy(&out.stderr)
        );
        env
    }

    fn home(&self) -> &Path {
        self.home.path()
    }

    fn data_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }

    fn db_path(&self) -> PathBuf {
        self.data_dir().join("runai.db")
    }

    fn exec(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = runai();
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNE_DATA_DIR", self.data_dir())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID");
        cmd.output().expect("runai binary spawn")
    }

    /// Insert one router_event row at a specific (ts, model, status, hit?, latency_ms).
    /// `hit` controls whether chosen_skills_json is `[]` (miss) or `["x"]` (hit).
    fn insert_event(&self, ts: i64, model: &str, status: &str, hit: bool, latency_ms: i64) {
        let conn = Connection::open(self.db_path()).expect("open seed db");
        let chosen = if hit { "[\"x\"]" } else { "[]" };
        conn.execute(
            "INSERT INTO router_events (
                ts, provider, model,
                prompt_tokens, completion_tokens, reasoning_tokens, total_tokens,
                cache_hit_tokens, cache_miss_tokens,
                latency_ms, chosen_skills_json, candidate_count, status, error_msg,
                session_id, mode, user_prompt, cwd, bm25_kept,
                llm_raw_response, hook_output, llm_input
             ) VALUES (?1, 'p', ?2, 0,0,0,0, 0,0,
                       ?3, ?4, 0, ?5, NULL,
                       '', 'exclusive', '', '', 0,
                       '', '', '')",
            params![ts, model, latency_ms, chosen, status],
        )
        .expect("insert router_event");
    }
}

/// RAII guard around a spawned `runai server` child process. Kills the
/// child on drop so a panicking assertion doesn't orphan the daemon.
struct ServerProc {
    child: Child,
    port: u16,
}

impl ServerProc {
    fn spawn(env: &Env, port: u16) -> Self {
        let mut cmd = runai();
        cmd.args(["server", "--host", "127.0.0.1", "--port"])
            .arg(port.to_string())
            .env("HOME", env.home())
            .env("RUNE_DATA_DIR", env.data_dir())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn runai server");

        // Drain stdout in a thread so a full pipe doesn't deadlock the child.
        if let Some(out) = child.stdout.take() {
            std::thread::spawn(move || {
                let r = BufReader::new(out);
                for _line in r.lines().map_while(Result::ok) {
                    // discard
                }
            });
        }
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                let r = BufReader::new(err);
                for _line in r.lines().map_while(Result::ok) {
                    // discard
                }
            });
        }

        let proc = Self { child, port };
        proc.wait_ready();
        proc
    }

    fn wait_ready(&self) {
        let url = format!("http://127.0.0.1:{}/api/timeline?hours=1", self.port);
        let deadline = Instant::now() + Duration::from_secs(15);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(800))
            .build()
            .unwrap();
        while Instant::now() < deadline {
            if let Ok(resp) = client.get(&url).send() {
                if resp.status().is_success() {
                    return;
                }
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        panic!(
            "runai server did not respond on 127.0.0.1:{} within 15s",
            self.port
        );
    }

    fn get_json(&self, path: &str) -> Value {
        let url = format!("http://127.0.0.1:{}{}", self.port, path);
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let resp = client.get(&url).send().expect("GET request");
        assert!(
            resp.status().is_success(),
            "GET {url} expected 2xx, got {}",
            resp.status()
        );
        resp.json::<Value>().expect("response is valid JSON")
    }
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[test]
fn api_timeline_aggregation_by_model() {
    let env = Env::new();
    let now = chrono::Utc::now().timestamp();

    // Seed events across multiple models inside a 1h window — the
    // endpoint aggregates everything regardless of model.
    // Place them comfortably inside the window (not at the boundary).
    let inside = now - 600; // 10 min ago
    env.insert_event(inside, "deepseek-chat", "ok", true, 100);
    env.insert_event(inside, "claude-sonnet", "ok", false, 200);
    env.insert_event(inside, "gpt-4o-mini", "error", false, 300);
    env.insert_event(inside, "deepseek-chat", "ok", true, 400);

    let port = pick_port();
    let server = ServerProc::spawn(&env, port);
    // 1h window → buckets = 3600/bucket_secs; default bucket = (1*3600/48).max(60)=75
    let json = server.get_json("/api/timeline?hours=1");

    let bucket_secs = json["bucket_secs"].as_i64().expect("bucket_secs i64");
    assert!(
        bucket_secs >= 60,
        "bucket_secs floor is 60s, got {bucket_secs}"
    );

    let points = json["points"].as_array().expect("points array");
    assert!(!points.is_empty(), "expected non-empty points array");

    // Sum totals/hits/errors across all buckets — must reflect every seed
    // row regardless of model.
    let total: i64 = points
        .iter()
        .map(|p| p["total"].as_i64().unwrap_or(0))
        .sum();
    let hits: i64 = points.iter().map(|p| p["hits"].as_i64().unwrap_or(0)).sum();
    let errors: i64 = points
        .iter()
        .map(|p| p["errors"].as_i64().unwrap_or(0))
        .sum();
    assert_eq!(
        total, 4,
        "all 4 seed events must be aggregated (no model filter)"
    );
    assert_eq!(hits, 2, "two ok+non-empty chosen rows are hits");
    assert_eq!(errors, 1, "one status!=ok row is an error");
}

#[test]
fn api_timeline_hourly_binning() {
    let env = Env::new();
    let port = pick_port();
    let server = ServerProc::spawn(&env, port);

    // The src formula:
    //   target_buckets = 48
    //   default_bucket = max((hours*3600)/48, 60)
    //   buckets = max((hours*3600)/default_bucket, 1)
    // For hours=24: default_bucket = 1800; buckets = 48
    // For hours=6:  default_bucket = max(450, 60) = 450; buckets = 48
    // For hours=1:  default_bucket = max(75, 60) = 75; buckets = 48
    // Override bucket_secs=3600 with hours=24 → 24 buckets of 1h.

    let j24 = server.get_json("/api/timeline?hours=24");
    assert_eq!(j24["bucket_secs"].as_i64().unwrap(), 1800);
    assert_eq!(
        j24["points"].as_array().unwrap().len(),
        48,
        "24h / 1800s = 48 buckets"
    );

    let j_hr = server.get_json("/api/timeline?hours=24&bucket_secs=3600");
    assert_eq!(j_hr["bucket_secs"].as_i64().unwrap(), 3600);
    assert_eq!(
        j_hr["points"].as_array().unwrap().len(),
        24,
        "24h hourly bucket override → 24 points"
    );

    // bucket_secs floor is 60s — under-floor inputs get bumped.
    let j_floor = server.get_json("/api/timeline?hours=1&bucket_secs=1");
    assert_eq!(
        j_floor["bucket_secs"].as_i64().unwrap(),
        60,
        "bucket_secs clamped up to 60s"
    );
}

#[test]
fn api_timeline_empty_response() {
    let env = Env::new();
    let port = pick_port();
    let server = ServerProc::spawn(&env, port);

    let json = server.get_json("/api/timeline?hours=24");
    let bucket_secs = json["bucket_secs"].as_i64().unwrap();
    let points = json["points"].as_array().unwrap();

    assert_eq!(bucket_secs, 1800);
    assert_eq!(points.len(), 48, "empty DB still returns full bucket grid");

    // Every point must be zeroed but structurally complete.
    for (i, p) in points.iter().enumerate() {
        assert_eq!(p["total"].as_i64().unwrap_or(-1), 0, "point {i} total=0");
        assert_eq!(p["hits"].as_i64().unwrap_or(-1), 0, "point {i} hits=0");
        assert_eq!(p["errors"].as_i64().unwrap_or(-1), 0, "point {i} errors=0");
        // avg_latency_ms is f64 — zero rows COALESCE to 0.0.
        let avg = p["avg_latency_ms"].as_f64().expect("avg_latency_ms f64");
        assert!((avg - 0.0).abs() < f64::EPSILON, "point {i} avg=0");
        assert!(
            p["ts_start"].as_i64().is_some(),
            "point {i} ts_start present"
        );
    }
}

#[test]
fn api_timeline_timezone_handling() {
    let env = Env::new();
    let port = pick_port();
    let server = ServerProc::spawn(&env, port);

    // hours=24, default bucket = 1800s. Consecutive ts_start deltas
    // must equal bucket_secs regardless of host timezone (ts_start is
    // unix epoch seconds in UTC, not local time).
    let json = server.get_json("/api/timeline?hours=24");
    let bucket_secs = json["bucket_secs"].as_i64().unwrap();
    let points = json["points"].as_array().unwrap();
    assert!(points.len() >= 2);

    let mut prev = points[0]["ts_start"].as_i64().unwrap();
    for p in points.iter().skip(1) {
        let cur = p["ts_start"].as_i64().unwrap();
        assert_eq!(
            cur - prev,
            bucket_secs,
            "ts_start grid must be uniformly spaced by bucket_secs"
        );
        prev = cur;
    }

    // The whole grid must end on/just before "now" (within 1 bucket).
    let now = chrono::Utc::now().timestamp();
    let last = points.last().unwrap()["ts_start"].as_i64().unwrap();
    assert!(
        last <= now,
        "last ts_start ({last}) must be <= now ({now}) — grid ends at present"
    );
    assert!(
        now - last <= bucket_secs * 2,
        "last ts_start must be within 2 buckets of now (now={now}, last={last}, bucket={bucket_secs})"
    );

    // Values are epoch seconds, not nanos / millis: a value near `now`
    // is ~10 digits, not 13/19.
    assert!(
        last.to_string().len() <= 11,
        "ts_start is seconds since epoch, not ms/ns (got {last})"
    );
}

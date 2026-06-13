//! P2 e2e coverage for `sm_recommend_stats` MCP tool.
//!
//! Strategy: spawn the real `runai mcp-serve` binary under an isolated
//! HOME + `RUNE_DATA_DIR`, pre-populate `runai.db`'s `router_events` table
//! by inserting rows over rusqlite (after first letting runai initialize
//! the schema), then drive the tool over stdio JSON-RPC and assert on
//! the formatted text result.

use rusqlite::{Connection, params};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Test harness helpers
// ---------------------------------------------------------------------------

const BIN: &str = "/Users/crosery/.cargo/bin/runai";

/// Holds the per-test sandbox so `Drop` cleans the tempdir + child process.
struct Harness {
    _home: tempfile::TempDir,
    data_dir: PathBuf,
}

impl Harness {
    /// Create a sandbox HOME + RUNE_DATA_DIR, run `runai list` once so the
    /// schema is migrated to the current version, return the harness.
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tempdir");
        let data_dir = home.path().join(".runai");

        // Trigger schema initialization. We do not assert success state, only
        // that the binary ran far enough to create runai.db. `list` is read-only.
        let status = Command::new(BIN)
            .arg("list")
            .env("HOME", home.path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", &data_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn runai list");
        assert!(status.success(), "runai list failed during schema init");

        let db_path = data_dir.join("runai.db");
        assert!(
            db_path.exists(),
            "expected runai.db at {} after init",
            db_path.display()
        );

        Self {
            _home: home,
            data_dir,
        }
    }

    fn db_path(&self) -> PathBuf {
        self.data_dir.join("runai.db")
    }

    fn home_path(&self) -> &Path {
        self._home.path()
    }
}

/// Insert one router_event row. Fields not exercised by sm_recommend_stats
/// get sensible defaults. `ts` is an absolute unix timestamp (seconds).
#[allow(clippy::too_many_arguments)]
fn insert_event(
    conn: &Connection,
    ts: i64,
    provider: &str,
    model: &str,
    prompt_tokens: i64,
    completion_tokens: i64,
    total_tokens: i64,
    latency_ms: i64,
    chosen_skills_json: &str,
    status: &str,
) {
    conn.execute(
        "INSERT INTO router_events (
            ts, provider, model,
            prompt_tokens, completion_tokens, reasoning_tokens, total_tokens,
            cache_hit_tokens, cache_miss_tokens,
            latency_ms, chosen_skills_json, candidate_count, status, error_msg,
            session_id, mode,
            user_prompt, cwd, bm25_kept,
            llm_raw_response, hook_output, llm_input
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, 0, 0, ?7, ?8, 0, ?9, NULL, '', 'exclusive', '', '', 0, '', '', '')",
        params![
            ts,
            provider,
            model,
            prompt_tokens,
            completion_tokens,
            total_tokens,
            latency_ms,
            chosen_skills_json,
            status,
        ],
    )
    .expect("insert router_event");
}

/// Spawn `runai mcp-serve`, do MCP initialize handshake, and return the child
/// + framed stdio reader/writer ready for tools/call.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<std::process::ChildStdout>,
}

impl Mcp {
    fn start(h: &Harness) -> Self {
        let mut child = Command::new(BIN)
            .arg("mcp-serve")
            .env("HOME", h.home_path())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", &h.data_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn runai mcp-serve");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let reader = BufReader::new(stdout);

        let mut me = Self {
            child,
            stdin,
            reader,
        };

        // MCP initialize handshake
        let init = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "p2c0-test", "version": "1.0"}
            }
        });
        me.send(&init);
        let resp = me.read_response();
        assert_eq!(
            resp["jsonrpc"], "2.0",
            "init response not jsonrpc 2.0: {resp}"
        );
        assert!(
            resp["result"]["serverInfo"].is_object(),
            "init missing serverInfo: {resp}"
        );

        // Notify initialized so the server is ready for tool calls.
        let notif = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        me.send(&notif);
        std::thread::sleep(Duration::from_millis(150));

        me
    }

    fn send(&mut self, v: &serde_json::Value) {
        writeln!(self.stdin, "{}", serde_json::to_string(v).unwrap()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read_response(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.reader
            .read_line(&mut line)
            .expect("read response line");
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("non-JSON line from mcp-serve: {e}\nGot: {line}"))
    }

    /// Call `sm_recommend_stats` with optional `hours` and `recent` params,
    /// return the textual result string (concatenated content blocks of type `text`).
    fn call_recommend_stats(&mut self, hours: Option<i64>, recent: Option<u64>) -> String {
        let mut args = serde_json::Map::new();
        if let Some(h) = hours {
            args.insert("hours".into(), serde_json::json!(h));
        }
        if let Some(r) = recent {
            args.insert("recent".into(), serde_json::json!(r));
        }
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": "sm_recommend_stats",
                "arguments": serde_json::Value::Object(args),
            }
        });
        self.send(&req);
        let resp = self.read_response();
        assert!(
            resp.get("error").is_none(),
            "sm_recommend_stats returned error: {resp}"
        );
        let content = resp["result"]["content"]
            .as_array()
            .unwrap_or_else(|| panic!("expected content array, got {resp}"));
        let mut out = String::new();
        for block in content {
            if block["type"] == "text"
                && let Some(t) = block["text"].as_str()
            {
                out.push_str(t);
            }
        }
        // The tool wraps the formatted lines in a TextResult JSON. Some
        // rmcp versions surface that as a JSON-encoded payload in text;
        // others pass through the inner string. Handle both — if the text
        // is JSON with a "result" field, extract it.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&out)
            && let Some(inner) = v.get("result").and_then(|r| r.as_str())
        {
            return inner.to_string();
        }
        out
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Drain stderr (mostly empty) so the process exits cleanly on CI.
        if let Some(mut err) = self.child.stderr.take() {
            let mut buf = Vec::new();
            let _ = err.read_to_end(&mut buf);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn sm_recommend_stats_no_history_returns_zero() {
    let h = Harness::new();
    // No rows inserted at all.
    let mut mcp = Mcp::start(&h);
    let text = mcp.call_recommend_stats(None, None);

    assert!(
        text.contains("Router LLM telemetry (all-time)"),
        "missing all-time header: {text}"
    );
    assert!(
        text.contains("total calls:        0"),
        "expected zero total_calls, got: {text}"
    );
    assert!(
        text.contains("errors:             0"),
        "expected zero errors, got: {text}"
    );
    // No per-model section when empty.
    assert!(
        !text.contains("per model:"),
        "should not render per-model section when empty: {text}"
    );
    // No recent section when recent=None/0.
    assert!(
        !text.contains("recent calls"),
        "should not render recent section when recent omitted: {text}"
    );
}

#[test]
fn sm_recommend_stats_all_time_window() {
    let h = Harness::new();
    let conn = Connection::open(h.db_path()).unwrap();
    let now = chrono::Utc::now().timestamp();
    // 10 events spread across time, all 'ok'. Mix of models to exercise grouping
    // labels and totals.
    for i in 0..10 {
        let ts = now - (i as i64 * 3600);
        let model = if i % 2 == 0 {
            "claude-3.5-sonnet"
        } else {
            "gpt-4"
        };
        insert_event(
            &conn,
            ts,
            "anthropic",
            model,
            100,
            50,
            150,
            200,
            "[\"alpha\"]",
            "ok",
        );
    }
    drop(conn);

    let mut mcp = Mcp::start(&h);
    let text = mcp.call_recommend_stats(None, None);

    assert!(
        text.contains("all-time"),
        "expected 'all-time' label: {text}"
    );
    assert!(
        text.contains("total calls:        10"),
        "expected total_calls=10, got: {text}"
    );
    assert!(
        text.contains("per model:"),
        "expected per-model section, got: {text}"
    );
    // Both models in summary
    assert!(
        text.contains("claude-3.5-sonnet"),
        "expected sonnet in per-model: {text}"
    );
    assert!(
        text.contains("gpt-4"),
        "expected gpt-4 in per-model: {text}"
    );
    // Each grouped row carries a calls + tokens column.
    assert!(text.contains("calls"), "row label includes 'calls': {text}");
    assert!(
        text.contains("tokens"),
        "row label includes 'tokens': {text}"
    );
}

#[test]
fn sm_recommend_stats_time_window_filter() {
    let h = Harness::new();
    let conn = Connection::open(h.db_path()).unwrap();
    let now = chrono::Utc::now().timestamp();

    // 5 events inside the last hour (ts = now - 60s..1500s)
    for i in 0..5 {
        let ts = now - (i as i64 * 300); // 0, 300, 600, 900, 1200 sec back -> all < 3600
        insert_event(
            &conn,
            ts,
            "anthropic",
            "claude-3.5-sonnet",
            100,
            50,
            150,
            200,
            "[\"alpha\"]",
            "ok",
        );
    }
    // 5 events older than 24 hours (ts = now - 25h..29h)
    for i in 0..5 {
        let ts = now - (25 + i as i64) * 3600;
        insert_event(
            &conn,
            ts,
            "anthropic",
            "claude-3.5-sonnet",
            100,
            50,
            150,
            200,
            "[\"beta\"]",
            "ok",
        );
    }
    drop(conn);

    let mut mcp = Mcp::start(&h);
    let text = mcp.call_recommend_stats(Some(1), None);

    assert!(
        text.contains("last 1h"),
        "expected 'last 1h' label, got: {text}"
    );
    assert!(
        text.contains("total calls:        5"),
        "expected total_calls=5 after 1h filter, got: {text}"
    );
    // The 5 older events must not bleed in: their tokens shouldn't be summed.
    // Inside the window: 5 * 150 = 750. Outside: 5 * 150 = 750. So total
    // tokens for window must be 750 not 1500.
    assert!(
        text.contains("total tokens:       750"),
        "expected 750 total tokens in window, got: {text}"
    );
}

#[test]
fn sm_recommend_stats_recent_events() {
    let h = Harness::new();
    let conn = Connection::open(h.db_path()).unwrap();
    let now = chrono::Utc::now().timestamp();
    // 10 events with strictly decreasing ts so the 'newest' subset is deterministic.
    for i in 0..10 {
        let ts = now - i as i64; // i=0 is newest
        insert_event(
            &conn,
            ts,
            "anthropic",
            &format!("model-{i}"),
            10 * (i + 1) as i64,
            5 * (i + 1) as i64,
            15 * (i + 1) as i64,
            100 + i as i64,
            &format!("[\"skill-{i}\"]"),
            "ok",
        );
    }
    drop(conn);

    let mut mcp = Mcp::start(&h);
    let text = mcp.call_recommend_stats(None, Some(3));

    assert!(
        text.contains("recent calls (newest first):"),
        "missing recent-calls header: {text}"
    );
    // 3 newest events have model-0, model-1, model-2 (and skill-0..skill-2).
    for i in 0..3 {
        assert!(
            text.contains(&format!("model-{i}")),
            "expected model-{i} in recent section: {text}"
        );
        assert!(
            text.contains(&format!("skill-{i}")),
            "expected skill-{i} in recent section: {text}"
        );
    }
    // Older entries (i >= 3) MUST NOT appear in the recent section. The
    // per-model summary above does include them, so we look for skill-N
    // (chosen_skills_json) which is unique to the recent section's chosen
    // column.
    for i in 3..10 {
        assert!(
            !text.contains(&format!("skill-{i}")),
            "older event skill-{i} leaked into top-3 recent: {text}"
        );
    }
    // Ordering: model-0 must appear before model-1 before model-2 (newest first).
    let p0 = text.find("model-0").expect("model-0 present");
    let p1 = text.find("model-1").expect("model-1 present");
    let p2 = text.find("model-2").expect("model-2 present");
    // model-0/1/2 each appear in BOTH the per-model summary AND the recent
    // section. The per-model summary is alphabetically/order-of-aggregation,
    // but the recent section is strict newest-first. We assert that the
    // LAST occurrences (i.e. in the recent section, since it's the
    // bottom-most block) are ordered 0 < 1 < 2.
    let last0 = text.rfind("model-0").unwrap();
    let last1 = text.rfind("model-1").unwrap();
    let last2 = text.rfind("model-2").unwrap();
    // Sanity: each model appears more than once (per-model + recent) for the
    // top-3 newest, so last_pos > first_pos.
    assert!(last0 >= p0 && last1 >= p1 && last2 >= p2);
    assert!(
        last0 < last1 && last1 < last2,
        "recent section ordering wrong: model-0={last0} model-1={last1} model-2={last2}\n{text}"
    );
}

#[test]
fn sm_recommend_stats_per_model_grouping() {
    let h = Harness::new();
    let conn = Connection::open(h.db_path()).unwrap();
    let now = chrono::Utc::now().timestamp();

    // 3 sonnet rows totaling 600 tokens; 2 gpt-4 rows totaling 300 tokens.
    for i in 0..3 {
        insert_event(
            &conn,
            now - i as i64 * 10,
            "anthropic",
            "claude-3.5-sonnet",
            100,
            100,
            200,
            150,
            "[\"foo\"]",
            "ok",
        );
    }
    for i in 0..2 {
        insert_event(
            &conn,
            now - 1000 - i as i64 * 10,
            "openai",
            "gpt-4",
            100,
            50,
            150,
            300,
            "[\"bar\"]",
            "ok",
        );
    }
    drop(conn);

    let mut mcp = Mcp::start(&h);
    let text = mcp.call_recommend_stats(None, None);

    assert!(
        text.contains("per model:"),
        "missing per-model section: {text}"
    );

    // Aggregation: sonnet 3 calls, 600 tokens; gpt-4 2 calls, 300 tokens.
    // The format string in tools.rs is:
    //   "    {:<28} {:>5} calls  {:>9} tokens"
    // So we look for substrings that anchor both model name + call count.
    let lines: Vec<&str> = text.lines().collect();
    let sonnet_line = lines
        .iter()
        .find(|l| l.contains("claude-3.5-sonnet") && l.contains("calls"))
        .unwrap_or_else(|| panic!("no sonnet per-model line in: {text}"));
    let gpt_line = lines
        .iter()
        .find(|l| l.contains("gpt-4") && l.contains("calls"))
        .unwrap_or_else(|| panic!("no gpt-4 per-model line in: {text}"));

    assert!(
        sonnet_line.contains("3 calls"),
        "sonnet should have 3 calls, line was: {sonnet_line}"
    );
    assert!(
        sonnet_line.contains("600 tokens"),
        "sonnet should sum to 600 tokens, line was: {sonnet_line}"
    );
    assert!(
        gpt_line.contains("2 calls"),
        "gpt-4 should have 2 calls, line was: {gpt_line}"
    );
    assert!(
        gpt_line.contains("300 tokens"),
        "gpt-4 should sum to 300 tokens, line was: {gpt_line}"
    );

    // Total calls across both groups = 5.
    assert!(
        text.contains("total calls:        5"),
        "expected total_calls=5 across both models, got: {text}"
    );
}

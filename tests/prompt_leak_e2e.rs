//! Prompt leakage physical e2e (PLANNING §1.3 guard).
//!
//! Verifies that the `/recommend` endpoint NEVER returns internal LLM
//! prompt template content (system prompt, user-msg scaffold, supporting
//! blocks like `recommend_history_prefix.md` etc.) in either the response
//! body OR the response headers. Those templates are sent ONLY to the
//! upstream LLM — leaking any of them back over HTTP to a remote client
//! would expose runai's routing strategy and the toggleable per-user
//! injection mechanism.
//!
//! Strategy:
//!   1. Spawn the real `runai server --mode team` binary on a free
//!      127.0.0.1 port inside an isolated HOME so the live home is never
//!      touched.
//!   2. Pre-plant a public skill so /recommend has a non-empty candidate
//!      set (otherwise the router short-circuits and never bothers to
//!      build/leak anything).
//!   3. POST /users/register → grab api_key.
//!   4. POST /recommend with Bearer api_key + the canonical Claude Code
//!      UserPromptSubmit JSON shape. The router will try to call the LLM
//!      and fail (no provider configured by default in a fresh team
//!      server) — but the server's RESPONSE to the client is the thing
//!      we're scanning, regardless of LLM outcome.
//!   5. Dump status + headers + body to /tmp/leak-dump.txt, then grep -if
//!      against /tmp/prompt-fingerprints.txt. Any match = leak = fail.
//!
//! The fingerprint file is the contract — it ships with this test (built
//! by the same scaffolding) and pins ~25 distinctive substrings (≥ 50
//! bytes of UTF-8 each) drawn straight from the .md prompt templates.

#![cfg(not(target_os = "windows"))]

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

// ─── helpers (mirror of tests/server_mode_e2e.rs::spawn_server pattern) ─────

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

fn spawn_team_server(home: TempDir, port: u16) -> ServerGuard {
    std::fs::create_dir_all(home.path().join(".runai/skills")).expect("pre-create .runai/skills");
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
        "runai server did not bind 127.0.0.1:{port} within 8s"
    );
    guard
}

fn write_test_skill(home_path: &Path, name: &str) {
    let dir = home_path.join(".runai/skills").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: prompt-leak test skill\n---\n\n# {name}\n"),
    )
    .unwrap();
}

/// Write a `~/.runai/config.toml` with recommend ENABLED pointing at an
/// unreachable localhost port. This forces the server's `/recommend`
/// handler to walk the full router pipeline — build the LLM messages
/// array (which carries every fingerprint string we're scanning for),
/// fail fast on the LLM call, and persist the error path. The HTTP
/// response back to the client is what we then assert is template-free.
/// Without this, the router short-circuits at the `if !cfg.enabled`
/// guard and never builds the LLM input — the test would pass trivially
/// because no template text ever entered any code path that touches the
/// HTTP response.
fn write_recommend_config(home_path: &Path) {
    let cfg = home_path.join(".runai/config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    // Field names match `RecommendConfig` in src/core/recommend/config.rs.
    // `summary_lang_confirmed = true` skips the enrich gate so we don't
    // get sidetracked by an unrelated warning. The provider is openai-
    // compat at a TCP-refused address; `recommend_for_user` will reach
    // the llm_call site, hit ECONNREFUSED, and bail.
    let toml = r#"
[recommend]
enabled = true
provider = "openai-compat"
base_url = "http://127.0.0.1:1"
model = "leak-test"
api_key = "dummy"
top_k = 8
min_prompt_len = 0
summary_lang = "zh"
summary_lang_confirmed = true
session_mode = "oneshot"
session_history_limit = 20
saved_providers = []
active_provider_id = ""
read_claude_md = false
skip_reminder_enabled = false
skip_reminder_template = "if the prompt does not match any candidate just skip"
"#;
    std::fs::write(&cfg, toml).unwrap();
}

fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("reqwest client")
}

// ─── fingerprint loader ────────────────────────────────────────────────────

/// Read the fingerprint list. Each non-empty, non-comment line is one
/// substring we assert is absent from every byte of the recommend response.
/// Lives at `/tmp/prompt-fingerprints.txt` so the harness that builds
/// fingerprints (P3) and the test that consumes them share the same file
/// path — keeps the contract grep-able from outside cargo.
fn load_fingerprints() -> Vec<String> {
    let path = "/tmp/prompt-fingerprints.txt";
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("fingerprint file missing at {path}: {e} — run the P3 fingerprint generator first")
    });
    let lines: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|s| s.to_string())
        .collect();
    assert!(
        !lines.is_empty(),
        "fingerprint file at {path} is empty — at least one substring is required"
    );
    lines
}

// ─── the test ──────────────────────────────────────────────────────────────

/// Drive a real `/recommend` request through the real server and assert
/// the response (status + headers + body) contains none of the LLM-only
/// prompt fingerprints. Dumps everything to `/tmp/leak-dump.txt` so a
/// failed run is auditable from outside cargo.
#[test]
fn recommend_response_does_not_leak_llm_prompt_text() {
    let home = tempfile::tempdir().expect("create tmp HOME");
    write_test_skill(home.path(), "alpha-skill");
    write_test_skill(home.path(), "beta-skill");
    write_recommend_config(home.path());
    let port = free_port();
    let server = spawn_team_server(home, port);

    let client = http_client();

    // 1. Register a user → get api_key.
    let reg_resp = client
        .post(format!("{}/users/register", server.base_url()))
        .json(&serde_json::json!({
            "username": "leak-tester",
            "password": "correct horse battery staple",
        }))
        .send()
        .expect("POST /users/register");
    assert_eq!(
        reg_resp.status().as_u16(),
        201,
        "expected 201 from /users/register in team mode"
    );
    let reg_body: serde_json::Value = reg_resp.json().expect("register JSON body");
    let api_key = reg_body
        .get("api_key")
        .and_then(|v| v.as_str())
        .expect("api_key in register response")
        .to_string();

    // 2. POST /recommend with the canonical Claude Code hook JSON shape.
    let payload = serde_json::json!({
        "prompt": "帮我画一个有视觉冲击力的页面",
        "session_id": "leak-test-session",
        "cwd": "/tmp/leak-test-cwd",
        "transcript_path": "",
    });
    let rec_resp = client
        .post(format!("{}/recommend", server.base_url()))
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&payload)
        .send()
        .expect("POST /recommend");

    let status = rec_resp.status();
    let headers = rec_resp.headers().clone();
    let body = rec_resp.text().unwrap_or_default();

    // 3. Dump everything to /tmp/leak-dump.txt so the human auditor can
    //    inspect even when the assertions pass.
    let dump_path = "/tmp/leak-dump.txt";
    {
        let mut f = std::fs::File::create(dump_path).expect("create dump");
        writeln!(f, "=== /recommend status ===").unwrap();
        writeln!(f, "{status}").unwrap();
        writeln!(f, "\n=== /recommend headers ===").unwrap();
        for (k, v) in headers.iter() {
            writeln!(f, "{}: {}", k.as_str(), v.to_str().unwrap_or("<non-ascii>")).unwrap();
        }
        writeln!(f, "\n=== /recommend body ({} bytes) ===", body.len()).unwrap();
        writeln!(f, "{body}").unwrap();
    }

    // 4. Grep -i: case-insensitive substring check across the whole dump
    //    payload (headers + body, the two surfaces a remote client sees).
    //    Build a single haystack mirroring what `grep -if fingerprints
    //    leak-dump.txt` would scan.
    let mut haystack = String::new();
    for (k, v) in headers.iter() {
        haystack.push_str(k.as_str());
        haystack.push_str(": ");
        haystack.push_str(v.to_str().unwrap_or(""));
        haystack.push('\n');
    }
    haystack.push_str(&body);
    let haystack_lower = haystack.to_lowercase();

    let fingerprints = load_fingerprints();
    let mut hits: Vec<(String, String)> = Vec::new();
    for fp in &fingerprints {
        let needle = fp.to_lowercase();
        if let Some(idx) = haystack_lower.find(&needle) {
            // Pull a small context window around the hit so the failure
            // message + the dump file together pinpoint where the leak
            // is in the response.
            let start = idx.saturating_sub(40);
            let end = (idx + needle.len() + 40).min(haystack.len());
            let snippet = haystack
                .get(start..end)
                .unwrap_or("<snippet out of utf8 bounds>")
                .replace('\n', "\\n");
            hits.push((fp.clone(), snippet));
        }
    }

    if !hits.is_empty() {
        let mut detail = String::new();
        detail.push_str(&format!(
            "PROMPT LEAK DETECTED in /recommend response (status={status}, body_len={}).\n",
            body.len()
        ));
        detail.push_str(&format!("Full dump: {dump_path}\n"));
        detail.push_str("\nMatching fingerprints (fp -> snippet):\n");
        for (fp, snip) in &hits {
            detail.push_str(&format!("  - [{fp}] ... {snip} ...\n"));
        }
        panic!("{detail}");
    }
}

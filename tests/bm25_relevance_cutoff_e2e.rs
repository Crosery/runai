//! BM25 relevance cutoff + empty-candidate Stage-2 short-circuit.
//!
//! The router used to fill `bm25_candidate_limit` (30) candidate slots by the
//! hybrid `bm25*0.35 + llm/10*0.45 + feedback*0.20` score even when a skill had
//! ZERO query-term overlap — the llm/feedback prior alone (~0.33 at bm25=0)
//! floated unrelated skills into the prompt, so a query with no matching skill
//! still burned a full Stage-2 router call over pure noise.
//!
//! This suite spawns the real `runai` binary against an in-process mock LLM and
//! asserts: (1) a query whose intent has no term overlap with any installed
//! skill yields ZERO candidates and NO Stage-2 call (the Stage-2 request count
//! stays 0), while Stage-1 still runs and a telemetry row lands with
//! `bm25_kept = 0` / `status = ok`; (2) a query that DOES overlap a skill still
//! reaches Stage-2 and surfaces that skill (the cutoff never over-filters a real
//! match).
#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

struct TestEnv {
    home: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tmp HOME");
        std::fs::create_dir_all(home.path().join(".runai/skills")).unwrap();
        std::fs::write(home.path().join(".runai/.bootstrap-seen"), "1").unwrap();
        Self { home }
    }
    fn home(&self) -> &Path {
        self.home.path()
    }
    fn runai_dir(&self) -> PathBuf {
        self.home().join(".runai")
    }
    fn db_path(&self) -> PathBuf {
        self.runai_dir().join("runai.db")
    }
    fn plant_skill(&self, name: &str, description: &str) {
        let dir = self.runai_dir().join("skills").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\n{description}\n"
            ),
        )
        .unwrap();
        let out = self.run(&["scan"]);
        assert!(out.status.success(), "scan failed");
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(RUNAI_BIN)
            .args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.runai_dir())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("RUNAI_RECOMMEND_API_KEY")
            .output()
            .expect("spawn runai")
    }
    fn recommend(&self, prompt: &str, session: &str) -> String {
        use std::process::Stdio;
        let payload = serde_json::json!({
            "prompt": prompt,
            "session_id": session,
            "client_kind": "claude",
            "cwd": "/tmp/cutoff-proj",
        });
        let mut child = Command::new(RUNAI_BIN)
            .args(["recommend"])
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.runai_dir())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("RUNAI_RECOMMEND_API_KEY")
            .env("RUNAI_BM25_TOP_K", "5")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn runai");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().expect("wait runai");
        assert!(
            out.status.success(),
            "recommend failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }
    fn write_config(&self, base_url: &str) {
        let body = format!(
            "[recommend]\nenabled = true\nprovider = \"openai-compat\"\nbase_url = \"{base_url}\"\nmodel = \"mock-model\"\napi_key = \"test-key\"\ntop_k = 8\nmin_prompt_len = 0\nsummary_lang = \"zh\"\nsession_mode = \"oneshot\"\nsession_history_limit = 0\n"
        );
        std::fs::write(self.runai_dir().join("config.toml"), body).unwrap();
    }
    /// (bm25_kept, status, intent_output_nonempty, empty_reason, calls).
    fn last_event(&self) -> Option<(i64, String, bool, String, i64)> {
        if !self.db_path().exists() {
            return None;
        }
        let conn = rusqlite::Connection::open(self.db_path()).ok()?;
        conn.query_row(
            "SELECT bm25_kept, status, intent_llm_output, empty_reason, llm_call_count FROM router_events ORDER BY id DESC LIMIT 1",
            rusqlite::params![],
            |r| {
                let kept: i64 = r.get(0)?;
                let status: String = r.get(1)?;
                let intent: String = r.get(2)?;
                let empty_reason: String = r.get(3)?;
                let calls: i64 = r.get(4)?;
                Ok((kept, status, !intent.trim().is_empty(), empty_reason, calls))
            },
        )
        .ok()
    }
}

/// Mock OpenAI-compatible endpoint. Stage-1 (intent) replies by echoing the
/// user's raw prompt as the BM25 intent artifact, so BM25 overlap is decided
/// purely by the planted skills. Stage-2 (router) replies with a fixed skill
/// pick AND increments a counter so the test can assert it was never called.
struct Mock {
    addr: String,
    stop: Arc<AtomicBool>,
    stage2_calls: Arc<AtomicUsize>,
    router_pick: Arc<Mutex<String>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Mock {
    fn start(router_pick: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stage2_calls = Arc::new(AtomicUsize::new(0));
        let pick = Arc::new(Mutex::new(router_pick.to_string()));
        let stop_t = stop.clone();
        let calls_t = stage2_calls.clone();
        let pick_t = pick.clone();
        let handle = thread::spawn(move || {
            let started = std::time::Instant::now();
            while !stop_t.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(30) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                        let body = read_body(&mut stream);
                        let is_intent = system_of(&body)
                            .map(|s| s.contains("第一波"))
                            .unwrap_or(false);
                        let is_router = serde_json::from_str::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|v| v.get("max_tokens").and_then(|n| n.as_i64()))
                            == Some(400);
                        let content = if is_intent {
                            let prompt = intent_prompt_of(&body);
                            format!("intent: {prompt}\ninclude_terms: {prompt}")
                        } else if is_router {
                            calls_t.fetch_add(1, Ordering::SeqCst);
                            let pick = pick_t.lock().unwrap().clone();
                            format!("EXCLUSIVE\nreasoning: mock 命中\n{pick}\n")
                        } else {
                            "task: 测试\ntriggers: test\ninputs: text\noutputs: text\nnot-for: none\nscore: 5".to_string()
                        };
                        let resp = openai_response(&content);
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                        drop(stream);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(15));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            stage2_calls,
            router_pick: pick,
            handle: Some(handle),
        }
    }
    fn base_url(&self) -> &str {
        &self.addr
    }
    fn stage2_calls(&self) -> usize {
        self.stage2_calls.load(Ordering::SeqCst)
    }
    fn set_pick(&self, pick: &str) {
        *self.router_pick.lock().unwrap() = pick.to_string();
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn read_body(stream: &mut std::net::TcpStream) -> String {
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    let header_end = loop {
        match stream.read(&mut buf) {
            Ok(0) => break None,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                if let Some(p) = acc.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(p);
                }
                if acc.len() > 1 << 22 {
                    break None;
                }
            }
            Err(_) => break None,
        }
    };
    let Some(he) = header_end else {
        return String::new();
    };
    let headers = String::from_utf8_lossy(&acc[..he]).to_lowercase();
    let content_len = headers
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = he + 4;
    while acc.len() - body_start < content_len {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&acc[body_start..]).to_string()
}

fn openai_response(content: &str) -> String {
    let body = serde_json::json!({
        "id": "mock",
        "object": "chat.completion",
        "choices": [{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}],
        "usage": {"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}
    })
    .to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
}

fn parse_messages(body: &str) -> Option<Vec<serde_json::Value>> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    v.get("messages")?.as_array().cloned()
}

fn system_of(body: &str) -> Option<String> {
    let msgs = parse_messages(body)?;
    let first = msgs.first()?;
    if first.get("role")?.as_str()? != "system" {
        return None;
    }
    Some(first.get("content")?.as_str()?.to_string())
}

/// The Stage-1 user message ends with `当前用户输入：\n<prompt>` — pull the prompt.
fn intent_prompt_of(body: &str) -> String {
    let msgs = parse_messages(body).unwrap_or_default();
    let last = msgs
        .last()
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    match last.split_once("当前用户输入：") {
        Some((_, tail)) => tail.trim().to_string(),
        None => last.trim().to_string(),
    }
}

/// Plant a spread of skills that share no vocabulary with the CJK ventoy prompt.
fn plant_noise(env: &TestEnv) {
    env.plant_skill(
        "swift-actor-persistence",
        "swift actor concurrency persistence state restore",
    );
    env.plant_skill(
        "react-component-builder",
        "react component frontend jsx props hooks render",
    );
    env.plant_skill(
        "sql-query-optimizer",
        "sql query optimize database index execution plan",
    );
    env.plant_skill(
        "kubernetes-deployer",
        "kubernetes deploy container cluster yaml pod service",
    );
    env.plant_skill(
        "audio-waveform-editor",
        "audio waveform trim normalize podcast sound",
    );
    env.plant_skill(
        "finance-ledger-audit",
        "finance ledger reconcile invoice accounting audit",
    );
}

#[test]
fn zero_overlap_query_skips_stage2_and_logs_empty_candidate_event() {
    let env = TestEnv::new();
    plant_noise(&env);
    let mock = Mock::start("");
    env.write_config(mock.base_url());

    // Real user failure case: intent has distinctive terms (ventoy/U盘/usb) that
    // match none of the English skills. No candidate has term overlap → the
    // router must not call Stage-2 at all.
    let hook = env.recommend("我插U盘了，直接移进去，U盘装了ventoy", "cutoff-empty");

    assert_eq!(
        mock.stage2_calls(),
        0,
        "Fast 全零检索证据时不得让先验复活无关候选：\n{hook}"
    );
    // No skill surfaced to the agent.
    for noise in [
        "swift-actor-persistence",
        "react-component-builder",
        "sql-query-optimizer",
        "kubernetes-deployer",
        "audio-waveform-editor",
        "finance-ledger-audit",
    ] {
        assert!(
            !hook.contains(noise),
            "no unrelated skill may be surfaced ({noise}):\n{hook}"
        );
    }

    // Telemetry: Fast skips both model calls when retrieval has no evidence.
    let (kept, status, intent_present, empty_reason, calls) =
        env.last_event().expect("a router_events row");
    assert_eq!(kept, 0);
    assert_eq!(empty_reason, "retrieval_zero");
    assert_eq!(calls, 0);
    assert_eq!(status, "ok");
    assert!(
        intent_present,
        "deterministic intent remains recorded for audit"
    );
}

#[test]
fn overlapping_query_still_reaches_stage2_and_surfaces_the_match() {
    let env = TestEnv::new();
    plant_noise(&env);
    // A genuinely matching skill: shares the ventoy/usb vocabulary.
    env.plant_skill("ventoy-usb-writer", "ventoy usb 启动盘 写入 iso 制作 U盘");
    let mock = Mock::start("ventoy-usb-writer");
    mock.set_pick("ventoy-usb-writer");
    env.write_config(mock.base_url());

    let hook = env.recommend("用 ventoy 做一个 usb 启动盘", "cutoff-match");

    assert!(
        mock.stage2_calls() >= 1,
        "Stage-2 must run when a candidate overlaps the query:\n{hook}"
    );
    assert!(
        hook.contains("ventoy-usb-writer"),
        "the matching skill must be surfaced:\n{hook}"
    );

    let (kept, status, _, empty_reason, calls) = env.last_event().expect("a router_events row");
    assert!(kept >= 1, "at least the matching candidate must be kept");
    assert_eq!(status, "ok");
    assert_eq!(empty_reason, "none");
    assert_eq!(calls, 1);
}

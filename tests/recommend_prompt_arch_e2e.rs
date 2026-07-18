//! Phase-5 prompt architecture regression: fixed system prompts (prefix-cache
//! friendly), one-shot dynamic user fields, session no-repeat removed, and a
//! token-budget ceiling on the Stage-1/Stage-2 user messages.
//!
//! Unlike `core_recommend_e2e_p1c0` (which reads what the binary *stored* in
//! `router_events`), this suite RECORDS the actual HTTP request bodies the
//! router sends to the LLM — the only way to assert the outgoing **system**
//! message (never persisted) is byte-identical across requests, which is the
//! precondition for provider prefix-cache hits.
//!
//! The router LLM is mocked with an in-process TCP listener; the real `runai`
//! binary runs inside an isolated HOME + RUNE_DATA_DIR sandbox.
#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
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
    fn run_with_input(&self, args: &[&str], stdin: &str) -> std::process::Output {
        use std::process::Stdio;
        let mut child = Command::new(RUNAI_BIN)
            .args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.runai_dir())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("RUNAI_RECOMMEND_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn runai");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().expect("wait runai")
    }
    fn write_config(&self, base_url: &str) {
        let body = format!(
            "[recommend]\nenabled = true\nprovider = \"openai-compat\"\nbase_url = \"{base_url}\"\nmodel = \"mock-model\"\napi_key = \"test-key\"\ntop_k = 8\nmin_prompt_len = 0\nsummary_lang = \"zh\"\nsession_mode = \"oneshot\"\nsession_history_limit = 0\n"
        );
        std::fs::write(self.runai_dir().join("config.toml"), body).unwrap();
    }
}

/// A mock OpenAI-compatible endpoint that records every request body it
/// receives, and replies based on the request's system prompt (Stage-1 intent
/// vs Stage-2 router) so a single mock drives both waves of any number of
/// recommend calls.
struct RecordingMock {
    addr: String,
    stop: Arc<AtomicBool>,
    bodies: Arc<Mutex<Vec<String>>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl RecordingMock {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let stop_t = stop.clone();
        let bodies_t = bodies.clone();
        let handle = thread::spawn(move || {
            let started = std::time::Instant::now();
            while !stop_t.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(30) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                        let mut acc: Vec<u8> = Vec::new();
                        let mut buf = [0u8; 8192];
                        // Read headers.
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
                        let body = if let Some(he) = header_end {
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
                        } else {
                            String::new()
                        };
                        let is_intent = system_of(&body)
                            .map(|s| s.contains("第一波"))
                            .unwrap_or(false);
                        bodies_t.lock().unwrap().push(body);
                        let content = if is_intent {
                            "intent: 处理 alpha 任务\ninclude_terms: alpha"
                        } else {
                            "EXCLUSIVE\nreasoning: alpha 命中\nalpha-skill\n"
                        };
                        let resp = openai_response(content);
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
            bodies,
            handle: Some(handle),
        }
    }
    fn base_url(&self) -> &str {
        &self.addr
    }
    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }
}

impl Drop for RecordingMock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
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

fn user_of(body: &str) -> Option<String> {
    let msgs = parse_messages(body)?;
    let last = msgs.last()?;
    Some(last.get("content")?.as_str()?.to_string())
}

fn max_tokens_of(body: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("max_tokens")?
        .as_i64()
}

fn recommend_stdin(env: &TestEnv, prompt: &str, session: &str) -> String {
    let payload = serde_json::json!({
        "prompt": prompt,
        "session_id": session,
        "client_kind": "codex",
        "cwd": "/tmp/arch-proj",
    });
    let out = env.run_with_input(&["recommend"], &payload.to_string());
    assert!(
        out.status.success(),
        "recommend failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn fixed_system_prompts_are_byte_identical_across_requests() {
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "处理 alpha 任务 alpha 工具");
    let mock = RecordingMock::start();
    env.write_config(mock.base_url());

    // Two DIFFERENT prompts in the same session — memory/prompt differ, but the
    // system messages must stay byte-identical (no dynamic values baked in).
    let _ = recommend_stdin(&env, "帮我处理 alpha 任务", "arch-session");
    let _ = recommend_stdin(&env, "再换一个方式处理 alpha", "arch-session");

    let bodies = mock.bodies();
    let intent_systems: Vec<String> = bodies
        .iter()
        .filter_map(|b| system_of(b))
        .filter(|s| s.contains("第一波"))
        .collect();
    let router_systems: Vec<String> = bodies
        .iter()
        .filter_map(|b| system_of(b))
        .filter(|s| s.contains("skill router"))
        .collect();

    assert_eq!(intent_systems.len(), 0, "Fast must skip Stage-1");
    assert_eq!(
        router_systems.len(),
        2,
        "expected 2 Stage-2 router calls, got {}",
        router_systems.len()
    );
    assert_eq!(
        router_systems[0], router_systems[1],
        "Stage-2 system prompt must be byte-identical across requests"
    );
    // The static instructions genuinely live in the system messages.
    assert!(router_systems[0].contains("skill router"));

    // max_tokens caps are present on both waves.
    let intent_bodies: Vec<&String> = bodies
        .iter()
        .filter(|b| system_of(b).map(|s| s.contains("第一波")).unwrap_or(false))
        .collect();
    let router_bodies: Vec<&String> = bodies
        .iter()
        .filter(|b| {
            system_of(b)
                .map(|s| s.contains("skill router"))
                .unwrap_or(false)
        })
        .collect();
    assert!(intent_bodies.is_empty());
    assert_eq!(max_tokens_of(router_bodies[0]), Some(400));
    // temperature 0 for deterministic + cache-friendly routing.
    for b in bodies.iter() {
        let v: serde_json::Value = serde_json::from_str(b).unwrap();
        assert_eq!(v.get("temperature").and_then(|t| t.as_i64()), Some(0));
    }
}

#[test]
fn fast_router_message_carries_task_anchor_once() {
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "处理 alpha 任务 alpha 工具");
    let mock = RecordingMock::start();
    env.write_config(mock.base_url());

    // Fast has no Stage-1; the bounded task anchor reaches the sole router call.
    let unique = "帮我处理 alpha 任务 UNIQUEMARK";
    let _ = recommend_stdin(&env, "first alpha turn", "once-session");
    let _ = recommend_stdin(&env, unique, "once-session");

    let bodies = mock.bodies();
    let intent_user = bodies
        .iter()
        .filter_map(|b| {
            if max_tokens_of(b) == Some(400) {
                user_of(b)
            } else {
                None
            }
        })
        .next_back()
        .expect("a router user message");

    assert_eq!(
        intent_user.matches("UNIQUEMARK").count(),
        1,
        "task anchor must appear exactly once in Fast router msg:\n{intent_user}"
    );
    assert!(intent_user.contains("当前任务锚点"));
    assert!(intent_user.contains("检索意图 / expansion"));
}

#[test]
fn same_skill_can_be_recommended_on_consecutive_turns() {
    // Session no-repeat suppression removed: the same skill re-appears in the
    // hook output on a follow-up turn, and the LLM input carries no
    // already-routed / 参考池 block.
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "处理 alpha 任务 alpha 工具");
    let mock = RecordingMock::start();
    env.write_config(mock.base_url());

    let out1 = recommend_stdin(&env, "帮我处理 alpha 任务", "norepeat-session");
    let out2 = recommend_stdin(&env, "再处理一下 alpha 任务", "norepeat-session");

    assert!(out1.contains("alpha-skill"), "turn 1 must surface skill");
    assert!(
        out2.contains("alpha-skill"),
        "turn 2 must surface the SAME skill (no suppression):\n{out2}"
    );
    assert!(!out2.contains("参考池"), "no 已推参考池 block:\n{out2}");
    assert!(
        !out2.contains("本会话已推过"),
        "no already-routed block:\n{out2}"
    );

    let bodies = mock.bodies();
    for b in bodies.iter() {
        if let Some(u) = user_of(b) {
            assert!(
                !u.contains("本会话已推过") && !u.contains("ALREADY_ROUTED"),
                "no already-routed block may reach the LLM input:\n{u}"
            );
        }
    }
}

#[test]
fn user_messages_and_hook_output_stay_under_token_budget() {
    // Fixed fixture (short prompt + 1 candidate) → absolute char ceilings.
    // These are hard upper bounds, not "smaller than before" — a regression
    // that re-bloats the messages trips them.
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "处理 alpha 任务 alpha 工具");
    let mock = RecordingMock::start();
    env.write_config(mock.base_url());

    let hook = recommend_stdin(&env, "帮我处理 alpha 任务", "budget-session");

    let bodies = mock.bodies();
    let intent_user = bodies
        .iter()
        .filter_map(|b| {
            if max_tokens_of(b) == Some(400) {
                user_of(b)
            } else {
                None
            }
        })
        .next()
        .expect("Fast router user msg");
    let router_user = bodies
        .iter()
        .filter_map(|b| {
            if system_of(b)
                .map(|s| s.contains("skill router"))
                .unwrap_or(false)
            {
                user_of(b)
            } else {
                None
            }
        })
        .next()
        .expect("Stage-2 user msg");

    assert!(
        intent_user.chars().count() < 1200,
        "Fast router user msg too large ({} chars):\n{intent_user}",
        intent_user.chars().count()
    );
    assert!(
        router_user.chars().count() < 1200,
        "Stage-2 user msg too large ({} chars):\n{router_user}",
        router_user.chars().count()
    );
    assert!(
        hook.chars().count() < 1200,
        "hook output too large ({} chars):\n{hook}",
        hook.chars().count()
    );
    // Protocol still intact after slimming.
    assert!(hook.contains("runai-client activate"));
    assert!(hook.contains("runai-client feedback"));
    assert!(hook.contains("runai-client file"));
}

#[test]
fn long_prompt_fast_anchor_keeps_tail_request_bounded() {
    // Fast skips Stage-1 and derives a compact task anchor from the tail.
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "处理 alpha 任务 alpha 工具");
    let mock = RecordingMock::start();
    env.write_config(mock.base_url());

    let filler = "旧对话粘贴内容 pasted old context ".repeat(300);
    let prompt = format!("HEADMARK 开头框架{filler}MIDDLEMARK{filler}TAILMARK 处理 alpha 任务");
    assert!(
        prompt.chars().count() > 5000,
        "fixture must be a long paste"
    );
    let _ = recommend_stdin(&env, &prompt, "trunc-session");

    let bodies = mock.bodies();
    let intent_user = bodies
        .iter()
        .filter_map(|b| {
            if max_tokens_of(b) == Some(400) {
                user_of(b)
            } else {
                None
            }
        })
        .next()
        .expect("Fast router user msg");

    assert!(
        !intent_user.contains("HEADMARK"),
        "long pasted head should not dominate Fast anchor:\n{intent_user}"
    );
    assert!(
        intent_user.contains("TAILMARK"),
        "trailing real request kept in Fast anchor:\n{intent_user}"
    );
    assert!(intent_user.chars().count() < 1200);
    assert!(
        !intent_user.contains("MIDDLEMARK"),
        "the elided middle must be gone from Stage-1 input:\n{intent_user}"
    );
}

#[test]
fn fast_router_message_carries_raw_task_anchor_and_deterministic_intent() {
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "处理 alpha 任务 alpha 工具");
    let mock = RecordingMock::start();
    env.write_config(mock.base_url());

    // "SENTINELZZZ" appears only here; "处理"/"include_terms" come from the
    // mock's Stage-1 reply, not from this raw text.
    let _ = recommend_stdin(
        &env,
        "SENTINELZZZ 麻烦你搞一下那个 alpha 的事情",
        "split-session",
    );

    let bodies = mock.bodies();
    let stage2_user = bodies
        .iter()
        .filter_map(|b| {
            if system_of(b)
                .map(|s| s.contains("skill router"))
                .unwrap_or(false)
            {
                user_of(b)
            } else {
                None
            }
        })
        .next()
        .expect("Stage-2 user msg");

    assert!(stage2_user.contains("SENTINELZZZ"));
    assert!(stage2_user.contains("当前任务锚点"));
    assert!(stage2_user.contains("检索意图 / expansion"));
    assert!(
        !stage2_user.contains("用户当前 prompt"),
        "the old raw-prompt header must be gone from Stage-2:\n{stage2_user}"
    );
}

#[test]
fn fast_omits_large_context_blocks_even_when_available() {
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "处理 alpha 任务 alpha 工具");
    let mock = RecordingMock::start();
    env.write_config(mock.base_url());
    std::fs::create_dir_all("/tmp/arch-proj").unwrap();
    std::fs::write("/tmp/arch-proj/CLAUDE.md", "FAST_PROJECT_CONTEXT_SENTINEL").unwrap();
    let _ = recommend_stdin(&env, "帮我处理 alpha 任务", "fast-no-large-blocks");
    let router_user = mock
        .bodies()
        .iter()
        .filter_map(|body| {
            if max_tokens_of(body) == Some(400) {
                user_of(body)
            } else {
                None
            }
        })
        .next()
        .unwrap();
    assert!(!router_user.contains("FAST_PROJECT_CONTEXT_SENTINEL"));
    assert!(!router_user.contains("最近对话"));
}

#[test]
fn output_and_quantity_rules_live_in_system_not_user_message() {
    // The 输出格式 + 候选数量 rule blocks are static — they belong in the fixed
    // Stage-2 system prompt (prefix-cache friendly), not re-sent per request in
    // the user message. The numeric hard cap is gone entirely.
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "处理 alpha 任务 alpha 工具");
    let mock = RecordingMock::start();
    env.write_config(mock.base_url());

    let _ = recommend_stdin(&env, "帮我处理 alpha 任务", "rules-session");

    let bodies = mock.bodies();
    let router_user = bodies
        .iter()
        .filter_map(|b| {
            if system_of(b)
                .map(|s| s.contains("skill router"))
                .unwrap_or(false)
            {
                user_of(b)
            } else {
                None
            }
        })
        .next()
        .expect("Stage-2 user msg");
    let router_system = bodies
        .iter()
        .filter_map(|b| system_of(b))
        .find(|s| s.contains("skill router"))
        .expect("Stage-2 system msg");

    assert!(router_user.contains("只返回 JSON"));
    assert!(
        !router_user.contains("硬上限"),
        "numeric hard cap must not be in the user message:\n{router_user}"
    );
    assert!(
        !router_user.contains("最小充分集合"),
        "quantity rules must not be in the user message:\n{router_user}"
    );
    assert!(router_system.contains("输出"));
    assert!(
        router_system.contains("最小充分集合"),
        "quantity rules must live in the system prompt"
    );
    assert!(
        !router_system.contains("硬上限"),
        "numeric hard cap must be removed everywhere"
    );
}

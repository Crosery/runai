#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use runai::core::db::{RouterEvent, SkillAiIndex};
use runai::core::manager::SkillManager;
use runai::core::recommend::{
    Provider, RecommendConfig, SessionMode, recommend_for_user_with_client,
    runai_session_id_from_native,
};

struct MockLlm {
    base_url: String,
    calls: Arc<AtomicUsize>,
    bodies: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockLlm {
    fn start(responses: Vec<&'static str>) -> Self {
        Self::start_with_statuses(responses, Vec::new())
    }

    fn start_with_statuses(responses: Vec<&'static str>, statuses: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let calls_t = calls.clone();
        let bodies_t = bodies.clone();
        let stop_t = stop.clone();
        let handle = thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                        let body = read_body(&mut stream);
                        bodies_t.lock().unwrap().push(body);
                        let idx = calls_t.fetch_add(1, Ordering::SeqCst);
                        let content = responses.get(idx).copied().unwrap_or("NONE");
                        let status = statuses.get(idx).copied().unwrap_or(200);
                        let body = serde_json::json!({
                            "choices": [{"message": {"content": content}}],
                            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                        })
                        .to_string();
                        let response = format!(
                            "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            if status == 200 { "OK" } else { "Error" },
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            base_url,
            calls,
            bodies,
            stop,
            handle: Some(handle),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies.lock().unwrap().clone()
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_body(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buf = [0_u8; 8192];
    let header_end = loop {
        match stream.read(&mut buf) {
            Ok(0) => return String::new(),
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
            }
            Err(_) => return String::new(),
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_lowercase();
    let len = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while bytes.len().saturating_sub(body_start) < len {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&bytes[body_start..]).to_string()
}

fn setup(
    mock: &MockLlm,
    routing_mode: &str,
    search_doc: &str,
) -> (tempfile::TempDir, SkillManager) {
    let root = tempfile::tempdir().unwrap();
    let mgr = SkillManager::with_base(root.path().join("data")).unwrap();
    let skill_dir = mgr.paths().skills_dir().join("target-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: target-skill\ndescription: target\n---\n# target\n",
    )
    .unwrap();
    mgr.register_local_skill("target-skill").unwrap();
    mgr.db()
        .set_skill_ai_index(
            "target-skill",
            &SkillAiIndex {
                summary: "target".into(),
                search_doc: search_doc.into(),
                router_card: "task: target | triggers: target | inputs: text | outputs: result | not-for: unrelated".into(),
                llm_score: 8,
                ..SkillAiIndex::default()
            },
        )
        .unwrap();
    mgr.db()
        .create_user("u1", "alice", "password", "key", false)
        .unwrap();
    mgr.db()
        .update_user_prefs(
            "u1",
            &serde_json::json!({
                "routing_mode": routing_mode,
                "allow_public_recommend": true,
                "bm25_candidate_limit": 30
            })
            .to_string(),
        )
        .unwrap();
    RecommendConfig {
        enabled: true,
        provider: Provider::OpenaiCompat,
        base_url: mock.base_url.clone(),
        model: "mock".into(),
        api_key: "test".into(),
        min_prompt_len: 0,
        summary_lang_confirmed: true,
        ..RecommendConfig::default()
    }
    .save(mgr.paths())
    .unwrap();
    (root, mgr)
}

#[test]
fn fast_default_uses_at_most_one_llm_call_and_short_id_whitelist() {
    let mock = MockLlm::start(vec![
        r#"{"mode":"exclusive","selected":["C01","C99"],"reasoning":"C01 direct"}"#,
    ]);
    let (_root, mgr) = setup(&mock, "fast", "alpha task");
    let decision = recommend_for_user_with_client(
        &mgr,
        "alpha task",
        None,
        Some("fast-session"),
        Some("/tmp/project"),
        Some("u1"),
        Some("pi"),
    )
    .unwrap();
    assert_eq!(mock.calls(), 1, "Fast 每轮至多调用一次路由 LLM");
    assert_eq!(
        decision
            .skills
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["target-skill"],
        "C99 不在当前候选白名单，必须被拒绝"
    );
    let bodies = mock.bodies();
    assert!(bodies[0].contains("C01"), "候选必须使用稳定短 ID");
}

#[test]
fn precise_keeps_original_anchor_parallel_to_stage1_expansion_and_calls_twice() {
    let mock = MockLlm::start(vec![
        "intent: synonym only\ninclude_terms: synonym",
        r#"{"mode":"exclusive","selected":["C01"]}"#,
    ]);
    let (_root, mgr) = setup(&mock, "precise", "ORIGINALANCHOR");
    let decision = recommend_for_user_with_client(
        &mgr,
        "Please process ORIGINALANCHOR now",
        None,
        Some("precise-session"),
        None,
        Some("u1"),
        Some("claude"),
    )
    .unwrap();
    assert_eq!(mock.calls(), 2, "Precise 保留 expansion + router 两次调用");
    assert_eq!(decision.skills.len(), 1);
    let bodies = mock.bodies();
    assert!(
        bodies.last().unwrap().contains("ORIGINALANCHOR"),
        "Stage-2 必须同时看到原始任务锚点，Stage-1 改写不能覆盖原证据"
    );
}

#[test]
fn precise_stage1_transport_failure_counts_attempt_then_router_success() {
    let mock = MockLlm::start_with_statuses(
        vec![
            "intent unavailable",
            r#"{"mode":"exclusive","selected":["C01"]}"#,
        ],
        vec![500, 200],
    );
    let (_root, mgr) = setup(&mock, "precise", "alpha task");
    let decision = recommend_for_user_with_client(
        &mgr,
        "alpha task",
        None,
        Some("precise-stage1-error"),
        None,
        Some("u1"),
        Some("claude"),
    )
    .unwrap();
    assert_eq!(decision.skills.len(), 1);
    assert_eq!(mock.calls(), 2);
    let event = mgr.db().router_recent_events(1).unwrap().pop().unwrap();
    assert_eq!(event.intent_status, "fallback");
    assert!(event.intent_error_msg.is_some());
    assert_eq!(event.llm_call_count, 2);
}

#[test]
fn precise_records_stage1_raw_and_cleaned_outputs() {
    let mock = MockLlm::start(vec![
        "```\nintent: expanded alpha\ninclude_terms: alpha synonym\n```",
        r#"{"mode":"exclusive","selected":["C01"]}"#,
    ]);
    let (_root, mgr) = setup(&mock, "precise", "alpha task");
    let _ = recommend_for_user_with_client(
        &mgr,
        "alpha task",
        None,
        Some("precise-raw"),
        None,
        Some("u1"),
        Some("claude"),
    )
    .unwrap();
    let event = mgr.db().router_recent_events(1).unwrap().pop().unwrap();
    assert!(event.intent_llm_raw_output.contains("```"));
    assert!(!event.intent_llm_output.contains("```"));
    assert!(event.intent_llm_output.contains("expanded alpha"));
    assert_eq!(event.llm_call_count, 2);
}

#[test]
fn precise_bounds_remote_cwd_and_client_kind_before_both_stages() {
    let mock = MockLlm::start(vec![
        "intent: alpha task\ninclude_terms: alpha",
        r#"{"mode":"exclusive","selected":["C01"]}"#,
    ]);
    let (_root, mgr) = setup(&mock, "precise", "alpha task");
    let cwd = format!("CWD_HEAD/{}CWD_TAIL", "x".repeat(10_000));
    let client = format!("CLIENT_HEAD-{}-CLIENT_TAIL", "y".repeat(10_000));
    recommend_for_user_with_client(
        &mgr,
        "alpha task",
        None,
        Some("bounded-dynamic-fields"),
        Some(&cwd),
        Some("u1"),
        Some(&client),
    )
    .unwrap();
    let bodies = mock.bodies();
    assert_eq!(bodies.len(), 2);
    for body in &bodies {
        assert!(body.contains("CWD_HEAD"));
        assert!(body.contains("CWD_TAIL"));
        assert!(!body.contains(&"x".repeat(1000)));
    }
    assert!(bodies[0].contains("CLIENT_HEAD"));
    assert!(bodies[0].contains("CLIENT_TAIL"));
    assert!(!bodies[0].contains(&"y".repeat(1000)));
    assert!(bodies.iter().all(|body| body.len() < 20_000));
}

#[test]
fn precise_conversation_history_has_turn_item_and_total_caps() {
    let mock = MockLlm::start(vec![
        "intent: alpha task\ninclude_terms: alpha",
        r#"{"mode":"exclusive","selected":["C01"]}"#,
    ]);
    let (_root, mgr) = setup(&mock, "precise", "alpha task");
    let mut cfg = RecommendConfig::load(mgr.paths()).unwrap();
    cfg.session_mode = SessionMode::Conversation;
    cfg.session_history_limit = 100_000;
    cfg.save(mgr.paths()).unwrap();
    let history_session = runai_session_id_from_native(Some("u1"), "history-cap").unwrap();
    for ts in 1..=12 {
        mgr.db()
            .insert_router_event(&RouterEvent {
                ts,
                session_id: history_session.clone(),
                status: "ok".into(),
                llm_input: format!("HISTORY_USER_{ts}_{}", "u".repeat(65_536)),
                llm_raw_response: format!("HISTORY_ASSISTANT_{ts}_{}", "a".repeat(65_536)),
                ..RouterEvent::default()
            })
            .unwrap();
    }
    recommend_for_user_with_client(
        &mgr,
        "alpha task",
        None,
        Some("history-cap"),
        None,
        Some("u1"),
        Some("claude"),
    )
    .unwrap();
    let router_body: serde_json::Value = serde_json::from_str(&mock.bodies()[1]).unwrap();
    let messages = router_body["messages"].as_array().unwrap();
    assert_eq!(
        messages.len(),
        6,
        "system + 2 recent history pairs within total budget + current user"
    );
    let history = &messages[1..messages.len() - 1];
    assert!(
        history
            .iter()
            .all(|message| { message["content"].as_str().unwrap().chars().count() <= 1200 })
    );
    let history_chars: usize = history
        .iter()
        .map(|message| message["content"].as_str().unwrap().chars().count())
        .sum();
    assert!(history_chars <= 6000);
    assert!(history.iter().any(|message| {
        message["content"]
            .as_str()
            .unwrap()
            .contains("HISTORY_USER_12")
    }));
    assert!(history.iter().any(|message| {
        message["content"]
            .as_str()
            .unwrap()
            .contains("HISTORY_USER_11")
    }));
    assert!(!history.iter().any(|message| {
        message["content"]
            .as_str()
            .unwrap()
            .contains("HISTORY_USER_10")
    }));
}

#[test]
fn fast_cross_language_zero_lexical_overlap_uses_bounded_fallback_pool() {
    let mock = MockLlm::start(vec![r#"{"mode":"exclusive","selected":["C01"]}"#]);
    let (_root, mgr) = setup(
        &mock,
        "fast",
        "task: send an instant message | triggers: 飞书消息 lark messaging",
    );
    let decision = recommend_for_user_with_client(
        &mgr,
        "给飞书联系人发一条即时消息",
        None,
        Some("cross-lang"),
        None,
        Some("u1"),
        Some("codex"),
    )
    .unwrap();
    assert_eq!(mock.calls(), 1);
    assert_eq!(
        decision.skills.len(),
        1,
        "结构化 triggers 中的跨语言词必须提供真实检索证据"
    );
}

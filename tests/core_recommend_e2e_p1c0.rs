//! End-to-end regression for `core::recommend` (PLANNING test plan 5.7).
//!
//! These tests spawn the workspace-built `runai` binary
//! (`env!("CARGO_BIN_EXE_runai")`) against an isolated `HOME` +
//! `RUNE_DATA_DIR` sandbox — production `~/.runai/` and the four CLI
//! homes are never touched.
//!
//! Coverage map vs. plan §5.7:
//!   1. `recommend_disabled_no_llm_call`   → [`disabled_recommend_emits_no_router_event`]
//!      + [`disabled_recommend_writes_bootstrap_seen_then_stays_silent`]
//!   6. `recommend_hook_output_format_safe` → [`mock_llm_recommend_emits_runai_client_activation`]
//!   7. `recommend_router_telemetry_persisted_even_on_error`
//!      → [`mock_llm_http_500_persists_error_router_event`]
//!
//! Plan tests 2/3/4/5 either depend on internals (`BM25_MIN_POSITIVE_HITS` env)
//! that are awkward to drive over the binary boundary, or on multi-user
//! (`recommend_for_user`, `UserPrefs.prompt_injection_flags`) — neither exists
//! in the v0.11.0-beta.5 single-user release branch this file is written
//! against. They are intentionally skipped to keep the regression hard to
//! flake.
#![cfg(not(target_os = "windows"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

const RUNAI_BIN: &str = env!("CARGO_BIN_EXE_runai");

// ─── Helpers ────────────────────────────────────────────────────────────────

struct TestEnv {
    home: TempDir,
}

impl TestEnv {
    fn new() -> Self {
        let home = tempfile::tempdir().expect("create tmp HOME");
        // Pre-create the managed dirs so the binary doesn't have to bootstrap
        // them — and so a stray `rm -rf` in this code can't reach outside.
        std::fs::create_dir_all(home.path().join(".runai/skills"))
            .expect("pre-create managed skills dir");
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

    fn config_path(&self) -> PathBuf {
        self.runai_dir().join("config.toml")
    }

    fn bootstrap_seen_path(&self) -> PathBuf {
        self.runai_dir().join(".bootstrap-seen")
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
        // Make sure the DB knows about it so the candidate set isn't empty.
        let out = self.run(&["scan"]);
        assert!(
            out.status.success(),
            "scan must succeed (stderr={})",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Spawn the installed binary with all relevant env vars pinned to the
    /// sandbox. `RUNAI_NO_AUTOSPAWN=1` prevents the binary from trying to
    /// boot a background dashboard server (would leak into other tests on
    /// the same machine).
    fn run(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new(RUNAI_BIN);
        cmd.args(args)
            .env("HOME", self.home())
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env("RUNE_DATA_DIR", self.runai_dir())
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .env_remove("CLAUDE_SESSION_ID")
            .env_remove("RUNAI_RECOMMEND_API_KEY");
        cmd.output().expect("runai binary spawn")
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
            .expect("runai binary spawn");
        if let Some(mut s) = child.stdin.take() {
            s.write_all(stdin.as_bytes()).unwrap();
        }
        child.wait_with_output().expect("wait runai")
    }

    /// Count rows in `router_events`. Returns 0 if the DB file doesn't
    /// exist yet (i.e. the recommend short-circuit ran without ever
    /// opening the DB). 0 is the truthful answer either way.
    fn router_events_count(&self) -> i64 {
        if !self.db_path().exists() {
            return 0;
        }
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.query_row(
            "SELECT COUNT(*) FROM router_events",
            rusqlite::params![],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    fn router_event_status_list(&self) -> Vec<String> {
        if !self.db_path().exists() {
            return Vec::new();
        }
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        let mut stmt = conn
            .prepare("SELECT status FROM router_events ORDER BY id ASC")
            .expect("prepare");
        let rows = stmt
            .query_map(rusqlite::params![], |r| r.get::<_, String>(0))
            .expect("query_map");
        rows.filter_map(|r| r.ok()).collect()
    }

    fn router_event_error_msgs(&self) -> Vec<Option<String>> {
        if !self.db_path().exists() {
            return Vec::new();
        }
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        let mut stmt = conn
            .prepare("SELECT error_msg FROM router_events ORDER BY id ASC")
            .expect("prepare");
        let rows = stmt
            .query_map(rusqlite::params![], |r| r.get::<_, Option<String>>(0))
            .expect("query_map");
        rows.filter_map(|r| r.ok()).collect()
    }

    fn router_llm_inputs(&self) -> Vec<String> {
        if !self.db_path().exists() {
            return Vec::new();
        }
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        let mut stmt = conn
            .prepare("SELECT llm_input FROM router_events ORDER BY id ASC")
            .expect("prepare");
        let rows = stmt
            .query_map(rusqlite::params![], |r| r.get::<_, String>(0))
            .expect("query_map");
        rows.filter_map(|r| r.ok()).collect()
    }

    fn router_stage_fields(&self) -> Vec<(String, String, String, String)> {
        if !self.db_path().exists() {
            return Vec::new();
        }
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        let mut stmt = conn
            .prepare(
                "SELECT intent_llm_input, intent_llm_output, intent_status, bm25_candidates_json
                 FROM router_events ORDER BY id ASC",
            )
            .expect("prepare");
        let rows = stmt
            .query_map(rusqlite::params![], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .expect("query_map");
        rows.filter_map(|r| r.ok()).collect()
    }

    fn router_chosen_skills_jsons(&self) -> Vec<String> {
        if !self.db_path().exists() {
            return Vec::new();
        }
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        let mut stmt = conn
            .prepare("SELECT chosen_skills_json FROM router_events ORDER BY id ASC")
            .expect("prepare");
        let rows = stmt
            .query_map(rusqlite::params![], |r| r.get::<_, String>(0))
            .expect("query_map");
        rows.filter_map(|r| r.ok()).collect()
    }

    fn router_intent_memories(&self) -> Vec<String> {
        if !self.db_path().exists() {
            return Vec::new();
        }
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        let mut stmt = conn
            .prepare("SELECT memory FROM router_intent_memory ORDER BY id ASC")
            .expect("prepare");
        let rows = stmt
            .query_map(rusqlite::params![], |r| r.get::<_, String>(0))
            .expect("query_map");
        rows.filter_map(|r| r.ok()).collect()
    }

    fn write_recommend_config(&self, body: &str) {
        // The binary expects `config.toml` to live under `<data_dir>/config.toml`.
        std::fs::write(self.config_path(), body).expect("write config.toml");
    }

    fn force_ai_index(&self, name: &str, search_doc: &str, router_card: &str, llm_score: i64) {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        conn.execute(
            "INSERT INTO resource_ai_summary
             (owner_user_id, name, summary, updated_at, llm_score, search_doc, router_card, source_hash, prompt_hash, format_key)
             VALUES ('', ?1, ?2, ?3, ?4, ?5, ?6, 'test', 'test', 'test')
             ON CONFLICT(owner_user_id, name) DO UPDATE SET
                summary = excluded.summary,
                updated_at = excluded.updated_at,
                llm_score = excluded.llm_score,
                search_doc = excluded.search_doc,
                router_card = excluded.router_card",
            rusqlite::params![
                name,
                router_card,
                chrono::Utc::now().timestamp(),
                llm_score,
                search_doc,
                router_card,
            ],
        )
        .expect("force ai index");
    }

    /// Seed `sessions` distinct chosen-sessions for `skill` in `router_events`
    /// (each choosing the skill), and mark `adopted` of them adopted via
    /// `router_session_adoptions`. Drives `skill_router_stats` so the
    /// `[adopt:NN%]` marker renders on the next real recommend.
    fn seed_router_history(&self, skill: &str, sessions: usize, adopted: usize) {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        let now = chrono::Utc::now().timestamp();
        let chosen = serde_json::json!([skill]).to_string();
        for i in 0..sessions {
            let sid = format!("seed-sess-{i}");
            conn.execute(
                "INSERT INTO router_events
                    (ts, provider, model, session_id, chosen_skills_json, bm25_candidates_json, llm_input, status)
                 VALUES (?1, 'mock', 'mock', ?2, ?3, ?3, '', 'ok')",
                rusqlite::params![now, sid, chosen],
            )
            .expect("seed router_event");
            if i < adopted {
                conn.execute(
                    "INSERT INTO router_session_adoptions (session_id, skill_name, ts)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![sid, skill, now],
                )
                .expect("seed adoption");
            }
        }
    }

    /// Seed `pos` positive + `neg` negative explicit feedback rows for `skill`
    /// so the `[fb:+P/-N]` marker renders on the next real recommend.
    fn seed_feedback(&self, skill: &str, pos: usize, neg: usize) {
        let conn = rusqlite::Connection::open(self.db_path()).expect("open test db");
        let now = chrono::Utc::now().timestamp();
        for _ in 0..pos {
            conn.execute(
                "INSERT INTO skill_feedback (ts, skill_name, verdict) VALUES (?1, ?2, 1)",
                rusqlite::params![now, skill],
            )
            .expect("seed pos feedback");
        }
        for _ in 0..neg {
            conn.execute(
                "INSERT INTO skill_feedback (ts, skill_name, verdict) VALUES (?1, ?2, -1)",
                rusqlite::params![now, skill],
            )
            .expect("seed neg feedback");
        }
    }
}

// ─── 1. Disabled router path ────────────────────────────────────────────────

#[test]
fn disabled_recommend_emits_no_router_event() {
    // Plan 5.7 #1: when the router is disabled (default), `runai recommend
    // <prompt>` must NOT call the LLM and must NOT insert a row into
    // router_events. The bootstrap guide is the only allowed side effect on
    // first run — we suppress it by pre-creating the `.bootstrap-seen`
    // marker so we get a deterministically empty stdout.
    let env = TestEnv::new();
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    // Sanity: status reflects disabled.
    let status = env.run(&["recommend", "status"]);
    assert!(
        status.status.success(),
        "`recommend status` must succeed even when disabled"
    );
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_out.contains("enabled:        false")
            || status_out.contains("enabled: false")
            || status_out.contains("enabled:\tfalse")
            || status_out.contains("enabled:")
                && status_out.split_whitespace().any(|t| t == "false"),
        "status output must mark router as disabled, got: {status_out}"
    );

    let out = env.run(&["recommend", "I want to make a presentation"]);
    assert!(
        out.status.success(),
        "disabled recommend must still exit 0 (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // No hook protocol elements may leak.
    assert!(
        !stdout.contains("curl"),
        "disabled router must not render hook activation, got: {stdout}"
    );
    assert!(
        !stdout.contains("/skills/get/"),
        "disabled router must not render the activation URL, got: {stdout}"
    );
    // Telemetry table must stay empty — recommend() short-circuits before
    // `insert_router_event`.
    assert_eq!(
        env.router_events_count(),
        0,
        "disabled router must NOT insert router_events rows"
    );
}

#[test]
fn disabled_recommend_writes_bootstrap_seen_then_stays_silent() {
    // Side test for plan 5.7 #1: on the very first disabled run the binary
    // prints a bootstrap guide and writes a marker; the second run must be
    // silent. Verifies the no-op path is *also* idempotent.
    let env = TestEnv::new();
    assert!(
        !env.bootstrap_seen_path().exists(),
        "precondition: bootstrap-seen marker absent"
    );

    let first = env.run(&["recommend", "trigger bootstrap"]);
    assert!(first.status.success());
    let first_stdout = String::from_utf8_lossy(&first.stdout);
    assert!(
        env.bootstrap_seen_path().exists(),
        "first disabled run must persist .bootstrap-seen marker"
    );
    // Guide is human-prose so we just check it's non-trivial. The actual
    // string lives in `bootstrap_guide()` and mentions `runai recommend setup`.
    assert!(
        first_stdout.contains("recommend setup") || !first_stdout.is_empty(),
        "first run should emit bootstrap guide pointing at setup, got: {first_stdout}"
    );

    let second = env.run(&["recommend", "again"]);
    assert!(second.status.success());
    let second_stdout = String::from_utf8_lossy(&second.stdout);
    assert!(
        second_stdout.is_empty(),
        "second disabled run must be silent (marker present), got: {second_stdout}"
    );

    // Either way: zero router_events. Disabled means disabled.
    assert_eq!(
        env.router_events_count(),
        0,
        "no router_events allowed on disabled path"
    );
}

// ─── Mini HTTP mock for the OpenAI-compatible router endpoint ───────────────

/// Behavior selector for `MockLlm`.
#[derive(Clone, Copy)]
enum MockBehavior {
    /// 200 OK + canned OpenAI-compat response that picks one skill name.
    OkPickSkill(&'static str, &'static str), // (skill_name, reasoning)
    /// 200 OK responses returned in request order. Used for stage 1 intent
    /// recognition followed by stage 2 router selection.
    Sequence(&'static [&'static str]),
    /// HTTP 500 with an error body.
    InternalError,
}

/// Tiny single-shot HTTP server that speaks just enough of the OpenAI
/// chat-completions wire format to drive `recommend`. Spawned on a free
/// loopback port. Drops after the test by virtue of the listener+thread
/// going out of scope.
struct MockLlm {
    addr: String,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    handle: Option<thread::JoinHandle<()>>,
}

impl MockLlm {
    fn start(behavior: MockBehavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1");
        listener
            .set_nonblocking(true)
            .expect("non-blocking listener");
        let addr = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_thread = requests.clone();
        let handle = thread::spawn(move || {
            // We accept up to a handful of connections to be safe; the
            // binary may issue one request, but we don't want to hang the
            // thread if it makes a probe call.
            let started = std::time::Instant::now();
            let mut request_idx = 0usize;
            while !stop_for_thread.load(Ordering::SeqCst)
                && started.elapsed() < Duration::from_secs(30)
            {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        // Read the request roughly — we don't care, we
                        // just need to consume the header bytes so the
                        // client doesn't see a RST. 8 KiB is plenty for
                        // headers; the body may be larger but the client
                        // doesn't wait for us to read everything before
                        // we write a response.
                        let mut buf = [0u8; 8192];
                        let _ = stream.read(&mut buf);
                        requests_for_thread.fetch_add(1, Ordering::SeqCst);
                        let resp = match behavior {
                            MockBehavior::OkPickSkill(skill, reasoning) => {
                                // OpenAI-compatible /chat/completions shape.
                                // The router parser strips ``` fences and
                                // leading dashes via `parse_lines`, so plain
                                // text is fine. First line is the mode tag
                                // ("EXCLUSIVE" / "COMPATIBLE"), second is
                                // reasoning, then skill names one per line.
                                let content =
                                    format!("EXCLUSIVE\nreasoning: {reasoning}\n{skill}\n");
                                openai_response(&content)
                            }
                            MockBehavior::Sequence(contents) => {
                                let content = contents
                                    .get(request_idx)
                                    .or_else(|| contents.last())
                                    .copied()
                                    .unwrap_or("");
                                request_idx += 1;
                                openai_response(content)
                            }
                            MockBehavior::InternalError => {
                                let body_str = "{\"error\":\"mock-500\"}";
                                format!(
                                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                    body_str.len(),
                                    body_str
                                )
                            }
                        };
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                        // Close the connection so the client sees EOF.
                        drop(stream);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            stop,
            requests,
            handle: Some(handle),
        }
    }

    fn base_url(&self) -> &str {
        &self.addr
    }

    /// How many HTTP requests the mock has served so far.
    fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for MockLlm {
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
        "created": 0,
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 5,
            "total_tokens": 15
        }
    });
    let body_str = body.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body_str.len(),
        body_str
    )
}

fn plant_android_debug_fixture(env: &TestEnv) {
    env.plant_skill(
        "android-cli",
        "Android ADB logcat emulator 模拟器 安卓调试 环境诊断",
    );
    env.force_ai_index(
        "android-cli",
        "android-cli Android ADB logcat emulator 模拟器 安卓调试 环境诊断 sdk avd",
        "android-cli: task: 管理 Android 开发命令行工具，包括 adb/logcat/模拟器调试 triggers: android, adb, logcat, emulator, 模拟器 inputs: 设备或模拟器 outputs: 环境诊断 not-for: ",
        8,
    );
    env.plant_skill(
        "emulator-launch",
        "启动 Android Emulator AVD 模拟器 cold boot GPU",
    );
    env.force_ai_index(
        "emulator-launch",
        "emulator-launch 启动 Android Emulator AVD 模拟器 cold boot GPU",
        "emulator-launch: task: 启动指定 Android 模拟器或 AVD triggers: emulator, AVD, 模拟器启动 inputs: AVD 名称 outputs: 模拟器启动报告 not-for: 安装 app",
        8,
    );
    env.plant_skill(
        "ktv-car-debug-suite",
        "KTVLite 车机 WebView H5 android emulator adb 白屏 调试套件",
    );
    env.force_ai_index(
        "ktv-car-debug-suite",
        "ktv-car-debug-suite KTVLite 车机 WebView H5 android emulator adb 白屏 调试套件 真车 理想 SS4",
        "ktv-car-debug-suite: task: KTV 车机 WebView/H5 调试套件 triggers: KTV, 车机, WebView, H5, emulator, adb inputs: KTV 车机场景 outputs: 车机 H5 调试链路 not-for: 普通 Android 模拟器调试, 通用 adb/logcat",
        10,
    );
}

fn plant_image_regeneration_fixture(env: &TestEnv) {
    env.plant_skill(
        "generate-image",
        "解决生成图片任务，如绘图、插画、海报、或基于参考图进行改图",
    );
    env.force_ai_index(
        "generate-image",
        "task: 生成或编辑图片 triggers: 生图 画图 参考图 reference image img2img 图生图 inputs: 提示词 参考图片 outputs: PNG 图像 not-for: 视频生成",
        "generate-image: task: 解决生成图片任务，支持参考图改图和图生图 triggers: reference image, img2img, 生图 inputs: 提示词, 参考图片 outputs: PNG not-for: 视频生成",
        9,
    );
    env.plant_skill(
        "mmx-cli",
        "通过 MiniMax AI 平台生成文本、图片、视频、语音和音乐内容",
    );
    env.force_ai_index(
        "mmx-cli",
        "task: 使用 MiniMax 生成图片和多模态内容 triggers: 文生图 图生图 图片生成 inputs: 提示词 图片文件 outputs: 图片文件 not-for: 代码调试",
        "mmx-cli: task: 通过 MiniMax 平台生成图片，支持图片文件输入 triggers: 文生图, 图生图 inputs: 提示词, 图片文件 outputs: 图片",
        8,
    );
    env.plant_skill(
        "imagegen",
        "生成或编辑栅格图像（照片、插图、精灵、产品矢量背景等）",
    );
    env.force_ai_index(
        "imagegen",
        "task: 生成或编辑栅格图像 triggers: image generation create image edit image illustration inputs: 图片描述 约束 outputs: 图像文件 not-for: 视频",
        "imagegen: task: 生成或编辑栅格图像 triggers: image generation, edit image inputs: 图片描述, 约束 outputs: 图像文件",
        7,
    );
    env.plant_skill(
        "interview-script",
        "生成基于 JTBD 的用户访谈脚本，挖掘过去真实行为和替代方案",
    );
    env.force_ai_index(
        "interview-script",
        "task: 生成用户访谈脚本 triggers: 访谈 JTBD 用户调研 inputs: 产品想法 outputs: 访谈脚本 not-for: 图片生成, 参考图改图",
        "interview-script: task: 生成用户访谈脚本 not-for: 图片生成, 参考图改图",
        9,
    );
}

fn enable_recommend_config(env: &TestEnv, base_url: &str) {
    // Note: `provider = "openai-compat"` matches the kebab-case serde rename.
    let body = format!(
        r#"[recommend]
enabled = true
provider = "openai-compat"
base_url = "{base_url}"
model = "mock-model"
api_key = "test-key"
top_k = 8
min_prompt_len = 0
summary_lang = "zh"
session_mode = "oneshot"
session_history_limit = 0
"#
    );
    env.write_recommend_config(&body);
}

// ─── 6. Hook output format safety (real router round-trip) ──────────────────

#[test]
fn mock_llm_recommend_emits_runai_client_activation() {
    // Plan 5.7 #6: when the router returns a real skill name, the binary's
    // hook stdout must use the unified curl-based activation protocol and
    // must not leak filesystem paths. We exercise the full pipeline:
    //   stdin JSON → recommend() → call_openai_compat() → mock HTTP →
    //   format_for_hook_full() → stdout
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "test skill alpha");
    // Suppress first-run bootstrap noise.
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    let mock = MockLlm::start(MockBehavior::OkPickSkill(
        "alpha-skill",
        "matches the user task",
    ));
    enable_recommend_config(&env, mock.base_url());

    // Drive via positional arg (no stdin parsing path). The CLI handler
    // skips transcript loading in this mode so we have a clean LLM call.
    let out = env.run(&["recommend", "i need an alpha"]);
    assert!(
        out.status.success(),
        "recommend with mock LLM must exit 0 (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    // The hook protocol now mandates a `runai-client activate` invocation
    // (PLANNING §1.3 activation/feedback protocol) — NOT a curl against
    // /skills/get. The agent-facing activation is client-mediated.
    assert!(
        stdout.contains("runai-client activate"),
        "hook output must include a runai-client activate command, got:\n{stdout}"
    );
    assert!(
        stdout.contains("runai-client feedback"),
        "hook output must include the feedback protocol command, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("curl -s -X POST"),
        "hook output must NOT use curl for activation, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("/skills/get/"),
        "hook output must NOT reference /skills/get/, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("/feedback\""),
        "hook output must NOT use bare curl /feedback, got:\n{stdout}"
    );
    // The chosen skill name MUST appear in the candidate list.
    assert!(
        stdout.contains("alpha-skill"),
        "hook output must name the chosen skill, got:\n{stdout}"
    );
    // Safety: NO filesystem paths and NO SKILL.md body may leak through.
    assert!(
        !stdout.contains(".runai/skills/"),
        "hook output must not reveal data-dir paths, got:\n{stdout}"
    );
    assert!(
        !stdout.contains(env.runai_dir().display().to_string().as_str()),
        "hook output must not reveal the absolute data dir, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("# alpha-skill\n"),
        "hook output must not inline SKILL.md body, got:\n{stdout}"
    );
    // Mode tag must be rendered (unified template uses {MODE}).
    let lower = stdout.to_lowercase();
    assert!(
        lower.contains("exclusive") || lower.contains("compatible"),
        "hook output must include a mode tag, got:\n{stdout}"
    );

    // Telemetry: exactly one event, status="ok".
    let statuses = env.router_event_status_list();
    assert_eq!(
        statuses.len(),
        1,
        "exactly one router_event row expected, got {:?}",
        statuses
    );
    assert_eq!(statuses[0], "ok", "successful LLM call → status=ok");
}

#[test]
fn stdin_json_client_kind_cwd_and_session_memory_feed_router_input() {
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "test skill alpha");
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    let mock = MockLlm::start(MockBehavior::Sequence(&[
        "EXCLUSIVE\nreasoning: matches the current intent\nalpha-skill\n",
        "EXCLUSIVE\nreasoning: matches the current intent\nalpha-skill\n",
    ]));
    enable_recommend_config(&env, mock.base_url());

    let cwd = env.home().join("project-runai");
    std::fs::create_dir_all(&cwd).unwrap();
    let payload1 = serde_json::json!({
        "prompt": "first runai intent memory alpha",
        "session_id": "native-session-1",
        "client_kind": "codex",
        "cwd": cwd.display().to_string(),
    });
    let out1 = env.run_with_input(&["recommend"], &payload1.to_string());
    assert!(
        out1.status.success(),
        "first stdin recommend must succeed (stderr={})",
        String::from_utf8_lossy(&out1.stderr)
    );

    let payload2 = serde_json::json!({
        "prompt": "second turn keeps alpha context",
        "session_id": "native-session-1",
        "client_kind": "codex",
        "cwd": cwd.display().to_string(),
    });
    let out2 = env.run_with_input(&["recommend"], &payload2.to_string());
    assert!(
        out2.status.success(),
        "second stdin recommend must succeed (stderr={})",
        String::from_utf8_lossy(&out2.stderr)
    );

    let statuses = env.router_event_status_list();
    assert_eq!(statuses, vec!["ok".to_string(), "ok".to_string()]);

    let memories = env.router_intent_memories();
    assert_eq!(memories.len(), 2);
    assert!(!memories[0].is_empty(), "memories={memories:?}");
    assert!(!memories[1].is_empty(), "memories={memories:?}");
    assert!(memories[1].contains("session_memory"));

    let inputs = env.router_llm_inputs();
    assert_eq!(inputs.len(), 2);
    let stage_fields = env.router_stage_fields();
    assert_eq!(stage_fields.len(), 2);
    assert!(stage_fields.iter().all(|fields| fields.0.is_empty()));
    assert!(stage_fields.iter().all(|fields| fields.2 == "skipped-fast"));
    let second = &inputs[1];
    assert!(second.contains("## 当前任务锚点"));
    assert!(second.contains("second turn keeps alpha context"));
    assert!(second.contains("最多 30 个"));
    assert!(!second.contains("CLAUDE_SESSION_ID"));
}

#[test]
fn mock_llm_android_emulator_does_not_emit_ktv_candidate() {
    let env = TestEnv::new();
    plant_android_debug_fixture(&env);
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    let mock = MockLlm::start(MockBehavior::Sequence(&[
        "COMPATIBLE\nreasoning: mock deliberately returns every candidate\nandroid-cli\nemulator-launch\nktv-car-debug-suite\n",
    ]));
    enable_recommend_config(&env, mock.base_url());

    let out = env.run(&["recommend", "帮我调试下安卓模拟器"]);
    assert!(
        out.status.success(),
        "android emulator recommend must succeed (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("android-cli"), "stdout={stdout}");
    assert!(stdout.contains("emulator-launch"), "stdout={stdout}");
    assert!(
        !stdout.contains("ktv-car-debug-suite"),
        "plain Android emulator debug must not emit KTV skill: {stdout}"
    );

    let inputs = env.router_llm_inputs();
    assert_eq!(inputs.len(), 1);
    assert!(inputs[0].contains("| android-cli"));
    assert!(inputs[0].contains("| emulator-launch"));
    assert!(
        !inputs[0].contains("| ktv-car-debug-suite"),
        "KTV candidate must be filtered before LLM input: {}",
        inputs[0]
    );
    let chosen = env.router_chosen_skills_jsons();
    assert_eq!(chosen.len(), 1);
    assert!(chosen[0].contains("android-cli"));
    assert!(chosen[0].contains("emulator-launch"));
    assert!(!chosen[0].contains("ktv-car-debug-suite"));
}

#[test]
fn image_regeneration_reference_prompt_uses_compressed_intent_and_single_direct_match() {
    let env = TestEnv::new();
    plant_image_regeneration_fixture(&env);
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    let mock = MockLlm::start(MockBehavior::Sequence(&[
        "EXCLUSIVE\nreasoning: mock returns multiple image alternatives\ngenerate-image\nmmx-cli\nimagegen\ninterview-script\n",
    ]));
    enable_recommend_config(&env, mock.base_url());

    let payload = serde_json::json!({
        "prompt": "没有用搭子形象的参考图啊你这个，重新生成",
        "session_id": "image-regen-session",
        "client_kind": "pi",
        "cwd": env.home().display().to_string(),
    });
    let out = env.run_with_input(&["recommend"], &payload.to_string());
    assert!(
        out.status.success(),
        "image regeneration recommend must succeed (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );

    let inputs = env.router_llm_inputs();
    assert_eq!(inputs.len(), 1);
    let stage_fields = env.router_stage_fields();
    assert_eq!(stage_fields.len(), 1);
    let (intent_input, intent_output, intent_status, bm25_candidates_json) = &stage_fields[0];
    assert!(intent_input.is_empty());
    assert_eq!(intent_status, "skipped-fast");
    assert!(
        intent_output.contains("image-generation"),
        "{intent_output}"
    );
    assert!(intent_output.contains("参考图"), "{intent_output}");

    assert!(inputs[0].contains("当前任务锚点"));
    assert!(inputs[0].contains("没有用搭子形象的参考图啊你这个，重新生成"));
    assert!(inputs[0].contains("image-generation"));
    assert!(inputs[0].contains("角色一致"));
    assert!(inputs[0].contains("| generate-image"));
    assert!(inputs[0].contains("| mmx-cli"));
    assert!(inputs[0].contains("| imagegen"));
    assert!(
        !inputs[0].contains("| interview-script"),
        "non-image product research skill must be gated before LLM input: {}",
        inputs[0]
    );
    assert!(bm25_candidates_json.contains("generate-image"));
    assert!(bm25_candidates_json.contains("mmx-cli"));
    assert!(bm25_candidates_json.contains("imagegen"));
    assert!(!bm25_candidates_json.contains("interview-script"));

    let memories = env.router_intent_memories();
    assert_eq!(memories.len(), 1);
    assert!(memories[0].contains("重新生成图片"));
    assert!(!memories[0].contains("没有用搭子形象的参考图啊你这个"));

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("generate-image"), "stdout={stdout}");
    assert!(
        !stdout.contains("一句话让用户挑"),
        "single direct image-regeneration match should not ask user to choose: {stdout}"
    );
    assert!(
        !stdout.contains("mmx-cli"),
        "exclusive single-task postprocess should keep only the first direct match: {stdout}"
    );
    let chosen = env.router_chosen_skills_jsons();
    assert_eq!(chosen.len(), 1);
    assert!(chosen[0].contains("generate-image"));
    assert!(!chosen[0].contains("mmx-cli"));
    assert!(!chosen[0].contains("imagegen"));
}

#[test]
fn mock_llm_ktv_webview_emulator_can_emit_ktv_candidate() {
    let env = TestEnv::new();
    plant_android_debug_fixture(&env);
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    let mock = MockLlm::start(MockBehavior::Sequence(&[
        "COMPATIBLE\nreasoning: KTV vehicle WebView emulator workflow\nktv-car-debug-suite\nandroid-cli\nemulator-launch\n",
    ]));
    enable_recommend_config(&env, mock.base_url());

    let out = env.run(&[
        "recommend",
        "帮我调试 KTV 车机 WebView H5 在安卓模拟器里的白屏",
    ]);
    assert!(
        out.status.success(),
        "KTV emulator recommend must succeed (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ktv-car-debug-suite"), "stdout={stdout}");
    assert!(stdout.contains("android-cli"), "stdout={stdout}");
    assert!(stdout.contains("emulator-launch"), "stdout={stdout}");

    let inputs = env.router_llm_inputs();
    assert_eq!(inputs.len(), 1);
    assert!(
        inputs[0].contains("| ktv-car-debug-suite"),
        "KTV candidate must remain visible for KTV/WebView prompts: {}",
        inputs[0]
    );
}

// ─── 7. Telemetry persisted even on LLM error ──────────────────────────────

#[test]
fn mock_llm_http_500_persists_error_router_event() {
    // Plan 5.7 #7: when the LLM endpoint returns HTTP 500 the binary must
    // STILL insert a router_events row, with status='error' and a non-null
    // error_msg. This is the cost-audit invariant — silent failures hide
    // crashes from the operator.
    let env = TestEnv::new();
    // Description overlaps the prompt so the skill survives the BM25 relevance
    // cutoff and the request actually reaches Stage-2 — this test is about the
    // Stage-2 HTTP-500 → status=error path, not the zero-candidate short-circuit
    // (which lands status=ok before any Stage-2 call).
    env.plant_skill("doomed", "trigger an error path when routing");
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    let mock = MockLlm::start(MockBehavior::InternalError);
    enable_recommend_config(&env, mock.base_url());

    let out = env.run(&["recommend", "trigger an error path"]);
    // recommend() never returns a non-zero status code for an LLM error —
    // it prints a `# runai recommend skipped: ...` line to stderr and
    // exits 0 so the hook doesn't poison Claude Code.
    assert!(
        out.status.success(),
        "binary must exit 0 even on LLM error (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );

    // No hook output on error → stdout must be empty (or at least not
    // contain a curl activation, since render_hook_output bails when the
    // decision has zero skills).
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("curl"),
        "no hook activation may render on LLM failure, got:\n{stdout}"
    );

    // Telemetry: exactly one event, status="error", error_msg populated.
    let statuses = env.router_event_status_list();
    assert_eq!(
        statuses.len(),
        1,
        "router_event must be persisted on LLM error, got {:?}",
        statuses
    );
    assert_eq!(statuses[0], "error", "LLM HTTP 500 → status=error");

    let errors = env.router_event_error_msgs();
    assert_eq!(errors.len(), 1);
    let msg = errors[0]
        .as_ref()
        .expect("error_msg must be populated on failure");
    assert!(
        !msg.is_empty(),
        "error_msg must be non-empty on HTTP 500, got: {msg:?}"
    );
    // Must mention HTTP error context — useful for the audit trail.
    let lower = msg.to_lowercase();
    assert!(
        lower.contains("500") || lower.contains("router http") || lower.contains("status"),
        "error_msg must surface HTTP failure, got: {msg}"
    );
}

// ─── Harness message pre-gate (zero LLM calls, zero telemetry) ─────────────

#[test]
fn harness_message_makes_no_llm_call_and_no_router_event() {
    // A host-injected `<task-notification>` envelope is never a human asking
    // for a skill. It must be dropped before either LLM wave and before any
    // router_events write — silent like the disabled path.
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "test skill alpha");
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    let mock = MockLlm::start(MockBehavior::OkPickSkill(
        "alpha-skill",
        "this reasoning must never be produced",
    ));
    enable_recommend_config(&env, mock.base_url());

    let out = env.run(&[
        "recommend",
        "<task-notification>build queue drained, 3 jobs done",
    ]);
    assert!(
        out.status.success(),
        "harness gate must exit 0 (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.is_empty(),
        "harness message must produce silent hook output, got:\n{stdout}"
    );

    // Allow a beat for any (erroneous) in-flight request to land.
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(
        mock.request_count(),
        0,
        "harness message must NOT reach the LLM (0 requests expected)"
    );
    assert_eq!(
        env.router_events_count(),
        0,
        "harness message must NOT write a router_events row"
    );
}

// ─── Feedback signal surfaces on the Stage-2 candidate line ─────────────────

#[test]
fn seeded_adoption_and_feedback_render_markers_in_router_input() {
    // With real adoption + explicit feedback history in the DB, the Stage-2
    // router input's candidate line must carry `[adopt:NN%]` and `[fb:+P/-N]`.
    let env = TestEnv::new();
    env.plant_skill("alpha-skill", "alpha skill for markers");
    env.force_ai_index(
        "alpha-skill",
        "alpha-skill alpha marker test skill 处理 alpha 任务",
        "alpha-skill: task: 处理 alpha 任务 triggers: alpha inputs: x outputs: y",
        6,
    );
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    // 4 chosen-sessions, 3 adopted → adopt = 75%. 2 up / 1 down feedback.
    env.seed_router_history("alpha-skill", 4, 3);
    env.seed_feedback("alpha-skill", 2, 1);

    let mock = MockLlm::start(MockBehavior::Sequence(&[
        "intent: 处理 alpha 任务\ninclude_terms: alpha",
        "EXCLUSIVE\nreasoning: alpha matches\nalpha-skill\n",
    ]));
    enable_recommend_config(&env, mock.base_url());

    let out = env.run(&["recommend", "帮我处理 alpha 任务"]);
    assert!(
        out.status.success(),
        "recommend must succeed (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );

    // Seeded rows carry empty llm_input; the real recommend row names the
    // candidate and must show both markers on its candidate line.
    let inputs = env.router_llm_inputs();
    assert!(
        inputs.iter().any(|i| i.contains("| alpha-skill")
            && i.contains("[adopt:75%]")
            && i.contains("[fb:+2/-1]")),
        "stage-2 router input must carry adopt/feedback markers, got: {inputs:?}"
    );
}

// ─── Hook stdin protocol smoke (positional vs JSON parity) ─────────────────

#[test]
fn disabled_recommend_via_stdin_json_also_silent_with_marker() {
    // The CLI accepts either a positional prompt or Claude Code's hook
    // JSON on stdin. When the router is disabled both branches must take
    // the same no-op shortcut — otherwise we have a hidden code path.
    let env = TestEnv::new();
    std::fs::write(env.bootstrap_seen_path(), "1").unwrap();

    let stdin = serde_json::json!({
        "prompt": "say hi",
        "transcript_path": null,
        "session_id": "sess-test-1",
        "cwd": env.home().display().to_string(),
    });

    let out = env.run_with_input(&["recommend"], &stdin.to_string());
    assert!(
        out.status.success(),
        "stdin-mode disabled recommend must exit 0 (stderr={})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        !stdout.contains("curl"),
        "stdin-mode disabled recommend must not render hook output, got:\n{stdout}"
    );
    assert_eq!(env.router_events_count(), 0);
}

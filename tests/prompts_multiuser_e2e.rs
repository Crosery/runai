//! PLANNING §1.3 — prompt-injection toggles, multi-user isolation.
//!
//! Verifies that:
//! 1. User A turning off `recommend_history_prefix` strips the history block
//!    from the LLM input on A's request, while B (toggle on) keeps it.
//! 2. A's prefs change cannot bleed into B's concurrent request — each
//!    call reads the requesting user's prefs freshly per request.
//! 3. An unauthenticated request (`user_id_opt = None`) walks the default
//!    path (every prompt enabled) and never reads any user's prefs.
//! 4. Switching the logged-in account is observed on the next request with
//!    no stale-prefs cache — exactly what the hook needs when the local
//!    `~/.runai-identity` is rotated.
//!
//! Strategy: drive `recommend::recommend_for_user` directly. Configure the
//! router with `Provider::ClaudeCli` so no api_key is required; the
//! sub-shell call to `claude` will fail in the CI env, which is exactly
//! what we want — the router persists the `RouterEvent` (including the
//! `llm_input` string we sent the LLM) BEFORE bailing on the error. We
//! then inspect `router_events.llm_input` to assert which optional
//! prompt blocks were stripped.

#![cfg(not(target_os = "windows"))]

use assert_cmd::cargo::CommandCargoExt;
use runai::core::auth;
use runai::core::manager::SkillManager;
use runai::core::prefs::UserPrefs;
use runai::core::recommend::{self, Provider, RecommendConfig, SessionMode};
use runai::core::resource::{Resource, ResourceKind, Source};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Bring up an isolated `<tempdir>/.runai/` data root and write a
/// `config.toml` that points the router at a deliberately-unreachable
/// localhost port. The LLM call will fail with connection refused on
/// every box (dev / CI / Linux / mac) — fast, deterministic, no external
/// dependency. The `RouterEvent` (carrying `llm_input`) is persisted
/// BEFORE the bail, which is the only thing the test reads back.
fn setup() -> (TempDir, SkillManager) {
    let home = tempfile::tempdir().expect("tmp HOME");
    let data = home.path().join(".runai");
    let mgr = SkillManager::with_base(data.clone()).expect("manager init");

    let cfg = RecommendConfig {
        enabled: true,
        provider: Provider::Anthropic,
        base_url: "http://127.0.0.1:1".into(),
        model: "claude-test".into(),
        api_key: "dummy".into(),
        min_prompt_len: 0,
        session_mode: SessionMode::Oneshot,
        summary_lang_confirmed: true,
        ..Default::default()
    };
    cfg.save(mgr.paths()).expect("save config");

    (home, mgr)
}

fn runai_cmd() -> Command {
    Command::cargo_bin("runai").expect("runai binary built by cargo test")
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral bind");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

fn wait_for_port(port: u16, timeout: Duration) {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("runai server did not bind 127.0.0.1:{port}");
}

struct ServerGuard {
    child: Child,
    port: u16,
}

impl ServerGuard {
    fn spawn(home: &Path, data_dir: &Path) -> Self {
        let port = free_port();
        let mut cmd = runai_cmd();
        cmd.arg("server")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--mode")
            .arg("team")
            .env("HOME", home)
            .env("RUNE_DATA_DIR", data_dir)
            .env("RUNAI_NO_AUTOSPAWN", "1")
            .env_remove("SKILL_MANAGER_DATA_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().expect("spawn runai server");
        wait_for_port(port, Duration::from_secs(8));
        Self { child, port }
    }

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

/// Plant a public skill so the router has a non-empty candidate set
/// (otherwise `recommend_for_user` short-circuits and never writes a row).
fn write_public_skill(root: &Path, name: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("# {name}\n\ntest skill for prompts e2e\n"),
    )
    .unwrap();
}

fn write_private_skill(mgr: &SkillManager, uid: &str, name: &str, body_marker: &str) -> Resource {
    mgr.paths().ensure_user_dirs(uid).unwrap();
    let dir = mgr.paths().user_skills_dir(uid).unwrap().join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("# {name}\n\nprivate skill marker {body_marker}\n"),
    )
    .unwrap();
    let source = Source::Local { path: dir.clone() };
    let res = Resource {
        id: Resource::generate_id(&source, name, Some(uid)),
        name: name.to_string(),
        kind: ResourceKind::Skill,
        description: format!("private description {body_marker}"),
        directory: dir,
        source,
        installed_at: chrono::Utc::now().timestamp(),
        enabled: HashMap::new(),
        usage_count: 0,
        last_used_at: None,
        owner_user_id: Some(uid.to_string()),
        publish_status: "draft".to_string(),
    };
    mgr.db().insert_resource(&res).unwrap();
    res
}

/// Create a real Claude-Code-style transcript jsonl so the history block
/// has real text to splice in when the toggle is on.
fn write_transcript(path: &Path, user_lines: &[&str]) {
    use std::io::Write;
    let mut f = std::fs::File::create(path).unwrap();
    for line in user_lines {
        let row = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": line},
        });
        writeln!(f, "{row}").unwrap();
        let assistant = serde_json::json!({
            "type": "assistant",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "noted"}]},
        });
        writeln!(f, "{assistant}").unwrap();
    }
}

/// Register a user + write their prefs JSON directly through the DB.
fn make_user(mgr: &SkillManager, username: &str, prefs: &UserPrefs) -> String {
    let uid = auth::new_user_id();
    let api_key = auth::new_api_key();
    let api_key_hash = auth::key_hash(&auth::BearerToken(api_key));
    let password_hash = auth::hash_password("test-password").unwrap();
    mgr.db()
        .create_user(&uid, username, &password_hash, &api_key_hash, false)
        .unwrap();
    mgr.db()
        .update_user_prefs(&uid, &prefs.to_json_str())
        .unwrap();
    uid
}

fn make_user_with_api_key(
    mgr: &SkillManager,
    username: &str,
    api_key: &str,
    prefs: &UserPrefs,
) -> String {
    let uid = auth::new_user_id();
    let api_key_hash = auth::key_hash(&auth::BearerToken(api_key.to_string()));
    let password_hash = auth::hash_password("test-password").unwrap();
    mgr.db()
        .create_user(&uid, username, &password_hash, &api_key_hash, false)
        .unwrap();
    mgr.db()
        .update_user_prefs(&uid, &prefs.to_json_str())
        .unwrap();
    uid
}

/// Fetch the most recent `llm_input` row stamped for `uid` (or `None` for
/// unauthenticated). `recommend_for_user` stamps `user_id` on the event
/// from its `user_id_opt` arg, so this filter is exact.
fn latest_llm_input(mgr: &SkillManager, uid: Option<&str>) -> String {
    let rows = mgr
        .db()
        .router_events_paged_filtered(None, 50, 0, None, false, uid)
        .unwrap();
    assert!(
        !rows.is_empty(),
        "expected a router_events row for uid={uid:?}, got none"
    );
    rows[0].llm_input.clone()
}

fn latest_router_event(mgr: &SkillManager, uid: Option<&str>) -> runai::core::db::RouterEvent {
    let rows = mgr
        .db()
        .router_events_paged_filtered(None, 50, 0, None, false, uid)
        .unwrap();
    assert!(
        !rows.is_empty(),
        "expected a router_events row for uid={uid:?}, got none"
    );
    rows[0].clone()
}

fn router_event_count_for(mgr: &SkillManager, uid: Option<&str>) -> usize {
    mgr.db()
        .router_events_paged_filtered(None, 50, 0, None, false, uid)
        .unwrap()
        .len()
}

fn anonymous_router_event_count(mgr: &SkillManager) -> usize {
    mgr.db()
        .router_events_paged(None, 50, 0, None, false)
        .unwrap()
        .into_iter()
        .filter(|ev| ev.user_id.is_none())
        .count()
}

fn llm_candidate_line_count(input: &str) -> usize {
    input
        .split("候选 skill:\n")
        .nth(1)
        .and_then(|tail| tail.split("\n\n---").next())
        .unwrap_or("")
        .lines()
        .filter(|line| line.starts_with("- "))
        .count()
}

fn run_cli_recommend_with_home(home: &Path, data_dir: &Path, hook_json: serde_json::Value) {
    let mut cmd = runai_cmd();
    cmd.arg("recommend")
        .env("HOME", home)
        .env("RUNE_DATA_DIR", data_dir)
        .env("RUNAI_NO_AUTOSPAWN", "1")
        .env_remove("SKILL_MANAGER_DATA_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn runai recommend");
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        write!(stdin, "{hook_json}").expect("write hook JSON");
    }
    let out = child.wait_with_output().expect("runai recommend output");
    assert!(
        out.status.success(),
        "runai recommend exited {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write_client_identity(home: &Path, username: &str, uid: &str, api_key: &str) {
    let body = serde_json::json!({
        "version": 1,
        "server": "http://127.0.0.1:17888",
        "user_id": uid,
        "username": username,
        "api_key": api_key,
        "is_admin": false,
    });
    let path = home.join(".runai-identity");
    std::fs::write(&path, serde_json::to_string_pretty(&body).unwrap()).unwrap();
}

/// Marker substrings drawn straight from the .md files. If the body of
/// `recommend_history_prefix.md` changes, this test needs the new substring
/// — that's intentional; the prompt registry is the contract.
const HISTORY_MARKER: &str = "最近对话历史";
const CWD_MARKER: &str = "用作领域消歧线索";
const PROJECT_CONTEXT_MARKER: &str = "当前项目背景";

#[test]
fn user_a_off_strips_history_block_user_b_on_keeps_it() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();
    write_public_skill(&paths.skills_dir(), "beta");
    mgr.register_local_skill("beta").unwrap();

    // A: history off; B: defaults (history on).
    let mut a_prefs = UserPrefs {
        allow_public_recommend: true,
        ..Default::default()
    };
    a_prefs
        .prompt_injection_flags
        .insert("recommend_history_prefix".into(), false);
    let b_prefs = UserPrefs {
        allow_public_recommend: true,
        ..Default::default()
    };
    let a_uid = make_user(&mgr, "alice", &a_prefs);
    let b_uid = make_user(&mgr, "bob", &b_prefs);

    let transcript = paths.data_dir().join("session.jsonl");
    write_transcript(&transcript, &["earlier user message about pptx export"]);

    // Drive both requests. The LLM call will fail (no `claude` binary, or
    // it'll choke on the test prompt) but the user_msg lands in
    // router_events first. We deliberately ignore the Err.
    let _ = recommend::recommend_for_user(
        &mgr,
        "现在做点东西",
        Some(&transcript),
        Some("session-a"),
        Some("/tmp/proj-a"),
        Some(&a_uid),
    );
    let _ = recommend::recommend_for_user(
        &mgr,
        "现在做点东西",
        Some(&transcript),
        Some("session-b"),
        Some("/tmp/proj-b"),
        Some(&b_uid),
    );

    let a_input = latest_llm_input(&mgr, Some(&a_uid));
    let b_input = latest_llm_input(&mgr, Some(&b_uid));

    assert!(
        !a_input.contains(HISTORY_MARKER),
        "A turned recommend_history_prefix OFF — block must be absent, got: {a_input}"
    );
    assert!(
        b_input.contains(HISTORY_MARKER),
        "B keeps default (ON) — block must be present, got: {b_input}"
    );
}

#[test]
fn a_prefs_change_does_not_affect_b_concurrent_request() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();

    // A: cwd off. B: cwd on (default). Both need allow_public_recommend so
    // the public "alpha" skill makes it into their per-user candidate set
    // and the router actually reaches the LLM call (which then fails fast
    // on the bogus URL, persisting llm_input).
    let mut a_prefs = UserPrefs {
        allow_public_recommend: true,
        ..Default::default()
    };
    a_prefs
        .prompt_injection_flags
        .insert("recommend_cwd_prefix".into(), false);
    let b_prefs = UserPrefs {
        allow_public_recommend: true,
        ..Default::default()
    };
    let a_uid = make_user(&mgr, "alice", &a_prefs);
    let b_uid = make_user(&mgr, "bob", &b_prefs);

    // Each thread opens its OWN SkillManager (rusqlite Connection is !Sync,
    // so we cannot share the manager). They concurrently hit the same
    // shared on-disk DB + config, so the in-memory user-prefs read is
    // genuinely scoped per-request — the test still proves "A's prefs don't
    // bleed into B's request" because both managers consult the same prefs
    // table and would fall over the same way if there were a global cache.
    let data_dir: PathBuf = paths.data_dir().to_path_buf();
    let data_dir_a = data_dir.clone();
    let data_dir_b = data_dir.clone();
    let a_uid_t = a_uid.clone();
    let b_uid_t = b_uid.clone();

    let h_a = thread::spawn(move || {
        let mgr_a = SkillManager::with_base(data_dir_a).expect("alice mgr");
        let _ = recommend::recommend_for_user(
            &mgr_a,
            "drawing",
            None,
            Some("session-a-conc"),
            Some("/tmp/proj-a-cwd"),
            Some(&a_uid_t),
        );
    });
    let h_b = thread::spawn(move || {
        let mgr_b = SkillManager::with_base(data_dir_b).expect("bob mgr");
        let _ = recommend::recommend_for_user(
            &mgr_b,
            "drawing",
            None,
            Some("session-b-conc"),
            Some("/tmp/proj-b-cwd"),
            Some(&b_uid_t),
        );
    });
    h_a.join().unwrap();
    h_b.join().unwrap();

    let a_input = latest_llm_input(&mgr, Some(&a_uid));
    let b_input = latest_llm_input(&mgr, Some(&b_uid));

    assert!(
        !a_input.contains(CWD_MARKER),
        "A turned recommend_cwd_prefix OFF — block absent: {a_input}"
    );
    assert!(
        b_input.contains(CWD_MARKER),
        "B keeps default ON — cwd block present: {b_input}"
    );

    // Sanity: each request's llm_input contains its own cwd path string
    // (only the one with cwd ON), proving the cwd substitution was scoped
    // to the requesting user — no cross-pollution.
    assert!(
        !a_input.contains("/tmp/proj-a-cwd"),
        "A: cwd path leaked despite toggle OFF"
    );
    assert!(
        b_input.contains("/tmp/proj-b-cwd"),
        "B: cwd path missing despite toggle ON"
    );
}

#[test]
fn unauthenticated_request_uses_defaults_and_does_not_read_user_prefs() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();

    // Register a user with EVERYTHING toggled off — if the unauth path
    // mistakenly read this user's prefs, the assertion below would fail.
    let mut paranoid = UserPrefs::default();
    paranoid
        .prompt_injection_flags
        .insert("recommend_history_prefix".into(), false);
    paranoid
        .prompt_injection_flags
        .insert("recommend_cwd_prefix".into(), false);
    paranoid
        .prompt_injection_flags
        .insert("recommend_project_context".into(), false);
    let _paranoid_uid = make_user(&mgr, "paranoid", &paranoid);

    let transcript = paths.data_dir().join("session.jsonl");
    write_transcript(&transcript, &["earlier turn"]);

    let _ = recommend::recommend_for_user(
        &mgr,
        "现在做点东西",
        Some(&transcript),
        Some("session-anon"),
        Some("/tmp/proj-anon"),
        None,
    );

    let anon_input = latest_llm_input(&mgr, None);
    // Defaults are ON for every toggleable block — each marker present.
    // (recommend_already_routed was removed with session no-repeat.)
    assert!(
        anon_input.contains(HISTORY_MARKER),
        "unauth: history block must be present (default ON): {anon_input}"
    );
    assert!(
        anon_input.contains(CWD_MARKER),
        "unauth: cwd block must be present (default ON): {anon_input}"
    );
    // The paranoid user's flags MUST NOT have been read for this request —
    // if they had, every block above would be stripped.
}

#[test]
fn stale_bearer_does_not_fall_back_to_anonymous_project_context() {
    let (home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();

    let cwd = home.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        cwd.join("CLAUDE.md"),
        "# project rules\nsecret project context\n",
    )
    .unwrap();

    let stale_key = auth::new_api_key();
    let current_key = auth::new_api_key();
    let mut prefs = UserPrefs {
        allow_public_recommend: true,
        read_claude_md: false,
        ..Default::default()
    };
    prefs
        .prompt_injection_flags
        .insert("recommend_project_context".into(), false);
    let uid = make_user_with_api_key(&mgr, "stale-user", &current_key, &prefs);
    assert_eq!(
        router_event_count_for(&mgr, None),
        0,
        "precondition: anonymous router event list is empty"
    );
    assert_eq!(
        router_event_count_for(&mgr, Some(&uid)),
        0,
        "precondition: user router event list is empty"
    );

    let server = ServerGuard::spawn(home.path(), paths.data_dir());
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
        .post(format!("{}/recommend", server.base_url()))
        .bearer_auth(&stale_key)
        .json(&serde_json::json!({
            "prompt": "帮我推荐一个技能",
            "session_id": "stale-bearer-session",
            "cwd": cwd.to_string_lossy(),
            "transcript_path": "",
        }))
        .send()
        .expect("POST /recommend with stale bearer");
    assert_eq!(resp.status().as_u16(), 200);
    let body = resp.text().unwrap_or_default();
    assert!(body.is_empty(), "stale Bearer returns empty hook output");
    assert_eq!(
        router_event_count_for(&mgr, None),
        0,
        "stale Bearer must not drop into anonymous default prefs"
    );
    assert_eq!(
        router_event_count_for(&mgr, Some(&uid)),
        0,
        "stale Bearer must not write a user-scoped event either"
    );

    let _ = recommend::recommend_for_user(
        &mgr,
        "帮我推荐一个技能",
        None,
        Some("fresh-key-direct"),
        Some(cwd.to_str().unwrap()),
        Some(&uid),
    );
    let user_input = latest_llm_input(&mgr, Some(&uid));
    assert!(
        !user_input.contains(PROJECT_CONTEXT_MARKER),
        "fresh authenticated user prefs keep project-context block absent"
    );
}

#[test]
fn local_cli_recommend_uses_identity_prefs_instead_of_anonymous_defaults() {
    let (home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();

    let cwd = home.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(
        cwd.join("CLAUDE.md"),
        "# Project Context\nLOCAL_CLI_IDENTITY_PROJECT_CONTEXT_MARKER\n",
    )
    .unwrap();

    let api_key = auth::new_api_key();
    let mut prefs = UserPrefs {
        allow_public_recommend: true,
        read_claude_md: true,
        ..Default::default()
    };
    prefs
        .prompt_injection_flags
        .insert("recommend_project_context".into(), false);
    let uid = make_user_with_api_key(&mgr, "cli-user", &api_key, &prefs);
    write_client_identity(home.path(), "cli-user", &uid, &api_key);

    run_cli_recommend_with_home(
        home.path(),
        paths.data_dir(),
        serde_json::json!({
            "prompt": "alpha project context routing test",
            "session_id": "local-cli-identity",
            "cwd": cwd.to_string_lossy(),
            "transcript_path": "",
        }),
    );

    let input = latest_llm_input(&mgr, Some(&uid));
    assert!(
        !input.contains(PROJECT_CONTEXT_MARKER),
        "CLI identity prefs must remove project-context prompt block: {input}"
    );
    assert!(
        !input.contains("LOCAL_CLI_IDENTITY_PROJECT_CONTEXT_MARKER"),
        "CLI identity prefs must remove CLAUDE.md content: {input}"
    );
    assert_eq!(
        anonymous_router_event_count(&mgr),
        0,
        "local CLI recommend with valid identity must not write anonymous events"
    );
}

#[test]
fn local_cli_recommend_stale_identity_does_not_write_anonymous_event() {
    let (home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();

    let prefs = UserPrefs {
        allow_public_recommend: true,
        ..Default::default()
    };
    let real_key = auth::new_api_key();
    let uid = make_user_with_api_key(&mgr, "cli-user", &real_key, &prefs);
    let stale_key = auth::new_api_key();
    write_client_identity(home.path(), "cli-user", &uid, &stale_key);

    run_cli_recommend_with_home(
        home.path(),
        paths.data_dir(),
        serde_json::json!({
            "prompt": "alpha routing test",
            "session_id": "local-cli-stale-identity",
            "cwd": "",
            "transcript_path": "",
        }),
    );

    assert_eq!(
        router_event_count_for(&mgr, Some(&uid)),
        0,
        "stale local identity must not write a user-scoped event"
    );
    assert_eq!(
        anonymous_router_event_count(&mgr),
        0,
        "stale local identity must fail closed instead of anonymous routing"
    );
}

#[test]
fn switching_logged_in_account_picks_up_new_prefs_immediately() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();

    // A: recommend_cwd_prefix off. B: cwd_prefix on (default). Both need
    // allow_public_recommend so the public "alpha" skill enters the
    // candidate set (otherwise the recommend short-circuits with an empty
    // candidate list and never persists a router_events row).
    //
    // (This used the recommend_already_routed toggle before session no-repeat
    // was removed; retargeted onto the still-live cwd toggle. The mechanism
    // under test — per-request prefs, no cross-request caching — is unchanged.)
    let mut a_prefs = UserPrefs {
        allow_public_recommend: true,
        ..Default::default()
    };
    a_prefs
        .prompt_injection_flags
        .insert("recommend_cwd_prefix".into(), false);
    let b_prefs = UserPrefs {
        allow_public_recommend: true,
        ..Default::default()
    };
    let a_uid = make_user(&mgr, "alice", &a_prefs);
    let b_uid = make_user(&mgr, "bob", &b_prefs);

    // First as A — cwd passed but toggle OFF, so the cwd block must be absent.
    let _ = recommend::recommend_for_user(
        &mgr,
        "switching test",
        None,
        Some("shared-session"),
        Some("/tmp/switch-proj"),
        Some(&a_uid),
    );
    // Immediately switch to B — same process, same SkillManager, no DB
    // reopen. If the router cached prefs from A's call we'd see the OFF
    // toggle here too.
    let _ = recommend::recommend_for_user(
        &mgr,
        "switching test",
        None,
        Some("shared-session"),
        Some("/tmp/switch-proj"),
        Some(&b_uid),
    );

    let a_input = latest_llm_input(&mgr, Some(&a_uid));
    let b_input = latest_llm_input(&mgr, Some(&b_uid));

    assert!(
        !a_input.contains(CWD_MARKER),
        "A: cwd_prefix OFF — block absent: {a_input}"
    );
    assert!(
        b_input.contains(CWD_MARKER),
        "B (same process, just switched): cwd_prefix default ON — block present: {b_input}"
    );
}

#[test]
fn default_library_scope_includes_private_skill_and_uses_private_index() {
    let (_home, mgr) = setup();

    let prefs = UserPrefs::default();
    let uid = make_user(&mgr, "alice", &prefs);
    let private = write_private_skill(&mgr, &uid, "private-alpha", "PRIVATE_ALPHA_BODY");
    mgr.db()
        .set_skill_ai_summary_scored_scoped(
            &private.name,
            Some(&uid),
            "task: 私有技能摘要 PRIVATE_ALPHA_INDEX\ntriggers: private-alpha\ninputs: 私有输入\noutputs: 私有输出\nnot-for: 公共技能",
            9,
        )
        .unwrap();

    let _ = recommend::recommend_for_user(
        &mgr,
        "private-alpha",
        None,
        Some("private-scope-session"),
        None,
        Some(&uid),
    );

    let input = latest_llm_input(&mgr, Some(&uid));
    assert!(
        input.contains("private-alpha"),
        "default library scope must still include private owned skills: {input}"
    );
    assert!(
        input.contains("PRIVATE_ALPHA_INDEX"),
        "router candidate listing must use the private skill's scoped AI index: {input}"
    );
}

#[test]
fn router_llm_input_strips_template_frontmatter_and_honors_bm25_candidate_limit() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();

    let mut cfg = RecommendConfig::load(paths).unwrap();
    cfg.top_k = 8;
    cfg.save(paths).unwrap();

    for idx in 0..9 {
        let name = format!("skill-{idx}");
        write_public_skill(&paths.skills_dir(), &name);
        mgr.register_local_skill(&name).unwrap();
    }

    let prefs = UserPrefs {
        allow_public_recommend: true,
        bm25_candidate_limit: 5,
        ..Default::default()
    };
    let uid = make_user(&mgr, "alice", &prefs);

    let _ = recommend::recommend_for_user(
        &mgr,
        "skill",
        None,
        Some("candidate-cap-session"),
        None,
        Some(&uid),
    );

    let event = latest_router_event(&mgr, Some(&uid));
    assert_eq!(
        event.bm25_kept, 5,
        "router must keep the configured number of LLM-facing candidates"
    );
    assert_eq!(
        llm_candidate_line_count(&event.llm_input),
        5,
        "llm_input candidate listing must match bm25_candidate_limit: {}",
        event.llm_input
    );
    assert!(
        !event.llm_input.contains("<!-- prompt:"),
        "runtime router LLM input must not include prompt-template frontmatter: {}",
        event.llm_input
    );
}

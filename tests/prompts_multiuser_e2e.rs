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

use runai::core::auth;
use runai::core::manager::SkillManager;
use runai::core::prefs::UserPrefs;
use runai::core::recommend::{self, Provider, RecommendConfig, SessionMode};
use std::path::{Path, PathBuf};
use std::thread;
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

    let mut cfg = RecommendConfig::default();
    cfg.enabled = true;
    cfg.provider = Provider::Anthropic;
    cfg.base_url = "http://127.0.0.1:1".into();
    cfg.model = "claude-test".into();
    cfg.api_key = "dummy".into();
    cfg.min_prompt_len = 0;
    cfg.session_mode = SessionMode::Oneshot;
    cfg.summary_lang_confirmed = true;
    cfg.save(mgr.paths()).expect("save config");

    (home, mgr)
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

/// Marker substrings drawn straight from the .md files. If the body of
/// `recommend_history_prefix.md` changes, this test needs the new substring
/// — that's intentional; the prompt registry is the contract.
const HISTORY_MARKER: &str = "最近对话历史";
const ALREADY_ROUTED_MARKER: &str = "本会话已推过的 skill";
const CWD_MARKER: &str = "用作领域消歧线索";

#[test]
fn user_a_off_strips_history_block_user_b_on_keeps_it() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();
    write_public_skill(&paths.skills_dir(), "beta");
    mgr.register_local_skill("beta").unwrap();

    // A: history off; B: defaults (history on).
    let mut a_prefs = UserPrefs::default();
    a_prefs.allow_public_recommend = true;
    a_prefs
        .prompt_injection_flags
        .insert("recommend_history_prefix".into(), false);
    let mut b_prefs = UserPrefs::default();
    b_prefs.allow_public_recommend = true;
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
    let mut a_prefs = UserPrefs::default();
    a_prefs.allow_public_recommend = true;
    a_prefs
        .prompt_injection_flags
        .insert("recommend_cwd_prefix".into(), false);
    let mut b_prefs = UserPrefs::default();
    b_prefs.allow_public_recommend = true;
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
        .insert("recommend_already_routed".into(), false);
    paranoid
        .prompt_injection_flags
        .insert("recommend_cwd_prefix".into(), false);
    paranoid
        .prompt_injection_flags
        .insert("recommend_project_context".into(), false);
    let _paranoid_uid = make_user(&mgr, "paranoid", &paranoid);

    let transcript = paths.data_dir().join("session.jsonl");
    write_transcript(&transcript, &["earlier turn"]);

    // Pre-seed an already-routed entry for this session so the
    // already_routed block has content to render — proves the block was
    // actually injected, not just absent for lack of input data.
    let event = runai::core::db::RouterEvent {
        id: None,
        ts: chrono::Utc::now().timestamp(),
        provider: "claude-cli".into(),
        model: "claude-haiku-test".into(),
        prompt_tokens: 0,
        completion_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        latency_ms: 0,
        chosen_skills_json: r#"["alpha"]"#.into(),
        candidate_count: 1,
        status: "ok".into(),
        error_msg: None,
        session_id: "session-anon".into(),
        mode: "exclusive".into(),
        user_prompt: "earlier".into(),
        cwd: "".into(),
        bm25_kept: 1,
        llm_raw_response: "EXCLUSIVE\nreasoning: seed\nalpha\n".into(),
        hook_output: "".into(),
        llm_input: "(seed)".into(),
        user_id: None,
    };
    mgr.db().insert_router_event(&event).unwrap();

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
    assert!(
        anon_input.contains(HISTORY_MARKER),
        "unauth: history block must be present (default ON): {anon_input}"
    );
    assert!(
        anon_input.contains(ALREADY_ROUTED_MARKER),
        "unauth: already_routed block must be present (default ON): {anon_input}"
    );
    assert!(
        anon_input.contains(CWD_MARKER),
        "unauth: cwd block must be present (default ON): {anon_input}"
    );
    // The paranoid user's flags MUST NOT have been read for this request —
    // if they had, every block above would be stripped.
}

#[test]
fn switching_logged_in_account_picks_up_new_prefs_immediately() {
    let (_home, mgr) = setup();
    let paths = mgr.paths();
    write_public_skill(&paths.skills_dir(), "alpha");
    mgr.register_local_skill("alpha").unwrap();

    // A: already_routed off. B: already_routed on (default). Both need
    // allow_public_recommend so the public "alpha" skill enters the
    // candidate set (otherwise the recommend short-circuits with an empty
    // candidate list and never persists a router_events row).
    let mut a_prefs = UserPrefs::default();
    a_prefs.allow_public_recommend = true;
    a_prefs
        .prompt_injection_flags
        .insert("recommend_already_routed".into(), false);
    let mut b_prefs = UserPrefs::default();
    b_prefs.allow_public_recommend = true;
    let a_uid = make_user(&mgr, "alice", &a_prefs);
    let b_uid = make_user(&mgr, "bob", &b_prefs);

    // Pre-seed an already-routed skill in the shared session id so the
    // block has content when the toggle is on.
    let event = runai::core::db::RouterEvent {
        id: None,
        ts: chrono::Utc::now().timestamp(),
        provider: "claude-cli".into(),
        model: "claude-haiku-test".into(),
        prompt_tokens: 0,
        completion_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 0,
        cache_hit_tokens: 0,
        cache_miss_tokens: 0,
        latency_ms: 0,
        chosen_skills_json: r#"["alpha"]"#.into(),
        candidate_count: 1,
        status: "ok".into(),
        error_msg: None,
        session_id: "shared-session".into(),
        mode: "exclusive".into(),
        user_prompt: "earlier".into(),
        cwd: "".into(),
        bm25_kept: 1,
        llm_raw_response: "EXCLUSIVE\nreasoning: seed\nalpha\n".into(),
        hook_output: "".into(),
        llm_input: "(seed)".into(),
        user_id: None,
    };
    mgr.db().insert_router_event(&event).unwrap();

    // First as A.
    let _ = recommend::recommend_for_user(
        &mgr,
        "switching test",
        None,
        Some("shared-session"),
        None,
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
        None,
        Some(&b_uid),
    );

    let a_input = latest_llm_input(&mgr, Some(&a_uid));
    let b_input = latest_llm_input(&mgr, Some(&b_uid));

    assert!(
        !a_input.contains(ALREADY_ROUTED_MARKER),
        "A: already_routed OFF — block absent: {a_input}"
    );
    assert!(
        b_input.contains(ALREADY_ROUTED_MARKER),
        "B (same process, just switched): already_routed default ON — block present: {b_input}"
    );
}

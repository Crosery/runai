//! E2E coverage for `src/tui/app/market_tab.rs` public API.
//!
//! Feature 1: `App::reload_community` (audit asked for
//! `install_market_selected`, but its given file path was wrong — the
//! real `install_market_selected` lives in `src/tui/app/market_ops.rs`
//! and is `pub(super)`, not callable from integration tests. The closest
//! *public* surface inside `market_tab.rs` semantically related to the
//! audit scenario ("install-flow side effects on App state") is
//! `reload_community` — the Community-tab fetcher that mirrors what the
//! install flow reads after an install completes).
//!
//! Real signature: `pub fn reload_community(&mut self)` — returns `()`,
//! not `Result<()>` as the audit assumed; success/failure is communicated
//! through `App.community_loading` / `community_skills` / `community_error`.
//!
//! Why integration tests are limited here: `reload_community` hardcodes
//! `COMMUNITY_PORT = 17888` and calls `server::ensure_running`, which on
//! a missed connect would auto-spawn a detached "runai server" via
//! `current_exe()`. In a `cargo test` binary that target would be the
//! test binary itself. We MUST prevent that by holding a TCP listener on
//! 127.0.0.1:17888 throughout the test so `ensure_running` short-circuits
//! on its "AlreadyRunning" TCP-probe (200 ms connect timeout). When the
//! port is already in use by another process we skip the network-touching
//! test rather than fail flakily.
//!
//! Skipped on Windows: HOME mocking + dir conventions are unix-only per
//! `AGENTS.md` Key constraints.

#![cfg(not(target_os = "windows"))]

use runai::core::manager::SkillManager;
use runai::tui::app::App;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::TempDir;

// ─── env-mutation lock ──────────────────────────────────────────────────────

/// Process-wide lock guarding HOME / RUNE_DATA_DIR mutations + the
/// 17888 listener binding. Inline TUI tests use a private `HOME_LOCK`
/// from `crate::test_support`; integration tests cannot reach it, so we
/// roll our own.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with `HOME` and `RUNE_DATA_DIR` pointed at `tmp` so
/// `SkillManager::with_base` plus any home-derived path (`dirs::home_dir`)
/// stays inside the sandbox.
fn with_sandbox<F: FnOnce()>(tmp: &Path, f: F) {
    let _guard = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let original_home = std::env::var("HOME").ok();
    let original_data = std::env::var("RUNE_DATA_DIR").ok();
    let original_skill = std::env::var("SKILL_MANAGER_DATA_DIR").ok();
    let original_autospawn = std::env::var("RUNAI_NO_AUTOSPAWN").ok();
    unsafe {
        std::env::set_var("HOME", tmp);
        std::env::set_var("RUNE_DATA_DIR", tmp.join(".runai"));
        std::env::set_var("RUNAI_NO_AUTOSPAWN", "1");
        std::env::remove_var("SKILL_MANAGER_DATA_DIR");
    }
    f();
    unsafe {
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match original_data {
            Some(v) => std::env::set_var("RUNE_DATA_DIR", v),
            None => std::env::remove_var("RUNE_DATA_DIR"),
        }
        match original_skill {
            Some(v) => std::env::set_var("SKILL_MANAGER_DATA_DIR", v),
            None => std::env::remove_var("SKILL_MANAGER_DATA_DIR"),
        }
        match original_autospawn {
            Some(v) => std::env::set_var("RUNAI_NO_AUTOSPAWN", v),
            None => std::env::remove_var("RUNAI_NO_AUTOSPAWN"),
        }
    }
}

fn fresh_app(tmp: &Path) -> App {
    let data = tmp.join(".runai");
    std::fs::create_dir_all(&data).expect("create data dir");
    let mgr = SkillManager::with_base(data).expect("SkillManager::with_base");
    App::new(mgr)
}

/// Best-effort grab of 127.0.0.1:17888. Some other process (a real
/// `runai server` on the dev box, a parallel test in another binary)
/// may already own it — returning `None` is the signal for the caller
/// to skip the network-touching assertion rather than fight for the port.
fn try_lock_community_port() -> Option<TcpListener> {
    TcpListener::bind("127.0.0.1:17888").ok()
}

/// Tiny HTTP serve loop that answers every accepted request with the
/// supplied status + body for ~5s. Used as a stand-in dashboard so
/// `reload_community` gets a deterministic HTTP response without
/// touching the real server. Runs on a worker thread; the listener
/// closes when the thread exits at deadline.
fn serve_canned_response(listener: TcpListener, status_line: &'static str, body: &'static str) {
    listener.set_nonblocking(false).expect("blocking mode");
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            // ensure_running uses connect_timeout (no read), so we may
            // accept a connection that immediately closes — handle that.
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                    let mut buf = [0u8; 2048];
                    let _ = stream.read(&mut buf);
                    let payload = format!(
                        "HTTP/1.1 {status_line}\r\n\
Content-Type: application/json\r\n\
Content-Length: {}\r\n\
Connection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(payload.as_bytes());
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                Err(_) => break,
            }
        }
    });
}

/// Build a `CommunitySkill` via JSON deserialization (the struct has no
/// `Default` impl and no public constructor; only `name` is required).
fn make_community_skill(name: &str) -> runai::tui::app::CommunitySkill {
    let json = format!(r#"{{"name":"{name}"}}"#);
    serde_json::from_str(&json).expect("seed CommunitySkill from minimal JSON")
}

// ─── feature 1: reload_community ─ audit said `install_market_selected` ────

#[test]
fn reload_community_handles_404_owner_mode() {
    let tmp = TempDir::new().unwrap();
    let Some(listener) = try_lock_community_port() else {
        eprintln!("[skip] 127.0.0.1:17888 already in use; cannot stage canned 404");
        return;
    };
    serve_canned_response(listener, "404 Not Found", "{}");

    with_sandbox(tmp.path(), || {
        let mut app = fresh_app(tmp.path());

        // Pre-load a stale community row to assert it gets cleared on 404.
        app.community_skills.push(make_community_skill("stale"));
        assert_eq!(app.community_skills.len(), 1, "precondition");

        app.reload_community();

        assert!(!app.community_loading, "loading flag must flip back off");
        assert!(
            app.community_skills.is_empty(),
            "404 means owner mode — list must be cleared"
        );
        assert!(
            !app.community_error.is_empty(),
            "404 must surface a non-empty error message"
        );
        // Source string anchor: `market_tab.rs` literally emits
        // "owner 模式不暴露社区市场" for the 404 branch. Pin it so a
        // future copy-edit forces this test to be re-validated.
        assert!(
            app.community_error.contains("owner"),
            "404 branch must mention owner mode; got {:?}",
            app.community_error
        );
    });
}

#[test]
fn reload_community_populates_skills_on_200() {
    let tmp = TempDir::new().unwrap();
    let Some(listener) = try_lock_community_port() else {
        eprintln!("[skip] 127.0.0.1:17888 already in use; cannot stage canned 200");
        return;
    };
    // Server response shape: src/server/community.rs returns
    // {"skills":[{...}], "offset":0, ...}. Field set deserialized into
    // CommunitySkill (model.rs) — `name` required, others #[serde(default)].
    // `created_at` is `i64`, `version` is `String`.
    serve_canned_response(
        listener,
        "200 OK",
        r#"{"skills":[{"name":"alpha","uploader_uid":"u1","uploader_username":"alice","version":"1","installs_total":3,"created_at":17000000}]}"#,
    );

    with_sandbox(tmp.path(), || {
        let mut app = fresh_app(tmp.path());
        app.reload_community();

        assert!(!app.community_loading);
        assert_eq!(
            app.community_skills.len(),
            1,
            "200 with 1 row must populate skills; got {:?}",
            app.community_skills
                .iter()
                .map(|s| &s.name)
                .collect::<Vec<_>>()
        );
        assert_eq!(app.community_skills[0].name, "alpha");
        assert!(
            app.community_error.is_empty(),
            "successful fetch must clear the error string; got {:?}",
            app.community_error
        );
    });
}

#[test]
fn reload_community_reports_generic_http_error() {
    let tmp = TempDir::new().unwrap();
    let Some(listener) = try_lock_community_port() else {
        eprintln!("[skip] 127.0.0.1:17888 already in use; cannot stage canned 500");
        return;
    };
    serve_canned_response(listener, "500 Internal Server Error", "boom");

    with_sandbox(tmp.path(), || {
        let mut app = fresh_app(tmp.path());
        // Seed an existing row to confirm non-404 codes leave it alone
        // (source only clears community_skills on 404).
        app.community_skills.push(make_community_skill("preserved"));

        app.reload_community();

        assert!(!app.community_loading, "loading flips back on error too");
        assert!(
            app.community_error.contains("500"),
            "500 branch must mention the code; got {:?}",
            app.community_error
        );
        // Pre-existing rows preserved (this is the "show stale on
        // transient hiccup" promise in the module doc).
        assert_eq!(
            app.community_skills.len(),
            1,
            "non-404 errors keep previously-loaded rows"
        );
    });
}

// ─── feature 2: scan_upload_candidates ─ audit said `upload_picker_scan` ───
//
// Real signature: `pub fn scan_upload_candidates(&mut self)` — returns
// `()`. Scans `~/.claude/skills/<name>/` and `<cwd>/.claude/skills/<name>/`
// for subdirectories containing `SKILL.md`. Populates
// `App.upload_candidates`, clears `upload_message`, clamps `upload_idx`.

/// Build a fake skill dir at `<parent>/<name>/SKILL.md` so the scanner
/// counts it as a valid candidate (source guard: `path.join("SKILL.md").is_file()`).
fn mkskill(parent: &Path, name: &str) {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    std::fs::write(dir.join("SKILL.md"), "---\nname: x\n---\nbody").expect("write SKILL.md");
}

#[test]
fn scan_upload_candidates_finds_skills_under_claude_user_dir() {
    let tmp = TempDir::new().unwrap();
    with_sandbox(tmp.path(), || {
        let user_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&user_skills).unwrap();
        mkskill(&user_skills, "z-skill-alpha");
        mkskill(&user_skills, "a-skill-beta");
        // Dir without SKILL.md must be ignored.
        std::fs::create_dir_all(user_skills.join("not-a-skill")).unwrap();

        let mut app = fresh_app(tmp.path());
        app.scan_upload_candidates();

        let names: Vec<String> = app
            .upload_candidates
            .iter()
            .map(|c| c.name.clone())
            .collect();
        assert!(
            names.iter().any(|n| n == "z-skill-alpha") && names.iter().any(|n| n == "a-skill-beta"),
            "both well-formed skills must be discovered; got {names:?}"
        );
        assert!(
            !names.iter().any(|n| n == "not-a-skill"),
            "dirs without SKILL.md must be skipped; got {names:?}"
        );
        assert!(
            app.upload_message.is_empty(),
            "scan must clear stale upload_message; got {:?}",
            app.upload_message
        );
    });
}

#[test]
fn scan_upload_candidates_resets_state_when_no_source_dirs_exist() {
    let tmp = TempDir::new().unwrap();
    with_sandbox(tmp.path(), || {
        // Intentionally do NOT create ~/.claude/skills. The source has
        // `let Ok(rd) = read_dir(parent) else { return; }` so missing
        // parents collapse to zero rows rather than panicking. The cwd
        // scan may still find skills (cargo test cwd is the repo), so
        // we cannot assert == 0; we assert the post-scan invariants
        // hold instead.
        let mut app = fresh_app(tmp.path());
        // Pre-set upload_idx to an out-of-range value; the scan must
        // clamp it to len-1 (saturating_sub(1)).
        app.upload_idx = 999;
        // Pre-set upload_message to a stale string; the scan must clear it.
        app.upload_message = "stale message".to_string();

        app.scan_upload_candidates();

        let len = app.upload_candidates.len();
        assert!(
            app.upload_idx == 0 || app.upload_idx < len,
            "upload_idx must be clamped post-scan; got {} for {} candidates",
            app.upload_idx,
            len
        );
        assert!(
            app.upload_message.is_empty(),
            "scan must clear stale upload_message; got {:?}",
            app.upload_message
        );
    });
}

#[test]
fn scan_upload_candidates_produces_alpha_sort_within_user_source() {
    let tmp = TempDir::new().unwrap();
    with_sandbox(tmp.path(), || {
        let user_skills = tmp.path().join(".claude/skills");
        std::fs::create_dir_all(&user_skills).unwrap();
        // Insert in reverse-alpha order to force the sort to rearrange.
        mkskill(&user_skills, "zeta-skill");
        mkskill(&user_skills, "alpha-skill");
        mkskill(&user_skills, "mid-skill");

        let mut app = fresh_app(tmp.path());
        app.scan_upload_candidates();

        // Filter to just the ones we planted: their paths live under
        // our tmp HOME's `.claude/skills/`. The source enum
        // (`UploadSource`) is not pub-re-exported so we discriminate
        // via the path prefix instead.
        let user_names: Vec<String> = app
            .upload_candidates
            .iter()
            .filter(|c| c.path.starts_with(tmp.path().join(".claude/skills")))
            .map(|c| c.name.clone())
            .collect();

        let planted: Vec<&str> = user_names
            .iter()
            .filter(|n| matches!(n.as_str(), "zeta-skill" | "alpha-skill" | "mid-skill"))
            .map(|n| n.as_str())
            .collect();

        assert_eq!(
            planted.len(),
            3,
            "all three planted skills must appear in the candidate list; got {planted:?}"
        );
        // Within a single source group the source sort is
        // `a.name.cmp(&b.name)` after sorting by source label first
        // (USER < PROJECT). So our three rows must appear in alpha order
        // among the USER segment.
        let mut sorted = planted.clone();
        sorted.sort();
        assert_eq!(
            planted, sorted,
            "USER candidates must be sorted alpha; got {planted:?}"
        );
    });
}

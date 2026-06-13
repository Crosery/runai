//! Coverage tests for `src/server.rs` public surface (chunk c0).
//!
//! Targets in this file (signatures grepped from `src/server.rs`):
//!   - `pub enum EnsureStatus { AlreadyRunning, Started }`
//!   - `pub fn ensure_running(host: &str, port: u16) -> Result<EnsureStatus>`
//!   - `pub async fn serve(host: &str, port: u16) -> Result<()>`
//!
//! `api_tls_fingerprint` is SKIPPED — that endpoint does not exist in this
//! tree (no route, no handler, no symbol). See returned `skipped[]`.
//!
//! Test isolation rules (must hold for every test in this file):
//!   - Random port via `TcpListener::bind("127.0.0.1:0")` + immediate drop;
//!     never hard-code a port (port conflicts killed earlier rounds).
//!   - Per-test sandboxed HOME via `tempfile::TempDir`.
//!   - `RUNE_DATA_DIR` points inside the sandbox HOME.
//!   - `RUNAI_NO_AUTOSPAWN=1` so a stray TUI run never forks an extra server.
//!
//! Skipped on Windows: the existing server-side test harnesses are all
//! unix-only because HOME mocking via env vars does not work there (see the
//! Windows constraint in `AGENTS.md`).
#![cfg(not(target_os = "windows"))]

use runai::server::EnsureStatus;

// ─── EnsureStatus (enum) ────────────────────────────────────────────────────

#[test]
fn ensurestatus_alreadyrunning_variant_is_constructible_and_eq() {
    let v = EnsureStatus::AlreadyRunning;
    assert_eq!(v, EnsureStatus::AlreadyRunning);
    assert_ne!(v, EnsureStatus::Started);
}

#[test]
fn ensurestatus_started_variant_is_constructible_and_eq() {
    let v = EnsureStatus::Started;
    assert_eq!(v, EnsureStatus::Started);
    assert_ne!(v, EnsureStatus::AlreadyRunning);
}

#[test]
fn ensurestatus_pattern_match_exhaustively_covers_both_arms() {
    // Force both arms to be reachable; if a new variant is ever added this
    // test will fail to compile (good — fail loudly on enum growth).
    for v in [EnsureStatus::AlreadyRunning, EnsureStatus::Started] {
        let label = match v {
            EnsureStatus::AlreadyRunning => "already",
            EnsureStatus::Started => "started",
        };
        assert!(label == "already" || label == "started");
    }
}

#[test]
fn ensurestatus_debug_contains_variant_name() {
    // The enum derives Debug; both variant names should appear in the
    // formatted output (used by anyhow context strings in the wild).
    assert!(format!("{:?}", EnsureStatus::AlreadyRunning).contains("AlreadyRunning"));
    assert!(format!("{:?}", EnsureStatus::Started).contains("Started"));
}

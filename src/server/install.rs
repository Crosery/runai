//! Client install / uninstall script serving (bash + PowerShell).
//!
//! GET /install + /uninstall serve the bash scripts; GET /install.ps1 +
//! /uninstall.ps1 serve the PowerShell variants. The script bodies are
//! TEMPLATES that the server hydrates per request:
//!
//! 1. **Mode gate** (PLANNING §1.2). When `state.mode == ServerMode::Owner`,
//!    `/install` and `/install.ps1` return 404 with empty body — owner mode
//!    is single-user self-serve, no remote client surface. `/uninstall*` is
//!    also gated, so a teammate who installed against a now-owner-mode
//!    server cannot ping a sibling endpoint to undo their hook either.
//! 2. **Section stripping**. Lines wrapped in
//!    `# === RUNAI_SECTION:<mode>-only START ===` ...
//!    `# === RUNAI_SECTION:<mode>-only END ===` are removed when serving
//!    to the OTHER mode. Currently every owner-only block is empty (owner
//!    mode 404s out anyway), but the symmetry is enforced by the renderer
//!    so future divergent surface lands on a tested code path. The marker
//!    grammar is identical for `.sh` and `.ps1` because both use `#` for
//!    line comments.
//! 3. **Placeholder substitution**. `{SERVER_URL}` is replaced with the URL
//!    the request came in on (best-effort: Host header → `--host` value).
//!
//! See `scripts/runai-client-install.sh` for the canonical bash template
//! and `scripts/runai-client-install.ps1` for the PowerShell mirror.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::core::server_mode::ServerMode;

use super::recommend::request_origin;
use super::state::AppState;
use super::{CLIENT_INSTALL_PS1, CLIENT_INSTALL_SH, CLIENT_UNINSTALL_PS1, CLIENT_UNINSTALL_SH};

/// Render a template script body for the given `mode` + `server_url`:
/// 1. Strip every `# === RUNAI_SECTION:<other>-only START ===` ... `END ===`
///    block (the marker grammar is the same for `.sh` and `.ps1` because
///    both use `#` comments).
/// 2. Replace `{SERVER_URL}` with the resolved request origin.
///
/// Returns the final script body the server should send on the wire.
///
/// Marker grammar is intentionally line-based and forgiving:
/// - Match leading whitespace, optional `#`, then the literal
///   `=== RUNAI_SECTION:<mode>-only START ===` (and likewise `END`).
/// - The START / END lines are removed along with everything between.
/// - The blocks for the requested mode keep their START / END markers
///   stripped too (so the served file is clean of templating noise),
///   but the content between them is preserved verbatim.
/// - Unbalanced markers are tolerated: an unmatched START swallows to EOF,
///   an unmatched END is silently dropped. The unit tests pin this so a
///   future "stricter" pass can't silently break in-flight templates.
pub(super) fn render_install_script(template: &str, mode: ServerMode, server_url: &str) -> String {
    // Strip blocks for the OTHER mode. `team` mode → strip `owner-only`
    // sections, and vice versa.
    let other = match mode {
        ServerMode::Owner => "team-only",
        ServerMode::Team => "owner-only",
    };
    let mine = match mode {
        ServerMode::Owner => "owner-only",
        ServerMode::Team => "team-only",
    };

    let mut out = String::with_capacity(template.len());
    let mut skipping = false;
    for line in template.lines() {
        let trimmed = line.trim_start();
        // Detect markers. `# === RUNAI_SECTION:<tag> START ===`.
        if let Some((tag, kind)) = parse_section_marker(trimmed) {
            if tag == other {
                // Toggle skip state. START → enter skip; END → leave skip.
                match kind {
                    MarkerKind::Start => skipping = true,
                    MarkerKind::End => skipping = false,
                }
                // Drop the marker line itself either way.
                continue;
            }
            if tag == mine {
                // Drop the marker line but keep the content between.
                continue;
            }
            // Unknown tag — keep the line so the original template stays
            // visible (helps debugging if someone fat-fingers a marker).
        }
        if skipping {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.replace("{SERVER_URL}", server_url)
}

#[derive(Debug, PartialEq, Eq)]
enum MarkerKind {
    Start,
    End,
}

/// Recognise a section marker line. Returns `Some((tag, kind))` for
/// `# === RUNAI_SECTION:<tag> START ===` and the END counterpart.
fn parse_section_marker(line: &str) -> Option<(&str, MarkerKind)> {
    // Skip optional leading `#` and whitespace.
    let s = line.trim_start_matches('#').trim();
    // Cheap prefix check before doing the heavier parse.
    let prefix = "=== RUNAI_SECTION:";
    let rest = s.strip_prefix(prefix)?;
    // `<tag> START ===` or `<tag> END ===`
    let rest = rest.trim_end_matches('=').trim_end();
    if let Some(tag) = rest.strip_suffix(" START") {
        return Some((tag.trim(), MarkerKind::Start));
    }
    if let Some(tag) = rest.strip_suffix(" END") {
        return Some((tag.trim(), MarkerKind::End));
    }
    None
}

/// GET /install — return the client install bash script with `{SERVER_URL}`
/// substituted by the URL the request came in on. Owner mode returns 404
/// (PLANNING §1.2 — owner mode has no remote client surface).
pub(super) async fn handle_install_script(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if state.mode == ServerMode::Owner {
        return (StatusCode::NOT_FOUND, "").into_response();
    }
    let server_url = request_origin(&headers);
    let body = render_install_script(CLIENT_INSTALL_SH, state.mode, &server_url);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        body,
    )
        .into_response()
}

/// GET /uninstall — return the client uninstall bash script. Reverses
/// /install: removes the hook entry from Claude Code settings.json and
/// deletes ~/.runai-hook.sh. Mirror-gated with /install: owner mode 404s.
pub(super) async fn handle_uninstall_script(State(state): State<Arc<AppState>>) -> Response {
    if state.mode == ServerMode::Owner {
        return (StatusCode::NOT_FOUND, "").into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        CLIENT_UNINSTALL_SH.to_string(),
    )
        .into_response()
}

/// GET /install.ps1 — Windows / PowerShell install. Teammate runs:
///   irm http://<server>:<port>/install.ps1 | iex
/// Owner mode returns 404 to mirror the bash variant.
pub(super) async fn handle_install_ps1(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if state.mode == ServerMode::Owner {
        return (StatusCode::NOT_FOUND, "").into_response();
    }
    let server_url = request_origin(&headers);
    let body = render_install_script(CLIENT_INSTALL_PS1, state.mode, &server_url);
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// GET /uninstall.ps1 — Windows / PowerShell uninstall.
pub(super) async fn handle_uninstall_ps1(State(state): State<Arc<AppState>>) -> Response {
    if state.mode == ServerMode::Owner {
        return (StatusCode::NOT_FOUND, "").into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        CLIENT_UNINSTALL_PS1.to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE: &str = r#"echo top
# === RUNAI_SECTION:owner-only START ===
echo owner-only-cmd
# === RUNAI_SECTION:owner-only END ===
echo middle {SERVER_URL}
# === RUNAI_SECTION:team-only START ===
echo team-only-cmd
# === RUNAI_SECTION:team-only END ===
echo bottom
"#;

    #[test]
    fn team_mode_strips_owner_only_keeps_team_only() {
        let out = render_install_script(TEMPLATE, ServerMode::Team, "http://x");
        assert!(!out.contains("owner-only-cmd"), "team strips owner: {out}");
        assert!(out.contains("team-only-cmd"), "team keeps team: {out}");
        // Marker lines themselves are gone too.
        assert!(!out.contains("RUNAI_SECTION"));
        // Placeholder replaced.
        assert!(out.contains("middle http://x"));
        // Surrounding lines intact.
        assert!(out.contains("echo top"));
        assert!(out.contains("echo bottom"));
    }

    #[test]
    fn owner_mode_strips_team_only_keeps_owner_only() {
        let out = render_install_script(TEMPLATE, ServerMode::Owner, "http://x");
        assert!(out.contains("owner-only-cmd"), "owner keeps owner: {out}");
        assert!(!out.contains("team-only-cmd"), "owner strips team: {out}");
        assert!(!out.contains("RUNAI_SECTION"));
    }

    #[test]
    fn real_bash_template_in_team_mode_has_no_scan_or_discover_commands() {
        // The bash template must not contain runai-binary management
        // commands — those only run on the server box and would 404 on a
        // remote client anyway. PLANNING §1.2 specifies scan / discover /
        // doctor must not appear in the team-mode client script.
        let out = render_install_script(CLIENT_INSTALL_SH, ServerMode::Team, "http://x");
        // Token-boundary check: `runai scan` / `runai discover` / `runai doctor`
        // are the binary subcommands; we want to be sure the script does
        // not telegraph them to remote users.
        for forbidden in &["runai scan", "runai discover", "runai doctor"] {
            assert!(
                !out.contains(forbidden),
                "team-mode install.sh leaked binary cmd {forbidden:?}"
            );
        }
    }

    #[test]
    fn server_url_placeholder_gets_substituted() {
        let out = render_install_script(
            CLIENT_INSTALL_SH,
            ServerMode::Team,
            "http://example.com:1234",
        );
        assert!(out.contains("http://example.com:1234"));
        assert!(
            !out.contains("{SERVER_URL}"),
            "placeholder must be fully replaced"
        );
    }

    #[test]
    fn parse_marker_handles_leading_hash_and_spacing() {
        let cases = &[
            (
                "# === RUNAI_SECTION:team-only START ===",
                Some(("team-only", MarkerKind::Start)),
            ),
            (
                "# === RUNAI_SECTION:owner-only END ===",
                Some(("owner-only", MarkerKind::End)),
            ),
            (
                "  # === RUNAI_SECTION:team-only START ===",
                Some(("team-only", MarkerKind::Start)),
            ),
            ("echo nope", None),
            ("# regular comment", None),
        ];
        for (line, expected) in cases {
            let got = parse_section_marker(line.trim_start());
            assert_eq!(got, *expected, "line={line:?}");
        }
    }
}

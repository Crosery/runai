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
//! The team-mode bash + PowerShell install templates wire the full
//! "runai-client 三件套" (PLANNING §1.6): the UserPromptSubmit hook, the
//! `runai-client` companion CLI, and the remote HTTP MCP. The MCP leg
//! registers a `runai-client` entry in `~/.claude.json`'s `mcpServers`
//! (`type:http`, `url:<SERVER_URL>/mcp`, `Authorization: Bearer <api_key>`
//! from `~/.runai-identity`) so Claude Code reaches the server's
//! streamable-HTTP MCP (`mcp_http.rs`). `{SERVER_URL}` substitution in the
//! template renderer only touches the bare `{SERVER_URL}` placeholder — the
//! scripts deliberately use the runtime `$SERVER_URL` / `$ServerUrl` shell
//! variable (no braces) inside the MCP block so the renderer never rewrites
//! the substring inside `${...}`. The uninstall scripts symmetrically drop
//! only the `runai-client` mcpServers key, preserving sibling entries. The
//! companion CLI subcommands that call server APIs must fail non-zero and print
//! the HTTP status + response body on non-2xx responses, and a stable
//! subcommand-prefixed transport error when the request cannot be sent at all;
//! curl and PowerShell defaults are not enough because they can hide the
//! failing status or produce version-dependent text. When the rendered server
//! URL is HTTPS, both install templates persist `~/.runai-server.json`, and
//! both the hook wrapper and companion CLI verify `/api/tls/fingerprint`
//! before forwarding any prompt or companion API request.
//!
//! See `scripts/runai-client-install.sh` for the canonical bash template
//! and `scripts/runai-client-install.ps1` for the PowerShell mirror.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::core::server_mode::ServerMode;

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
    uri: Uri,
) -> Response {
    if state.mode == ServerMode::Owner {
        return (StatusCode::NOT_FOUND, "").into_response();
    }
    let server_url = state.public_server_url(&headers, &uri);
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
    uri: Uri,
) -> Response {
    if state.mode == ServerMode::Owner {
        return (StatusCode::NOT_FOUND, "").into_response();
    }
    let server_url = state.public_server_url(&headers, &uri);
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
    fn real_install_templates_in_team_mode_have_no_server_box_commands() {
        // The client templates must not contain runai-binary management
        // commands — those only run on the server box and would 404 on a
        // remote client anyway. PLANNING §1.2 specifies scan / discover /
        // doctor must not appear in team-mode client scripts.
        for (name, template) in [
            ("bash", CLIENT_INSTALL_SH),
            ("powershell", CLIENT_INSTALL_PS1),
        ] {
            let out = render_install_script(template, ServerMode::Team, "http://x");
            // Token-boundary check: `runai scan` / `runai discover` /
            // `runai doctor` are the binary subcommands; we want to be sure
            // the script does not telegraph them to remote users.
            for forbidden in &["runai scan", "runai discover", "runai doctor"] {
                assert!(
                    !out.contains(forbidden),
                    "team-mode {name} install template leaked binary cmd {forbidden:?}"
                );
            }
        }
    }

    #[test]
    fn server_url_placeholder_gets_substituted() {
        for (name, template) in [
            ("bash", CLIENT_INSTALL_SH),
            ("powershell", CLIENT_INSTALL_PS1),
        ] {
            let out = render_install_script(template, ServerMode::Team, "http://example.com:1234");
            assert!(
                out.contains("http://example.com:1234"),
                "{name} template missing substituted server URL"
            );
            assert!(
                !out.contains("{SERVER_URL}"),
                "{name} placeholder must be fully replaced"
            );
        }
    }

    #[test]
    fn bash_template_companion_cli_wraps_server_errors() {
        let out = render_install_script(CLIENT_INSTALL_SH, ServerMode::Team, "http://x");
        for required in [
            "runai_curl()",
            "request failed (curl exit $rc)",
            "runai-client install: server returned HTTP $HTTP",
            "runai-client upload: server returned HTTP $HTTP",
            "runai-client list: server returned HTTP $HTTP",
            r#"RESP=$(runai_curl "runai-client install""#,
            r#"RESP=$(runai_curl "runai-client upload""#,
            r#"RESP=$(runai_curl "runai-client list""#,
        ] {
            assert!(
                out.contains(required),
                "bash install template missing companion CLI error contract: {required}"
            );
        }
    }

    #[test]
    fn powershell_template_installs_companion_cli_surface() {
        let out = render_install_script(CLIENT_INSTALL_PS1, ServerMode::Team, "http://x");
        for required in [
            "function Resolve-RunaiProfileRoot",
            r#"$RunaiProfileRoot = Resolve-RunaiProfileRoot"#,
            r#"$RunaiClientPath = Join-Path (Join-Path $RunaiProfileRoot ".local\bin") "runai-client.ps1""#,
            r#"$RunaiClientShimPath = Join-Path (Join-Path $RunaiProfileRoot ".local\bin") "runai-client.cmd""#,
            "install runai-client companion",
            "Invoke-RunaiList",
            "Invoke-RunaiInstall",
            "Invoke-RunaiUpload",
            "Invoke-RunaiGet",
            "Get-RunaiHttpErrorMessage",
            "server returned HTTP",
            r#"-ErrorPrefix "runai-client install""#,
            r#"-ErrorPrefix "runai-client list""#,
            r#"-Prefix "runai-client upload""#,
            "/api/community/list",
            "/api/community/install/",
            "/api/community/upload",
            "/skills/bundle/",
            "RUNAI_LOCAL_SKILLS",
            ".runai-local-skills",
            "Test-RunaiLocalManifestContains",
            "refusing to overwrite untracked local skill",
            "AppendAllText",
            "[System.Text.UTF8Encoding]::new($false)",
            r#"[System.IO.File]::WriteAllText($RunaiClientPath"#,
            r#"[System.IO.File]::WriteAllText($RunaiClientShimPath"#,
            r#"powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0runai-client.ps1" %*"#,
        ] {
            assert!(
                out.contains(required),
                "PowerShell install template missing companion CLI contract: {required}"
            );
        }
        assert!(
            !out.contains(
                "if (Test-Path -LiteralPath $dest) {\n                Remove-Item -LiteralPath $dest -Recurse -Force"
            ),
            "PowerShell get must not delete an existing local skill before checking the local manifest"
        );
    }

    #[test]
    fn powershell_template_reuses_verified_identity_and_disables_raw_ansi() {
        let out = render_install_script(CLIENT_INSTALL_PS1, ServerMode::Team, "http://x");
        for required in [
            "Test-RunaiIdentityWithServer",
            r#"$me = Invoke-RestMethod -Method Get -Uri "$ServerUrl/api/me" -Headers $headers"#,
            "server accepted stored api_key",
            "existing identity was rejected",
            "$RunaiAnsi = $false",
            "$Esc = [char]27",
            "function Runai-Style",
            "function Stop-RunaiInstall",
            r#"$env:NO_COLOR"#,
        ] {
            assert!(
                out.contains(required),
                "PowerShell install template missing identity/ANSI contract: {required}"
            );
        }
        assert!(
            !out.contains("`e["),
            "PowerShell template must not use PS7-only raw `e ANSI escapes"
        );
        for forbidden in [
            "Write-Fail2 \"existing identity was rejected by $ServerUrl\"\n            exit 1",
            "Write-Fail2 \"username cannot be empty",
            "Write-Fail2 \"password cannot be empty",
        ] {
            assert!(
                !out.contains(forbidden),
                "PowerShell installer should throw instead of exiting the host shell: {forbidden}"
            );
        }
    }

    #[test]
    fn powershell_templates_are_ascii_safe_for_windows_powershell_51() {
        for (name, template) in [
            ("install.ps1", CLIENT_INSTALL_PS1),
            ("uninstall.ps1", CLIENT_UNINSTALL_PS1),
        ] {
            let out = render_install_script(template, ServerMode::Team, "http://x");
            assert!(
                out.is_ascii(),
                "{name} must stay ASCII-only because Windows PowerShell 5.1 can parse UTF-8 no-BOM scripts as ANSI"
            );
        }
    }

    #[test]
    fn client_templates_pin_https_fingerprints_before_remote_calls() {
        let bash = render_install_script(CLIENT_INSTALL_SH, ServerMode::Team, "https://x");
        for required in [
            r#"SERVER_PIN_PATH="$HOME/.runai-server.json""#,
            "pin_server_fingerprint()",
            "if [[ \"$DO_AUTH\" -eq 1 || \"$DO_HOOK\" -eq 1 ]]",
            "/api/tls/fingerprint",
            "'scheme': 'https'",
            "runai-hook: missing HTTPS fingerprint pin",
            "runai-hook: server fingerprint mismatch",
            "RUNAI_SERVER_PIN=\"$HOME/.runai-server.json\"",
            "verify_server_pin()",
            "verify_server_pin",
            "runai-client: server fingerprint mismatch",
            "CURL_TLS=\"--insecure\"",
        ] {
            assert!(
                bash.contains(required),
                "bash install template missing TLS pin contract: {required}"
            );
        }
        assert!(
            bash.find("verify_server_pin").unwrap() < bash.find("runai_curl()").unwrap(),
            "bash runai-client must define/route pin verification before issuing API curls"
        );

        let ps = render_install_script(CLIENT_INSTALL_PS1, ServerMode::Team, "https://x");
        for required in [
            r#"$ServerPinPath = Join-Path $RunaiProfileRoot ".runai-server.json""#,
            "function Write-RunaiServerPin",
            "if ($DoAuth -or $DoHook)",
            "/api/tls/fingerprint",
            r#"scheme      = "https""#,
            r#"`$RunaiServer = "$ServerUrl""#,
            r#"`$RunaiServerPin = Join-Path `$RunaiProfileRoot ".runai-server.json""#,
            "function Test-RunaiServerPin",
            "runai-hook: server fingerprint mismatch",
            "if (-not (Test-RunaiServerPin)) { exit 1 }",
            r#"$RunaiServerPinPath = Join-Path $RunaiProfileRoot ".runai-server.json""#,
            "function Assert-RunaiServerPin",
            "Assert-RunaiServerPin $server",
            "server fingerprint mismatch - refusing to contact",
            "$PSDefaultParameterValues['Invoke-RestMethod:SkipCertificateCheck'] = $true",
        ] {
            assert!(
                ps.contains(required),
                "PowerShell install template missing TLS pin contract: {required}"
            );
        }
        assert!(
            ps.find("if (-not (Test-RunaiServerPin)) { exit 1 }")
                .unwrap()
                < ps.find(r#"-Uri "`$RunaiServer/recommend""#).unwrap(),
            "PowerShell hook must verify the pin before forwarding /recommend"
        );
        assert!(
            ps.find("Assert-RunaiServerPin $server").unwrap() < ps.find("return $server").unwrap(),
            "PowerShell companion CLI must verify the pin before returning a usable server"
        );
    }

    #[test]
    fn powershell_uninstall_cleans_companion_cli_and_local_manifest() {
        for required in [
            "function Resolve-RunaiProfileRoot",
            r#"$RunaiProfileRoot = Resolve-RunaiProfileRoot"#,
            r#"$RunaiClientPath = Join-Path (Join-Path $RunaiProfileRoot ".local\bin") "runai-client.ps1""#,
            r#"$RunaiClientShimPath = Join-Path (Join-Path $RunaiProfileRoot ".local\bin") "runai-client.cmd""#,
            r#"$ServerPinPath = Join-Path $RunaiProfileRoot ".runai-server.json""#,
            r#"$LocalManifestPath = Join-Path $RunaiProfileRoot ".runai-local-skills""#,
            "Test-RunaiSafeSkillName",
            "Get-RunaiTargetDir",
            "Remove-Item -LiteralPath $ServerPinPath -Force",
            "Remove-Item -LiteralPath $skillDir -Recurse -Force",
            "Remove-Item -LiteralPath $LocalManifestPath -Force",
            "Remove-Item -LiteralPath $RunaiClientPath -Force",
            "Remove-Item -LiteralPath $RunaiClientShimPath -Force",
        ] {
            assert!(
                CLIENT_UNINSTALL_PS1.contains(required),
                "PowerShell uninstall template missing cleanup contract: {required}"
            );
        }
        assert!(
            !CLIENT_UNINSTALL_PS1.contains("Get-ChildItem -Recurse"),
            "PowerShell uninstall must not scan user skill trees"
        );
    }

    #[test]
    fn install_templates_emit_machine_parseable_completion() {
        for (name, template, marker) in [
            ("bash", CLIENT_INSTALL_SH, "printf \"install complete\\n\""),
            (
                "powershell",
                CLIENT_INSTALL_PS1,
                "Write-Host \"install complete\"",
            ),
        ] {
            let out = render_install_script(template, ServerMode::Team, "http://x");
            assert!(
                out.contains(marker),
                "{name} template must emit a stable install-complete line"
            );
            for field in [
                "account", "password", "api_key", "server", "identity", "hook", "config", "client",
            ] {
                assert!(out.contains(field), "{name} summary missing {field}");
            }
        }
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

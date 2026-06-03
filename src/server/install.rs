//! Client install / uninstall script serving (bash + PowerShell).
//!
//! GET /install + /uninstall serve the bash scripts; GET /install.ps1 +
//! /uninstall.ps1 serve the PowerShell variants. The `{SERVER_URL}`
//! placeholder is substituted with the URL the request came in on so the
//! resulting script already knows where to point the hook wrapper.

use axum::{
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};

use super::recommend::guess_server_url;
use super::{CLIENT_INSTALL_PS1, CLIENT_INSTALL_SH, CLIENT_UNINSTALL_PS1, CLIENT_UNINSTALL_SH};

/// GET /install — return the client install bash script with `{SERVER_URL}`
/// substituted by the URL the request came in on. Teammate runs:
///   curl -fsSL http://<server>:<port>/install | bash
pub(super) async fn handle_install_script(headers: HeaderMap) -> Response {
    let server_url = guess_server_url(&headers);
    let body = CLIENT_INSTALL_SH.replace("{SERVER_URL}", &server_url);
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        body,
    )
        .into_response()
}

/// GET /uninstall — return the client uninstall bash script. Reverses
/// /install: removes the hook entry from Claude Code settings.json and
/// deletes ~/.runai-hook.sh.
pub(super) async fn handle_uninstall_script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        CLIENT_UNINSTALL_SH.to_string(),
    )
        .into_response()
}

/// GET /install.ps1 — Windows / PowerShell install. Teammate runs:
///   irm http://<server>:<port>/install.ps1 | iex
pub(super) async fn handle_install_ps1(headers: HeaderMap) -> Response {
    let server_url = guess_server_url(&headers);
    let body = CLIENT_INSTALL_PS1.replace("{SERVER_URL}", &server_url);
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], body).into_response()
}

/// GET /uninstall.ps1 — Windows / PowerShell uninstall.
pub(super) async fn handle_uninstall_ps1() -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        CLIENT_UNINSTALL_PS1.to_string(),
    )
        .into_response()
}

//! HTTP dashboard for router telemetry.
//!
//! Spawned by `runai server [--port N] [--host H]`. Reads `~/.runai/runai.db`
//! and serves a single-page HTML dashboard plus JSON endpoints so users can
//! inspect every hook invocation: the user prompt, cwd, chosen skills, BM25
//! prefilter ratio, latency and token usage.
//!
//! No external CDN — index.html / app.js / app.css are bundled via
//! `include_str!` so the dashboard works offline (same single-binary
//! philosophy as the rest of runai).
//!
//! This module is split into one file per route family; the thin `mod.rs`
//! holds only the bundled web-asset consts + the public re-exports.

mod admin;
mod app;
mod auth;
mod error;
mod install;
mod library;
mod market;
mod market_github;
mod market_preview;
mod prefs;
mod recommend;
mod skills;
mod state;
mod telemetry;

pub use app::{EnsureStatus, ensure_running, serve};

const INDEX_HTML: &str = include_str!("../../web/index.html");
const APP_JS: &str = include_str!("../../web/app.js");
const APP_CSS: &str = include_str!("../../web/app.css");
/// Client-side install / uninstall scripts. The server serves these from
/// GET /install and GET /uninstall after replacing the `{SERVER_URL}`
/// placeholder with the URL the teammate just curl'd from, so the
/// resulting bash script already knows where to point the hook wrapper.
/// See scripts/runai-client-install.sh for the full doc.
const CLIENT_INSTALL_SH: &str = include_str!("../../scripts/runai-client-install.sh");
const CLIENT_UNINSTALL_SH: &str = include_str!("../../scripts/runai-client-uninstall.sh");
/// Windows / PowerShell variants of install / uninstall scripts. Served
/// from GET /install.ps1 + /uninstall.ps1 so teammates on Windows can run
/// `irm http://<server>/install.ps1 | iex` (PowerShell equivalent of
/// `curl ... | bash`). Same {SERVER_URL} placeholder substitution.
const CLIENT_INSTALL_PS1: &str = include_str!("../../scripts/runai-client-install.ps1");
const CLIENT_UNINSTALL_PS1: &str = include_str!("../../scripts/runai-client-uninstall.ps1");

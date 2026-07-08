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
mod community;
mod enrich_state;
mod error;
mod install;
mod library;
mod market;
mod market_github;
mod market_preview;
mod middleware;
mod prefs;
mod private_upload;
mod recommend;
mod skills;
mod state;
mod telemetry;
mod tls;

pub use app::{EnsureStatus, ensure_running, serve, serve_with};

const INDEX_HTML: &str = include_str!("../../web/index.html");
/// Setup tab content (PLANNING §1.7). Authored as plain markdown so the
/// content stays version-controlled + diffable; the JS in `18-setup-tab.js`
/// fetches `/setup.md`, renders to HTML, and attaches per-block copy
/// buttons so users can grab CLI commands without selecting text.
const SETUP_MD: &str = include_str!("../../web/setup.md");
// app.js / app.css are split into safe-boundary parts under web/{js,css}/.
// concat! over include_str! in sorted order rebuilds the exact byte stream the
// dashboard used to serve — the served /app.js and /app.css routes stay
// byte-identical to the pre-split single files. See web/README.md for the
// invariant: cut only at safe boundaries, never reorder, concat! here.
const APP_JS: &str = concat!(
    include_str!("../../web/js/01-iife-state-formatters.js"),
    include_str!("../../web/js/02-router.js"),
    include_str!("../../web/js/03-api-overview.js"),
    include_str!("../../web/js/04-activity-detail.js"),
    include_str!("../../web/js/05-library-skills.js"),
    include_str!("../../web/js/06-library-detail.js"),
    include_str!("../../web/js/07-polling-models.js"),
    include_str!("../../web/js/08-dropdown-swatches-cursor.js"),
    include_str!("../../web/js/09-wiring.js"),
    include_str!("../../web/js/10-settings.js"),
    include_str!("../../web/js/11-account-library.js"),
    include_str!("../../web/js/12-admin-scope-skills.js"),
    include_str!("../../web/js/13-auth-bulk-prefs.js"),
    include_str!("../../web/js/14-market-list.js"),
    include_str!("../../web/js/15-market-detail-github.js"),
    include_str!("../../web/js/17-community-market.js"),
    include_str!("../../web/js/18-setup-tab.js"),
    include_str!("../../web/js/19-admin-publish-review.js"),
    include_str!("../../web/js/20-skill-radar.js"),
    include_str!("../../web/js/16-boot.js"),
);
const APP_CSS: &str = concat!(
    include_str!("../../web/css/01-base-themes.css"),
    include_str!("../../web/css/02-mesh-cursor.css"),
    include_str!("../../web/css/03-layout-topbar-sidebar.css"),
    include_str!("../../web/css/04-overview-tab.css"),
    include_str!("../../web/css/05-library-dropdown.css"),
    include_str!("../../web/css/06-skill-rows.css"),
    include_str!("../../web/css/07-skill-detail.css"),
    include_str!("../../web/css/08-event-dialog.css"),
    include_str!("../../web/css/09-settings-tab.css"),
    include_str!("../../web/css/10-v15-account-auth-library.css"),
    include_str!("../../web/css/11-v15-market-github.css"),
    include_str!("../../web/css/12-community-market.css"),
    include_str!("../../web/css/13-owner-mode.css"),
    include_str!("../../web/css/14-userlib-admin.css"),
    include_str!("../../web/css/15-setup-tab.css"),
    include_str!("../../web/css/16-skill-radar.css"),
);
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

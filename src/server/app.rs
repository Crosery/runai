//! Server bootstrap: `serve()` router build, the idempotent `ensure_running`
//! spawn helper, the static-asset serving, and the per-boot cache-buster.

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    http::header,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::core::paths::AppPaths;

use super::admin::{api_admin_users_delete, api_admin_users_list, api_admin_users_update};
use super::auth::{api_login, api_logout, api_me, api_register};
use super::install::{
    handle_install_ps1, handle_install_script, handle_uninstall_ps1, handle_uninstall_script,
};
use super::library::{
    api_library_clear, api_library_fill, api_library_import_from_usage, api_library_list,
    api_library_mutate,
};
use super::market::{api_market_install, api_market_list, api_market_refresh};
use super::market_github::{api_install_github, api_parse_github};
use super::market_preview::{api_market_preview, api_market_preview_files};
use super::prefs::{
    api_activate_provider, api_delete_provider, api_get_prefs, api_get_settings, api_post_prefs,
    api_post_settings, api_upsert_provider,
};
use super::recommend::{handle_feedback, handle_recommend};
use super::skills::{
    api_skill_detail, api_skill_file, api_skill_files, api_skills, handle_skill_bundle,
    handle_skill_file, handle_skill_get,
};
use super::state::AppState;
use super::telemetry::{api_event_by_id, api_events, api_summary, api_timeline};
use super::{APP_CSS, APP_JS, INDEX_HTML};

/// Result of `ensure_running`. `AlreadyRunning` is the hot path for repeat
/// invocations (hook / SessionStart); `Started` happens once per machine boot.
#[derive(Debug, PartialEq, Eq)]
pub enum EnsureStatus {
    AlreadyRunning,
    Started,
}

/// Idempotent "is the dashboard up? if not, spawn it" helper. Designed to be
/// called from Claude Code's SessionStart hook (or any shell rc) so the
/// dashboard is always reachable without the user remembering to start it.
///
/// Behavior:
/// - If we can TCP-connect to `host:port` within 200ms → return `AlreadyRunning`.
///   This is the steady-state hot path (< 50ms total).
/// - Otherwise spawn `runai server --port P --host H` as a detached child with
///   stdio nullified, then poll the port for up to ~2s and return `Started`
///   when it comes up. Returns an error only if the spawn itself fails or the
///   server never binds.
///
/// The detached child becomes an orphan when this process exits and is
/// reparented to init (PID 1), which keeps the server running across the
/// lifetime of the launching shell / Claude Code session.
pub fn ensure_running(host: &str, port: u16) -> Result<EnsureStatus> {
    use std::net::TcpStream;
    use std::time::Duration;

    let addr_str = format!("{host}:{port}");
    let sock: SocketAddr = addr_str
        .parse()
        .with_context(|| format!("parse {addr_str}"))?;
    if TcpStream::connect_timeout(&sock, Duration::from_millis(200)).is_ok() {
        return Ok(EnsureStatus::AlreadyRunning);
    }

    let exe = std::env::current_exe().context("locate runai binary via current_exe")?;
    std::process::Command::new(&exe)
        .arg("server")
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg(host)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("spawn `{}` server daemon", exe.display()))?;

    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        if TcpStream::connect_timeout(&sock, Duration::from_millis(100)).is_ok() {
            return Ok(EnsureStatus::Started);
        }
    }
    bail!("started runai server daemon but {addr_str} did not respond within 2s")
}

pub async fn serve(host: &str, port: u16) -> Result<()> {
    let paths = AppPaths::default_path();
    let state = Arc::new(AppState {
        db_path: paths.db_path(),
    });

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/app.js", get(serve_app_js))
        .route("/app.css", get(serve_app_css))
        .route("/api/summary", get(api_summary))
        .route("/api/timeline", get(api_timeline))
        .route("/api/events", get(api_events))
        .route("/api/event/{id}", get(api_event_by_id))
        .route("/api/skills", get(api_skills))
        .route("/api/skill/{name}", get(api_skill_detail))
        .route("/api/skill/{name}/files", get(api_skill_files))
        .route("/api/skill/{name}/file", get(api_skill_file))
        // Remote-hook protocol: teammates' Claude Code UserPromptSubmit hooks
        // POST their standard hook JSON here and pipe stdout back into the
        // agent. See scripts/runai-client-install.sh for the wrapper they run.
        .route("/recommend", post(handle_recommend))
        .route("/skills/get/{name}", post(handle_skill_get))
        // GET /skills/file/{name}/{*path} — raw file body for any path
        // inside a skill directory. Acts as a Read-tool replacement so
        // remote teammates can fetch references/X.md, scripts/Y.py, etc.
        // referenced from SKILL.md without having the binary or the
        // skill files on disk locally.
        .route("/skills/file/{name}/{*path}", get(handle_skill_file))
        // GET /skills/bundle/{name} — gzipped tarball of the whole skill
        // directory (SKILL.md + scripts/ + references/ + anything else).
        // Client install scripts and hook wrappers fetch this once and
        // unpack into a local cache so SKILL.md's `scripts/foo.sh`
        // references work without round-tripping every file.
        .route("/skills/bundle/{name}", get(handle_skill_bundle))
        .route("/feedback", post(handle_feedback))
        .route("/install", get(handle_install_script))
        .route("/uninstall", get(handle_uninstall_script))
        .route("/install.ps1", get(handle_install_ps1))
        .route("/uninstall.ps1", get(handle_uninstall_ps1))
        // Settings — recommend config + providers CRUD for the dashboard
        // Settings tab. All endpoints return / accept JSON; api_key bytes
        // are never sent back to the browser (only `has_api_key: bool`).
        // v15: admin-gated (compat: open when no users exist yet).
        .route("/api/settings", get(api_get_settings).post(api_post_settings))
        .route("/api/providers", post(api_upsert_provider))
        .route("/api/providers/{id}", delete(api_delete_provider))
        .route("/api/providers/{id}/activate", post(api_activate_provider))
        // v15 admin: per-user management (list / promote / disable)
        .route("/api/admin/users", get(api_admin_users_list))
        .route(
            "/api/admin/users/{user_id}",
            post(api_admin_users_update).delete(api_admin_users_delete),
        )
        // v15 marketplace: any logged-in user can browse / install. Newly
        // installed skills go to the public pool AND are auto-subscribed
        // to the installing user's library.
        .route("/api/market", get(api_market_list))
        .route("/api/market/refresh", post(api_market_refresh))
        .route("/api/market/preview", get(api_market_preview))
        .route("/api/market/preview-files", get(api_market_preview_files))
        .route("/api/market/install", post(api_market_install))
        .route("/api/install/github", post(api_install_github))
        .route("/api/parse/github", post(api_parse_github))
        // ---- v15 multi-user (auth + per-user library + per-user prefs) ----
        .route("/users/register", post(api_register))
        .route("/auth/login", post(api_login))
        .route("/auth/logout", post(api_logout))
        .route("/api/me", get(api_me))
        .route("/api/prefs", get(api_get_prefs).post(api_post_prefs))
        .route(
            "/api/skills/library",
            get(api_library_list).post(api_library_mutate),
        )
        .route("/api/skills/library/clear", post(api_library_clear))
        .route("/api/skills/library/fill", post(api_library_fill))
        .route(
            "/api/skills/library/import-from-usage",
            post(api_library_import_from_usage),
        )
        .with_state(state);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("parse {host}:{port}"))?;
    println!("runai dashboard at http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind {addr}"))?;
    axum::serve(listener, app).await.context("axum::serve")?;
    Ok(())
}

/// Process-lifetime cache-buster. Generated once when the server boots from
/// the current unix timestamp; injected into every `<link href="...">` /
/// `<script src="...">` URL in `index.html`. Every `runai server` restart
/// produces a fresh value, so the browser sees a different URL for the CSS
/// and JS and is forced to fetch the new bytes — no Cmd+Shift+R needed even
/// the first time after upgrade.
static BUILD_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
fn build_id() -> &'static str {
    BUILD_ID.get_or_init(|| chrono::Utc::now().timestamp().to_string())
}

async fn serve_index() -> Response {
    // Rewrite static asset URLs to include the per-boot build_id query
    // string so cached entries from a prior server boot can never satisfy
    // a request for this boot's assets.
    let bid = build_id();
    let patched = INDEX_HTML
        .replace("\"/app.css\"", &format!("\"/app.css?v={bid}\""))
        .replace("\"/app.js\"", &format!("\"/app.js?v={bid}\""));
    dynamic_response(patched, "text/html; charset=utf-8")
}
async fn serve_app_js() -> Response {
    static_response(APP_JS, "application/javascript; charset=utf-8")
}
async fn serve_app_css() -> Response {
    static_response(APP_CSS, "text/css; charset=utf-8")
}

fn static_response(body: &'static str, content_type: &'static str) -> Response {
    // `no-store` + must-revalidate: assets are bundled into the binary via
    // `include_str!` so the only way they change is when the binary is
    // rebuilt. Cache-Control = no-store stops the browser from reading its
    // disk cache without revalidating; the cache-busting query string in
    // `serve_index` is the belt-and-braces defense that handles browsers
    // that ignored a no-store directive on prior responses.
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store, must-revalidate"),
        ],
        body.to_string(),
    )
        .into_response()
}

fn dynamic_response(body: String, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store, must-revalidate"),
        ],
        body,
    )
        .into_response()
}

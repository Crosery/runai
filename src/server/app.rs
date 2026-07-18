//! Server bootstrap: `serve()` router build, the idempotent `ensure_running`
//! spawn helper, the static-asset serving, and the per-boot cache-buster.

use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    middleware as axum_middleware,
    response::{IntoResponse, Response},
    routing::{any, delete, get, post},
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::core::paths::AppPaths;
use crate::core::server_mode::{self, ServerMode};

use super::middleware::rate_limit;

use super::admin::{
    api_admin_publish_approve, api_admin_publish_list, api_admin_publish_reject,
    api_admin_reset_password, api_admin_skills_trash, api_admin_userlib_detail,
    api_admin_userlib_list, api_admin_users_delete, api_admin_users_list, api_admin_users_update,
};
use super::auth::{api_login, api_logout, api_logout_everywhere, api_me, api_register};
use super::community::{
    api_community_delete, api_community_download, api_community_install, api_community_list,
    api_community_skill_detail, api_community_upload,
};
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
    api_post_settings, api_test_provider, api_upsert_provider,
};
use super::private_upload::{api_publish_request, api_user_skills_list, api_user_skills_upload};
use super::recommend::{handle_feedback, handle_recommend};
use super::skills::{
    api_skill_detail, api_skill_file, api_skill_files, api_skills, handle_skill_bundle,
    handle_skill_file, handle_skill_get, handle_skill_use,
};
use super::state::AppState;
use super::telemetry::{api_event_by_id, api_events, api_summary, api_timeline};
use super::{APP_CSS, APP_JS, INDEX_HTML, SETUP_MD};

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
    serve_with(host, port, ServerMode::Owner, None, None).await
}

/// Boot the dashboard server with a fully-specified runtime identity.
/// `serve` keeps the legacy signature (owner mode, no TLS) for compat with
/// `ensure_running`'s detached spawn path; `serve_with` is the canonical
/// path the CLI dispatcher uses to pass through the operator's chosen
/// `--mode` / `--tls-cert` / `--tls-key`. See PLANNING.md §1.1 / §2.3.2.
pub async fn serve_with(
    host: &str,
    port: u16,
    mode: ServerMode,
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
) -> Result<()> {
    server_mode::validate_startup(mode, host, tls_cert.as_deref(), tls_key.as_deref())?;

    // PLANNING §1.1: stash the runtime mode into the process-global atomic
    // so `state::current_user` / `private_data_locked` / `auth::api_me` /
    // `serve_index` can short-circuit without threading `mode` through
    // every wrapper signature.
    super::state::set_server_mode(mode);

    // issue #24: honor RUNE_DATA_DIR / SKILL_MANAGER_DATA_DIR so the server's
    // data dir matches what `main.rs` and every CLI subcommand (incl. the
    // `recommend enrich` child the server spawns) resolve to. Using the
    // env-blind `default_path()` here split a `RUNE_DATA_DIR=B runai server`
    // between HOME/.runai (server) and B (enrich child).
    let paths = AppPaths::resolve();
    let state = Arc::new(AppState {
        db_path: paths.db_path(),
        mode,
        tls_cert: tls_cert.clone(),
        tls_key: tls_key.clone(),
    });

    // Real-time enrichment watcher (PLANNING real-time enrichment): watch the
    // public pool + every user's private pool so an edited SKILL.md or a new
    // skill auto-triggers `recommend enrich`. Pre-create both roots so the
    // RECURSIVE watch also covers users who register AFTER startup (new
    // `users/<uid>/skills/` lands inside the already-watched `users/`).
    // Failure to start is non-fatal — the dashboard is the primary surface.
    let data_dir = state
        .db_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let _ = std::fs::create_dir_all(data_dir.join("skills"));
    let _ = std::fs::create_dir_all(data_dir.join("users"));
    // RUNAI_DISABLE_SKILL_WATCHER=1 opts a host out of the real-time watcher
    // (default unchanged: watcher on). A box with a large static skill pool can
    // hit a startup enrich storm from the recursive watch's initial event
    // burst, so unattended servers can disable it without affecting normal use.
    let _skill_watcher =
        if std::env::var("RUNAI_DISABLE_SKILL_WATCHER").ok().as_deref() == Some("1") {
            tracing::info!("skill watcher disabled via RUNAI_DISABLE_SKILL_WATCHER=1");
            None
        } else {
            crate::core::skill_watcher::SkillWatcher::start(data_dir, |names| {
                super::market::spawn_enrich(names);
            })
            .map_err(|e| tracing::warn!("skill watcher failed to start: {e}"))
            .ok()
        };

    // Sub-routers for the rate-limited families. Building them as
    // independent sub-routers means the `from_fn` middleware applies
    // exactly to those paths and only those paths — no risk that the
    // limit accidentally covers an unrelated handler.
    let login_router = Router::new()
        .route("/auth/login", post(api_login))
        .layer(axum_middleware::from_fn(rate_limit::login_limit));
    let upload_router = Router::new()
        .route(
            "/api/community/upload",
            post(api_community_upload).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .layer(axum_middleware::from_fn(rate_limit::upload_limit));
    let skills_get_router = Router::new()
        .route("/skills/get/{name}", post(handle_skill_get))
        .route("/skills/use/{name}", post(handle_skill_use))
        .layer(axum_middleware::from_fn(rate_limit::skills_get_limit));
    // C2 (scan_findings.md): the user-facing private-pool upload endpoint gets
    // the same 10/hour/user throttle as /api/community/upload. Mounted as its
    // own sub-router so the limit confines to exactly this path (and picks up
    // its own PrivateUpload bucket namespace). Keeps the 64 MiB body cap.
    let private_upload_router = Router::new()
        .route(
            "/api/users/me/skills/upload",
            post(api_user_skills_upload).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .layer(axum_middleware::from_fn(rate_limit::private_upload_limit));

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/app.js", get(serve_app_js))
        .route("/app.css", get(serve_app_css))
        // PLANNING §1.7 Setup tab content. Plain markdown; the dashboard
        // JS fetches it on first nav to #/setup, renders inline, attaches
        // per-block copy buttons so users can grab CLI commands directly.
        .route("/setup.md", get(serve_setup_md))
        .route("/api/summary", get(api_summary))
        .route("/api/timeline", get(api_timeline))
        .route("/api/events", get(api_events))
        .route("/api/event/{id}", get(api_event_by_id))
        .route("/api/skills", get(api_skills))
        .route("/api/skill/{name}", get(api_skill_detail))
        .route("/api/skill/{name}/files", get(api_skill_files))
        .route("/api/skill/{name}/file", get(api_skill_file))
        // PLANNING §2.3 item 3: clients pin the server's leaf-cert
        // SHA-256 at install time so a later MITM that swaps the cert
        // gets rejected. The endpoint is unauthenticated by design —
        // anyone who can reach the server already sees the cert on the
        // TLS handshake, exposing the fingerprint over HTTP adds no info.
        .route("/api/tls/fingerprint", get(api_tls_fingerprint))
        // Remote-hook protocol: teammates' Claude Code UserPromptSubmit hooks
        // POST their standard hook JSON here and pipe stdout back into the
        // agent. See scripts/runai-client-install.sh for the wrapper they run.
        .route("/recommend", post(handle_recommend))
        // /skills/get/{name} is mounted via the `skills_get_router` sub-router
        // below so it picks up the 20/sec/IP rate limit (PLANNING §2.3 item 6).
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
        .route(
            "/api/settings",
            get(api_get_settings).post(api_post_settings),
        )
        .route("/api/providers", post(api_upsert_provider))
        .route("/api/providers/{id}", delete(api_delete_provider))
        .route("/api/providers/{id}/activate", post(api_activate_provider))
        .route("/api/providers/{id}/test", post(api_test_provider))
        // v15 admin: per-user management (list / promote / disable)
        .route("/api/admin/users", get(api_admin_users_list))
        .route(
            "/api/admin/users/{user_id}",
            post(api_admin_users_update).delete(api_admin_users_delete),
        )
        // Admin forced password reset for any user (writes argon2 hash +
        // rotates api_key). The 正规 replacement for hand-editing the users
        // table via SQL when someone forgets their password.
        .route(
            "/api/admin/users/{user_id}/reset-password",
            post(api_admin_reset_password),
        )
        // PLANNING §1.6 model B — admin userlib browse: list non-admin
        // users with private/imported counts, then drill into a single
        // user's split library.
        .route("/api/admin/userlib", get(api_admin_userlib_list))
        .route(
            "/api/admin/userlib/{user_id}",
            get(api_admin_userlib_detail),
        )
        // PLANNING §1.6 Model B C7b: admin batch-trash public-pool skills.
        // Returns 200 with per-name failed list rather than aborting on the
        // first unknown name so the dashboard can render outcomes per row.
        .route("/api/admin/skills/trash", post(api_admin_skills_trash))
        // PLANNING §1.4 C9d — admin reviews pending publish-requests,
        // either approves (copies to community pool) or rejects (with
        // a reason the user can see in list-mine).
        .route("/api/admin/publish-requests", get(api_admin_publish_list))
        .route(
            "/api/admin/publish-requests/{resource_id}/approve",
            post(api_admin_publish_approve),
        )
        .route(
            "/api/admin/publish-requests/{resource_id}/reject",
            post(api_admin_publish_reject),
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
        // /auth/login is mounted via the `login_router` sub-router below so it
        // picks up the 5/min/IP rate limit (PLANNING §2.3 item 6).
        .route("/auth/logout", post(api_logout))
        .route("/api/me/logout-everywhere", post(api_logout_everywhere))
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
        // ---- v16 community market (team mode only — owner mode 404s) ----
        // /api/community/upload is mounted via the `upload_router` sub-router
        // below — it picks up both the 64 MiB body limit AND the 10/hour/user
        // rate limit (PLANNING §2.3 item 6).
        .route("/api/community/list", get(api_community_list))
        .route(
            "/api/community/skill/{uid}/{name}",
            get(api_community_skill_detail).delete(api_community_delete),
        )
        .route(
            "/api/community/download/{uid}/{name}",
            get(api_community_download),
        )
        .route(
            "/api/community/install/{uid}/{name}",
            post(api_community_install),
        )
        // PLANNING §1.4 rewrite — private skill upload entry point.
        // Multipart name+bundle, lands at <data>/users/<uid>/skills/<name>/
        // with publish_status='draft'. team mode only; owner mode 403.
        // Mounted via the `private_upload_router` sub-router below so it picks
        // up both the 64 MiB body limit AND the 10/hour/user rate limit
        // (C2 — scan_findings.md).
        // PLANNING §1.4 C9e — caller lists their own private skills with
        // publish + enrich state so the CLI / dashboard can render
        // status badges and gate the publish-request button.
        .route("/api/users/me/skills", get(api_user_skills_list))
        // PLANNING §1.4 C9c — user requests their draft skill be reviewed
        // for publication to the community pool. Pre-condition: enrich
        // must have produced a non-empty resource_ai_summary row.
        .route(
            "/api/users/me/skills/{name}/publish-request",
            post(api_publish_request),
        )
        // PLANNING §2.3 item 4 — explicitly reject the canonical
        // self-describing-API paths an automated agent would probe to map
        // the surface. Method-agnostic (`any(...)`) so a HEAD/POST/OPTIONS
        // probe gets the same 404 + empty body as a GET. The fallback
        // handler covers everything else, so this list is "name the well
        // known probes explicitly" — defense in depth, not load-bearing.
        .route("/openapi.json", any(empty_404))
        .route("/openapi.yaml", any(empty_404))
        .route("/swagger", any(empty_404))
        .route("/swagger.json", any(empty_404))
        .route("/swagger-ui", any(empty_404))
        .route("/swagger-ui/", any(empty_404))
        .route("/swagger-ui/{*rest}", any(empty_404))
        .route("/docs", any(empty_404))
        .route("/docs/", any(empty_404))
        .route("/docs/{*rest}", any(empty_404))
        .route("/api-docs", any(empty_404))
        .route("/api-docs/", any(empty_404))
        .route("/api-docs/{*rest}", any(empty_404))
        .route("/__schema", any(empty_404))
        .route("/graphql", any(empty_404))
        .route("/redoc", any(empty_404))
        // Merge the rate-limited sub-routers and attach the shared state.
        .merge(login_router)
        .merge(upload_router)
        .merge(private_upload_router)
        .merge(skills_get_router)
        // PLANNING §2.3 item 4 — every un-matched path returns a uniform
        // empty-body 404. No X-Powered-By, no JSON `{"error":"not found"}`,
        // no hint that there could be a similarly-named route somewhere.
        .fallback(empty_404)
        .with_state(state);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .with_context(|| format!("parse {host}:{port}"))?;
    let use_tls = tls_cert.is_some() && tls_key.is_some();
    let scheme = if use_tls { "https" } else { "http" };
    println!("runai dashboard at {scheme}://{addr} (mode={mode})");
    if use_tls {
        // PLANNING §2.3 item 2 — real TLS via axum-server's rustls feature.
        // `validate_startup` already gated team + non-loopback + no TLS,
        // but owner mode (and team-loopback) can still opt in via the
        // explicit --tls-cert / --tls-key flags. The check here is "both
        // flags were passed" — not "we are required to use TLS".
        let cert = tls_cert.as_deref().expect("checked use_tls");
        let key = tls_key.as_deref().expect("checked use_tls");
        let cfg = super::tls::load_rustls_config(cert, key).await?;
        axum_server::bind_rustls(addr, cfg)
            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .context("axum_server::bind_rustls serve")?;
    } else {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("bind {addr}"))?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .context("axum::serve")?;
    }
    Ok(())
}

/// PLANNING §2.3 item 4: uniform 404 for any unmatched path AND for the
/// explicitly-rejected anti-probe routes. Empty body — no diagnostic JSON,
/// no `Content-Type` hint at all. The Status line itself plus an empty
/// body is the only thing the wire sees, so an attacker cannot diff
/// "unknown path" against "auth required" against "internal error" via
/// response shape.
async fn empty_404() -> Response {
    (StatusCode::NOT_FOUND, "").into_response()
}

/// GET /api/tls/fingerprint — return the SHA-256 of the server's leaf
/// certificate so the install script can pin it into `~/.runai-server.json`.
///
/// Output shape:
///   - TLS configured: `200 + {"fingerprint":"<64 hex chars>"}`
///   - No TLS:         `404 + ""` (we don't have one to give)
///   - PEM unreadable: `500` (operator misconfiguration)
///
/// We deliberately do NOT gate this on auth: an attacker on the network
/// already sees the cert on the TLS handshake. The only people who can't
/// see the cert without this endpoint are clients on an HTTP-only deploy,
/// and those clients hit the 404 path here anyway — no pinning is possible
/// without TLS.
async fn api_tls_fingerprint(State(state): State<Arc<AppState>>) -> Response {
    let Some(cert_path) = state.tls_cert.as_deref() else {
        return (StatusCode::NOT_FOUND, "").into_response();
    };
    let cert_path = cert_path.to_path_buf();
    let fp =
        match tokio::task::spawn_blocking(move || super::tls::leaf_fingerprint_sha256(&cert_path))
            .await
        {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                eprintln!("/api/tls/fingerprint: leaf_fingerprint_sha256 failed: {e:#}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
            }
            Err(e) => {
                eprintln!("/api/tls/fingerprint: spawn_blocking join failed: {e}");
                return (StatusCode::INTERNAL_SERVER_ERROR, "").into_response();
            }
        };
    (StatusCode::OK, Json(serde_json::json!({"fingerprint": fp}))).into_response()
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
    let mut patched = INDEX_HTML
        .replace("\"/app.css\"", &format!("\"/app.css?v={bid}\""))
        .replace("\"/app.js\"", &format!("\"/app.js?v={bid}\""));
    // PLANNING §1.1: owner mode dashboard cut. Inject a `mode-owner` body
    // class so the bundled CSS can hide the team-only chrome (account
    // pill, login modal, scope segments, userlib tab, community tab, the
    // admin user-management pane). Team mode body class is left untouched
    // so existing rendering is byte-identical (regression-safe).
    if super::state::server_mode() == ServerMode::Owner {
        patched = patched.replacen(
            "<body class=\"theme-github\"",
            "<body class=\"theme-github mode-owner\"",
            1,
        );
    }
    dynamic_response(patched, "text/html; charset=utf-8")
}
async fn serve_app_js() -> Response {
    static_response(APP_JS, "application/javascript; charset=utf-8")
}
async fn serve_app_css() -> Response {
    static_response(APP_CSS, "text/css; charset=utf-8")
}
/// GET /setup.md — Setup tab content, rendered per request because:
///   1. `{SERVER_URL}` placeholder is substituted with the actual request
///      origin so users can copy-paste the commands verbatim.
///   2. ADMIN-only / USER-only marker sections are stripped based on who
///      is asking — owner mode synthetic owner + team-mode admin see the
///      admin variant (启动 server + 公共池 + 垃圾桶 + 用户管理); team-
///      mode regular user + anonymous see the user variant (装客户端 +
///      配 hook + 装到「我的库」+ 上传社区).
async fn serve_setup_md(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let body = tokio::task::spawn_blocking(move || -> String {
        let is_admin = state
            .db()
            .ok()
            .and_then(|db| super::state::current_user(&headers, &db).ok().flatten())
            .map(|u| u.is_admin)
            .unwrap_or(false);
        // Use the request origin verbatim (NOT guess_server_url) — see
        // recommend::request_origin docs. The Setup reader is the curl
        // client themself, so the URL that got them here is exactly the
        // URL their copy-pasted commands must hit.
        let server_url = super::recommend::request_origin(&headers);
        let mut md = SETUP_MD.replace("{SERVER_URL}", &server_url);
        let drop = if is_admin { "user-only" } else { "admin-only" };
        md = strip_setup_marker_section(&md, drop);
        md
    })
    .await
    .unwrap_or_else(|_| String::new());
    dynamic_response(body, "text/markdown; charset=utf-8")
}

/// Strip the `<!-- runai:<drop_audience>-start --> ... <!-- end -->` block
/// PLUS the matching keep-side audience marker comment lines (user-only
/// or admin-only) so the rendered markdown shows neither raw side's
/// comments. Other marker families (notably `<!-- runai:os-* -->`) are
/// LEFT in place because the client (18-setup-tab.js) does its own OS-
/// aware strip based on `navigator.userAgent` — the server doesn't see
/// that signal. The matcher is line-anchored (trimmed); a marker buried
/// in a paragraph wouldn't be a real marker anyway.
fn strip_setup_marker_section(md: &str, drop_audience: &str) -> String {
    let drop_begin = format!("<!-- runai:{drop_audience}-start -->");
    let drop_end = format!("<!-- runai:{drop_audience}-end -->");
    let keep_audience = match drop_audience {
        "user-only" => "admin-only",
        "admin-only" => "user-only",
        _ => "",
    };
    let keep_begin = format!("<!-- runai:{keep_audience}-start -->");
    let keep_end = format!("<!-- runai:{keep_audience}-end -->");
    let mut out = String::with_capacity(md.len());
    let mut in_drop = false;
    for line in md.split('\n') {
        let t = line.trim();
        if t == drop_begin {
            in_drop = true;
            continue;
        }
        if t == drop_end {
            in_drop = false;
            continue;
        }
        if in_drop {
            continue;
        }
        if !keep_audience.is_empty() && (t == keep_begin || t == keep_end) {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
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

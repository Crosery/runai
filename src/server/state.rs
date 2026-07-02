//! Shared request-scoped state + the private auth / owner / skill-dir
//! resolution helpers used across every route family.
//!
//! Auth resolution order: Authorization: Bearer first (client hook),
//! then runai_session cookie (browser dashboard). Both end up at the
//! same User row via api_key_hash lookup, so password is only used at
//! /auth/login to mint the cookie.

use anyhow::Result;
use axum::http::{HeaderMap, header};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::core::auth as authmod;
use crate::core::db::{Database, User};
use crate::core::server_mode::ServerMode;

use super::error::ApiError;

/// Process-global runtime server mode. Set once at `serve_with` boot, read
/// by `current_user` / `private_data_locked` / `app.rs::serve_index` /
/// `auth.rs::api_me`. The atomic-vs-`AppState` split keeps the 41
/// `require_user` / `require_admin` / `current_owner_id` call sites
/// unchanged — those auth helpers can short-circuit on mode without
/// requiring every downstream signature to thread `mode: ServerMode`
/// through. PLANNING §1.1 dashboard-cut path.
///
/// Encoding: `0` = `Owner` (default — matches `ServerMode::default()`
/// so an unset atomic still behaves correctly), `1` = `Team`.
static SERVER_MODE_VAL: AtomicU8 = AtomicU8::new(0);

pub(super) fn set_server_mode(mode: ServerMode) {
    let v = match mode {
        ServerMode::Owner => 0,
        ServerMode::Team => 1,
    };
    SERVER_MODE_VAL.store(v, Ordering::SeqCst);
}

pub(super) fn server_mode() -> ServerMode {
    match SERVER_MODE_VAL.load(Ordering::SeqCst) {
        0 => ServerMode::Owner,
        _ => ServerMode::Team,
    }
}

/// Synthetic implicit-admin identity used when owner mode skips real auth.
/// Never persisted to the DB — `user_id = "owner"` is a reserved sentinel
/// that downstream helpers (`require_user` / `require_admin` /
/// `current_owner_id` / `resolve_view_user`) treat as a logged-in admin
/// without ever calling into `users` table lookup.
///
/// PLANNING §1.1 owner-mode dashboard cut: this lets the single-user
/// self-serve deployment skip the login modal + render the implicit-admin
/// view straight away, while the 41 auth call sites stay untouched.
fn synthetic_owner() -> User {
    User {
        user_id: "owner".to_string(),
        username: "owner".to_string(),
        password_hash: String::new(),
        api_key_hash: String::new(),
        is_admin: true,
        disabled: false,
        prefs_json: String::new(),
        created_at: 0,
    }
}

/// Shared state for handlers. Holds only the DB path (and AppPaths if needed
/// later for other resources) — rusqlite `Connection` is `!Sync`, so each
/// handler opens its own connection per request. SQLite open is cheap
/// (microseconds for the same file in the OS page cache); this keeps the
/// server lock-free and avoids serialising readers on a Mutex.
///
/// `mode` is the runtime owner-vs-team flag from `runai server --mode`
/// (default `owner`). Every handler that branches on identity model reads
/// it from here rather than re-deriving from DB state. `tls_cert` /
/// `tls_key` are the resolved `--tls-cert` / `--tls-key` paths from the
/// operator — `app.rs::serve_with` swaps `axum::serve` for
/// `axum_server::bind_rustls` when both are present, and
/// `app.rs::api_tls_fingerprint` reads `tls_cert` so clients can pin the
/// leaf-cert SHA-256.
pub(super) struct AppState {
    pub(super) db_path: PathBuf,
    pub(super) mode: ServerMode,
    pub(super) tls_cert: Option<PathBuf>,
    #[allow(dead_code)] // read by app.rs::serve_with at bind time only
    pub(super) tls_key: Option<PathBuf>,
}

impl AppState {
    pub(super) fn db(&self) -> Result<Database> {
        Database::open(&self.db_path)
    }
}

/// Resolve the current user from request headers. Returns None when no
/// credential is presented, or when the credential doesn't match any
/// non-disabled user. Used by every authenticated route.
///
/// Owner-mode short circuit (PLANNING §1.1): when the server boots in
/// `owner` mode, this returns a `Some(synthetic_owner())` regardless of
/// whether any credential is supplied. The local user is the implicit
/// admin of their own single-user deployment — there is no login flow,
/// no second user, no notion of "anonymous" access at this layer.
pub(super) fn current_user(headers: &HeaderMap, db: &Database) -> Result<Option<User>> {
    if server_mode() == ServerMode::Owner {
        return Ok(Some(synthetic_owner()));
    }
    // 1. Bearer header (hook path)
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Some(token) = authmod::parse_bearer_header(auth) {
        let h = authmod::key_hash(&token);
        if let Some(u) = db.find_user_by_api_key_hash(&h)? {
            if u.disabled {
                return Ok(None);
            }
            return Ok(Some(u));
        }
    }

    // 2. Cookie session (dashboard path) — cookie value is the raw api_key
    let cookie_hdr = headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
    if let Some(tok) = authmod::parse_session_cookie(cookie_hdr) {
        let token = authmod::BearerToken(tok);
        let h = authmod::key_hash(&token);
        if let Some(u) = db.find_user_by_api_key_hash(&h)? {
            if u.disabled {
                return Ok(None);
            }
            return Ok(Some(u));
        }
    }
    Ok(None)
}

pub(super) fn require_user(headers: &HeaderMap, db: &Database) -> Result<User, ApiError> {
    match current_user(headers, db).map_err(ApiError::Internal)? {
        Some(u) => Ok(u),
        None => Err(ApiError::Unauthorized),
    }
}

/// Owner scope to use when looking up / writing skill resources for the
/// current request. `Ok(None)` means anonymous / compat (public pool);
/// `Ok(Some(uid))` means private to the authenticated user. Never errors
/// — auth failure degrades to anonymous on purpose so the legacy unauth
/// client paths keep working.
pub(super) fn current_owner_id(headers: &HeaderMap, db: &Database) -> Option<String> {
    current_user(headers, db).ok().flatten().map(|u| u.user_id)
}

/// Resolve `(skill_dir, owner_user_id)` for `/skills/get` and `/skills/file`.
///
/// Lookup order:
/// 1. If the caller is authenticated and has a private skill named `name`,
///    return its directory + owner_user_id.
/// 2. Otherwise fall back to a public skill with this name (DB row or, for
///    the compat window, the on-disk directory under `paths.skills_dir()`).
///
/// Errors only when neither path resolves. Callers should map that to 404.
pub(super) fn resolve_skill_dir(
    headers: &HeaderMap,
    db: &Database,
    paths: &crate::core::paths::AppPaths,
    name: &str,
) -> Result<(std::path::PathBuf, Option<String>)> {
    resolve_skill_dir_scoped(headers, db, paths, name, None)
}

/// Like [`resolve_skill_dir`], but lets an ADMIN viewer pin resolution to a
/// specific user's pool via `requested_owner` (the dashboard "用户库"
/// drill-in passes `?owner=<uid>`). The override is admin-gated: a non-admin's
/// `requested_owner` is IGNORED so they cannot read another user's private
/// skill — they keep resolving within their own scope. `resolve_skill_dir`
/// is the `requested_owner = None` shorthand, so behavior is byte-identical
/// for every existing caller.
pub(super) fn resolve_skill_dir_scoped(
    headers: &HeaderMap,
    db: &Database,
    paths: &crate::core::paths::AppPaths,
    name: &str,
    requested_owner: Option<&str>,
) -> Result<(std::path::PathBuf, Option<String>)> {
    // C1 SECURITY: the route `name` is user-controlled and is joined onto
    // `skills_dir()` in the disk-fallback below. Reject any traversal-flavored
    // name up front (`..`, embedded separators, control chars) so a crafted
    // `%2e%2e` can never make the resolved dir climb out of the public pool
    // (which would let anon `/skills/{get,file,bundle}` tar/read the whole
    // data dir incl. runai.db). Bailing here — before the DB lookup even —
    // also keeps a bogus name from matching anything by accident.
    if !crate::core::paths::is_safe_skill_name(name) {
        anyhow::bail!("unsafe skill name: {name}");
    }
    // admin + non-empty ?owner= → pin to that user's pool. The full-user
    // lookup (for `is_admin`) only happens when an override is actually
    // requested; the common path stays on `current_owner_id`.
    let admin_override: Option<String> = match requested_owner {
        Some(uid) if !uid.is_empty() => current_user(headers, db)
            .ok()
            .flatten()
            .map(|u| u.is_admin)
            .unwrap_or(false)
            .then(|| uid.to_string()),
        _ => None,
    };
    // Non-admin's `requested_owner` collapses to None above → falls back to
    // the viewer's own scope (anonymous → None → public pool).
    let scope: Option<String> = admin_override.or_else(|| current_owner_id(headers, db));
    if let Some(row) = db.find_resource_by_name_for_user(
        crate::core::resource::ResourceKind::Skill,
        name,
        scope.as_deref(),
    )? {
        return Ok((row.directory.clone(), row.owner_user_id.clone()));
    }
    // Compat: a public skill might exist on disk without a DB row (e.g.
    // a freshly-cloned tree that hasn't been `runai scan`-ed yet).
    //
    // A1 SECURITY: do NOT let a stray public-pool dir shadow a PRIVATE skill
    // row. If any row owns this name and it's private, the DB lookup above
    // missed it only because the caller's scope doesn't include that owner —
    // serving the bare on-disk dir here would hand a private skill to anyone
    // (the `agent-browser` anon leak). Refuse the disk fallback in that case.
    let public_dir = paths.skills_dir().join(name);
    if public_dir.exists() {
        let private_shadow = db
            .find_resource_by_name_for_user(
                crate::core::resource::ResourceKind::Skill,
                name,
                Some("*"),
            )?
            .map(|r| r.owner_user_id.is_some())
            .unwrap_or(false);
        if !private_shadow {
            return Ok((public_dir, None));
        }
    }
    anyhow::bail!("skill not found: {name}")
}

/// Returns the current user only if they are admin. Used to gate
/// provider/settings/users-management endpoints.
pub(super) fn require_admin(headers: &HeaderMap, db: &Database) -> Result<User, ApiError> {
    let u = require_user(headers, db)?;
    if !u.is_admin {
        return Err(ApiError::Forbidden);
    }
    Ok(u)
}

/// True when this server already holds user-identifiable telemetry
/// (any registered user OR any router_event row). The "first-run"
/// compat carve-out that lets `users` table be empty must NOT let an
/// already-populated router_events table leak to anonymous callers —
/// that's how PLANNING §2.3 item 5 "log in is uniform" gets bypassed
/// when an owner-mode server has been running and accumulating events
/// without ever registering a user.
pub(super) fn private_data_locked(db: &Database) -> bool {
    // Owner-mode short circuit (PLANNING §1.1): the local user is the
    // implicit admin of their own deployment, so the carve-out is
    // permanent — they read all telemetry without ever needing to log in.
    if server_mode() == ServerMode::Owner {
        return false;
    }
    if db
        .list_users()
        .map_err(|_| ())
        .map(|u| !u.is_empty())
        .unwrap_or(true)
    {
        return true;
    }
    if db
        .router_events_count_filtered(None, None, false, None)
        .map_err(|_| ())
        .map(|n| n > 0)
        .unwrap_or(true)
    {
        return true;
    }
    false
}

/// Resolve "which user_id should we filter activity by". Returns
/// `Some(uid)` to scope SQL to that user, `None` for global view.
///
/// Industry-standard tenant isolation (Linear / Stripe / GitHub dashboards
/// all follow the same pattern):
///   - **No auth → 401** when this server already holds any user or any
///     router_event (compat closed; first-run carve-out removed for hot
///     servers — see `private_data_locked`). Activity / events / summary
///     are private telemetry; anonymous reads are rejected.
///   - **Non-admin → forced own scope.** Even if `?user=` is passed,
///     anything other than their own uid is 403. Default scope is their
///     own uid, no opt-out.
///   - **Admin → global by default, can target a specific user.** Default
///     (no `?user=` param) returns `None` so admin's Activity tab shows
///     the global view straight away. `?user=<uid>` scopes to that user
///     for cross-user inspection. `?user=me` is a convenience alias for
///     "filter to my own events".
///
/// First-run bootstrap: the very first /users/register call auto-promotes
/// the first registered account to admin, so the dashboard is usable
/// immediately after a fresh deployment.
pub(super) fn resolve_view_user(
    headers: &HeaderMap,
    db: &Database,
    requested: Option<&str>,
) -> Result<Option<String>, ApiError> {
    // Compat carve-out: only when the server is *cold* (no users AND no
    // telemetry rows). A previously owner-mode server with accumulated
    // history must require auth — that's the load-bearing gate.
    if !private_data_locked(db) {
        return Ok(None);
    }
    let me = require_user(headers, db)?;
    let req = requested.map(str::trim).filter(|s| !s.is_empty());
    if me.is_admin {
        match req {
            None | Some("all") => Ok(None),
            Some("me") => Ok(Some(me.user_id)),
            Some(other) => Ok(Some(other.to_string())),
        }
    } else {
        match req {
            None => Ok(Some(me.user_id)),
            Some("me") => Ok(Some(me.user_id)),
            Some(uid) if uid == me.user_id => Ok(Some(me.user_id)),
            _ => Err(ApiError::Forbidden),
        }
    }
}

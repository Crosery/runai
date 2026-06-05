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

use crate::core::auth as authmod;
use crate::core::db::{Database, User};
use crate::core::server_mode::ServerMode;

use super::error::ApiError;

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
pub(super) fn current_user(headers: &HeaderMap, db: &Database) -> Result<Option<User>> {
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
    let owner = current_owner_id(headers, db);
    if let Some(row) = db.find_resource_by_name_for_user(
        crate::core::resource::ResourceKind::Skill,
        name,
        owner.as_deref(),
    )? {
        return Ok((row.directory.clone(), row.owner_user_id.clone()));
    }
    // Compat: a public skill might exist on disk without a DB row (e.g.
    // a freshly-cloned tree that hasn't been `runai scan`-ed yet).
    let public_dir = paths.skills_dir().join(name);
    if public_dir.exists() {
        return Ok((public_dir, None));
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

/// Resolve "which user_id should we filter activity by". Returns
/// `Some(uid)` to scope SQL to that user, `None` for global view.
///
/// Industry-standard tenant isolation (Linear / Stripe / GitHub dashboards
/// all follow the same pattern):
///   - **No auth → 401.** Activity / events / summary are private telemetry;
///     anonymous reads are rejected.
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
    // Compat carve-out: when the users table is literally empty (no one
    // has registered yet), allow anonymous read so the very first visit
    // to the dashboard isn't a 401 wall. The moment any user is created,
    // auth becomes mandatory.
    if db.list_users().map_err(ApiError::Internal)?.is_empty() {
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

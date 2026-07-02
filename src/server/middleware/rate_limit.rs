//! Per-route fixed-window rate limiting (PLANNING.md §2.3 item 6).
//!
//! Four categories, all tuned around "an automated agent walks one endpoint
//! to abuse it":
//!
//! | endpoint                        | limit        | key      |
//! |---------------------------------|--------------|----------|
//! | `POST /auth/login`              | 5 / minute   | client IP|
//! | `POST /api/community/upload`    | 10 / hour    | user_id  |
//! | `POST /api/users/me/skills/upload` | 10 / hour | user_id  |
//! | `*  /skills/get/*`              | 20 / second  | client IP|
//!
//! When the bucket is full, the request is short-circuited at the middleware
//! layer with `429 + empty body` — no `Retry-After`, no JSON `{"error": ...}`,
//! no body at all. The threat model wants minimal info to leak: every blocked
//! request returns the same response so an attacker cannot tell "I'm limited
//! at 5/min" from "I'm limited at 6/min" from "I'm limited at all".
//!
//! Algorithm: fixed-window counter keyed by `(route_class, principal,
//! window_index)`. `window_index` is measured from one process-global
//! monotonic `Instant`; do not compute it from a request-local `Instant`, or
//! every request lands in window 0 and the limiter never recovers until restart.
//! Cheap, no per-request floating-point math, and the small `dashmap` works
//! fine for the volumes runai handles (single-server dashboard, not a public
//! CDN).
//!
//! Memory: every minute a sweep prunes entries whose window has expired by
//! more than one period, so the map stays bounded. The sweep runs lazily on
//! the next limiter call rather than via a background task — simpler and one
//! fewer Tokio handle to manage.

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use std::net::SocketAddr;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::core::auth as authmod;

/// Category of route this limiter applies to. The window length and the
/// max-events-per-window depend on which family the request lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RouteClass {
    /// `POST /auth/login` — 5 / min / IP. Defends against credential stuffing
    /// + account-existence probes.
    AuthLogin,
    /// `POST /api/community/upload` — 10 / hour / user_id. Defends against
    /// disk-fill via repeated bundle uploads. Falls back to client IP when
    /// the request is unauthenticated (which the upload handler would 401
    /// anyway, but the limiter runs first so we have to key on something).
    CommunityUpload,
    /// `POST /api/users/me/skills/upload` — 10 / hour / user_id. The
    /// user-facing private-pool upload endpoint (issue #29 made
    /// `/api/community/upload` admin-only, so THIS is the path any non-admin
    /// can flood). Each call extracts a tar.gz AND forks a `runai recommend
    /// enrich` LLM child, so an unthrottled loop is a disk-fill + LLM-spend
    /// DoS. Same 10/hour/user quota as `CommunityUpload` but a separate
    /// bucket namespace (distinct `tag`), so the two upload surfaces don't
    /// share a counter. (C2 — scan_findings.md.)
    PrivateUpload,
    /// `* /skills/get/*` — 20 / second / IP. Defends against scraping the
    /// whole skill catalog one HTTP call at a time.
    SkillsGet,
}

impl RouteClass {
    fn window(self) -> Duration {
        match self {
            RouteClass::AuthLogin => Duration::from_secs(60),
            RouteClass::CommunityUpload | RouteClass::PrivateUpload => Duration::from_secs(60 * 60),
            RouteClass::SkillsGet => Duration::from_secs(1),
        }
    }

    fn quota(self) -> u32 {
        match self {
            RouteClass::AuthLogin => 5,
            RouteClass::CommunityUpload | RouteClass::PrivateUpload => 10,
            RouteClass::SkillsGet => 20,
        }
    }

    /// Short tag used as the first component of the bucket key. Keeps the
    /// `(class, principal)` namespaces from colliding when the same IP hits
    /// multiple limited routes.
    fn tag(self) -> &'static str {
        match self {
            RouteClass::AuthLogin => "login",
            RouteClass::CommunityUpload => "upload",
            RouteClass::PrivateUpload => "priv-upload",
            RouteClass::SkillsGet => "get",
        }
    }
}

/// `(route_tag, principal, window_index)` → request count in that window.
/// `window_index` = `epoch_seconds / window_seconds`, so an entry
/// auto-expires by being replaced with a different key the next window.
type BucketMap = DashMap<String, BucketEntry>;

struct BucketEntry {
    count: u32,
    /// `Instant` when this window started — used by the lazy sweep to drop
    /// entries whose window has long since expired.
    started: Instant,
}

fn buckets() -> &'static BucketMap {
    static BUCKETS: OnceLock<BucketMap> = OnceLock::new();
    BUCKETS.get_or_init(DashMap::new)
}

fn limiter_started_at() -> Instant {
    static STARTED_AT: OnceLock<Instant> = OnceLock::new();
    *STARTED_AT.get_or_init(Instant::now)
}

/// Returns true when the bucket has room. Also bumps the counter when the
/// answer is true. False = caller should respond 429.
fn check_and_record(class: RouteClass, principal: &str) -> bool {
    let window = class.window();
    let now = Instant::now();
    // Window key is `(tag, principal, epoch_window_index)`. The index is
    // computed off Instant rather than SystemTime because we don't care
    // about wall-clock alignment, only about uniformly-sized windows.
    let nanos = now.duration_since(limiter_started_at()).as_nanos();
    let window_idx = nanos / window.as_nanos();
    let key = format!("{}:{}:{}", class.tag(), principal, window_idx);

    let mut entry = buckets().entry(key).or_insert(BucketEntry {
        count: 0,
        started: now,
    });
    if entry.count >= class.quota() {
        return false;
    }
    entry.count += 1;
    true
}

/// Best-effort cleanup. Called from the public middleware entry every
/// `SWEEP_EVERY_N` requests so we don't pay the lock cost on every hop.
/// Single-pass walk: any entry older than 2× its window is removed.
fn maybe_sweep(class: RouteClass) {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    const SWEEP_EVERY_N: u64 = 1024;
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if !n.is_multiple_of(SWEEP_EVERY_N) {
        return;
    }
    let now = Instant::now();
    let max_age = class.window().saturating_mul(2);
    buckets().retain(|_, v| now.duration_since(v.started) <= max_age);
}

/// Client-IP extractor. Trusts `X-Forwarded-For` only when explicit-proxy
/// support is opt-in (it isn't yet) — for now we use the socket peer
/// address axum's `ConnectInfo` extractor surfaces. Falls back to a
/// fixed sentinel so we still rate-limit even when the request arrived
/// over a transport that doesn't expose a peer address.
fn principal_ip<B>(req: &Request<B>) -> String {
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return addr.ip().to_string();
    }
    "unknown".to_string()
}

/// User-id extractor for the per-user limits. Same lookup as
/// `state::current_user` but stripped to just the api_key path (cookies
/// + bearer) and without a DB round-trip — we hash the credential and
///   use the hash as the principal. Two consequences:
/// - we don't need a DB connection inside the middleware (cheaper)
/// - rotating the api_key resets the bucket (acceptable: that's a deliberate
///   "I want a new identity" signal)
///
/// Falls back to the IP when no credential is presented.
fn principal_user<B>(req: &Request<B>) -> String {
    if let Some(token) = bearer_or_cookie(req.headers()) {
        return format!("u:{}", authmod::key_hash(&token));
    }
    principal_ip(req)
}

fn bearer_or_cookie(headers: &HeaderMap) -> Option<authmod::BearerToken> {
    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(tok) = authmod::parse_bearer_header(Some(auth))
    {
        return Some(tok);
    }
    if let Some(cookie) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok())
        && let Some(raw) = authmod::parse_session_cookie(Some(cookie))
    {
        return Some(authmod::BearerToken(raw));
    }
    None
}

/// Synthetic 429 response. Empty body, no `Retry-After`. We could put a
/// `Content-Length: 0` header here but axum / hyper already does that.
fn over_limit_response() -> Response {
    (StatusCode::TOO_MANY_REQUESTS, Body::empty()).into_response()
}

/// Build the middleware function for a specific route class. axum middleware
/// is `Fn(Request, Next) -> Future<Response>` so we close over `class`.
async fn rate_limit_layer(
    class: RouteClass,
    req: Request<Body>,
    next: Next,
) -> Result<Response, Response> {
    let principal = match class {
        RouteClass::AuthLogin | RouteClass::SkillsGet => principal_ip(&req),
        RouteClass::CommunityUpload | RouteClass::PrivateUpload => principal_user(&req),
    };
    maybe_sweep(class);
    if !check_and_record(class, &principal) {
        return Err(over_limit_response());
    }
    Ok(next.run(req).await)
}

// ── public middleware-function wrappers ─────────────────────────────────────
//
// axum's `middleware::from_fn` wants an async fn, not a closure, so we
// expose one wrapper per class.

pub(in crate::server) async fn login_limit(req: Request<Body>, next: Next) -> Response {
    match rate_limit_layer(RouteClass::AuthLogin, req, next).await {
        Ok(r) => r,
        Err(r) => r,
    }
}

pub(in crate::server) async fn upload_limit(req: Request<Body>, next: Next) -> Response {
    match rate_limit_layer(RouteClass::CommunityUpload, req, next).await {
        Ok(r) => r,
        Err(r) => r,
    }
}

pub(in crate::server) async fn private_upload_limit(req: Request<Body>, next: Next) -> Response {
    match rate_limit_layer(RouteClass::PrivateUpload, req, next).await {
        Ok(r) => r,
        Err(r) => r,
    }
}

pub(in crate::server) async fn skills_get_limit(req: Request<Body>, next: Next) -> Response {
    match rate_limit_layer(RouteClass::SkillsGet, req, next).await {
        Ok(r) => r,
        Err(r) => r,
    }
}

/// Test-only escape hatch: wipe every bucket so independent tests don't
/// leak counter state into each other. Production builds never see this.
#[cfg(test)]
pub(crate) fn _reset_all_for_tests() {
    buckets().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_guard() -> MutexGuard<'static, ()> {
        let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        _reset_all_for_tests();
        guard
    }

    #[test]
    fn quota_enforced_per_class_and_principal() {
        let _guard = test_guard();
        // login = 5/min/IP
        for _ in 0..5 {
            assert!(check_and_record(RouteClass::AuthLogin, "1.2.3.4"));
        }
        assert!(
            !check_and_record(RouteClass::AuthLogin, "1.2.3.4"),
            "6th hit must be over the 5/min quota"
        );
        // Different principal → independent bucket.
        assert!(check_and_record(RouteClass::AuthLogin, "5.6.7.8"));
        // Different class with same principal → independent bucket.
        assert!(check_and_record(RouteClass::CommunityUpload, "1.2.3.4"));
    }

    #[test]
    fn upload_quota_is_ten() {
        let _guard = test_guard();
        for _ in 0..10 {
            assert!(check_and_record(RouteClass::CommunityUpload, "u:abc"));
        }
        assert!(
            !check_and_record(RouteClass::CommunityUpload, "u:abc"),
            "11th hit must be over the 10/hour quota"
        );
    }

    #[test]
    fn private_upload_quota_is_ten() {
        let _guard = test_guard();
        for _ in 0..10 {
            assert!(check_and_record(RouteClass::PrivateUpload, "u:priv"));
        }
        assert!(
            !check_and_record(RouteClass::PrivateUpload, "u:priv"),
            "11th hit must be over the 10/hour quota"
        );
        // Separate bucket namespace from CommunityUpload — same principal, the
        // two upload surfaces must not share a counter.
        assert!(check_and_record(RouteClass::CommunityUpload, "u:priv"));
    }

    #[test]
    fn skills_get_quota_is_twenty() {
        let _guard = test_guard();
        for _ in 0..20 {
            assert!(check_and_record(RouteClass::SkillsGet, "9.9.9.9"));
        }
        assert!(
            !check_and_record(RouteClass::SkillsGet, "9.9.9.9"),
            "21st hit must be over the 20/sec quota"
        );
    }

    #[test]
    fn skills_get_bucket_rolls_over_after_window() {
        let _guard = test_guard();
        for _ in 0..20 {
            assert!(check_and_record(RouteClass::SkillsGet, "9.9.9.9"));
        }
        assert!(
            !check_and_record(RouteClass::SkillsGet, "9.9.9.9"),
            "precondition: same-window request is over quota"
        );

        std::thread::sleep(RouteClass::SkillsGet.window() + Duration::from_millis(25));

        assert!(
            check_and_record(RouteClass::SkillsGet, "9.9.9.9"),
            "a new fixed window must accept the next request"
        );
    }
}

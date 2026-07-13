//! Process-level TTL cache for the router-telemetry aggregates that back the
//! skill-feedback-radar (`skills.rs::api_skill_detail`).
//!
//! `Database::skill_router_stats` scans every `router_events` row inside a
//! 90-day window and then runs one `router_session_adoptions` COUNT query
//! PER `(skill, session)` pair it found chosen — on an install with
//! thousands of events that is thousands of synchronous SQLite round trips
//! in one call. `api_skill_detail` re-ran this (plus the equally
//! full-table-scan `skill_feedback_counts_all`) on EVERY request, and every
//! open skill-detail browser tab polls that endpoint every 5s — so the cost
//! scaled with (open tabs / 5s), not with how often the underlying data
//! actually changed. Combined with `api_skill_detail` not running inside
//! `spawn_blocking` (fixed alongside this cache — see `skills.rs`), a
//! handful of concurrent viewers could tie up every tokio worker thread in
//! synchronous SQLite work long enough to make the whole server (including
//! unrelated static routes) stop responding until restarted.
//!
//! Scope: only the two aggregates that are NOT owner-scoped —
//! `skill_router_stats` and `skill_feedback_counts_all` both read across the
//! whole `router_events` / `skill_feedback` tables with no owner filter (the
//! router blends feedback/usage regardless of which pool produced it — see
//! `db/AGENTS.md`). The caller's owner-scoped pieces (`compare_resources`,
//! `max_usage`, `radar_avg`) stay computed per request: they're in-memory
//! folds over maps this cache already hands back, not additional DB scans,
//! and they legitimately differ per viewer (a non-admin's average must never
//! touch another user's private counts).
//!
//! Keyed by `db_path` rather than a single global slot so multiple
//! `Database` instances in one process (test binaries hammer this) each get
//! their own cache entry instead of one test's snapshot leaking into
//! another's assertions; in production a server process only ever opens one
//! `db_path`; so this is a correctness nicety there, not a perf feature.
//!
//! TTL is intentionally coarse (`CACHE_TTL_SECS`): a feedback vote can lag
//! up to that long before it moves the radar for OTHER viewers polling the
//! same skill. Documented, accepted tradeoff (PLANNING §2.2 doesn't cover
//! this — it's a fresh 2026-07 decision, not a re-litigation of an existing
//! invariant).
//!
//! ## Stale-while-revalidate (2026-07)
//!
//! A plain TTL cache still makes ONE unlucky caller pay the full recompute
//! cost synchronously: the request that lands right after the snapshot ages
//! out blocks on a fresh `skill_router_stats` + `skill_feedback_counts_all`
//! pass before it gets an answer. On a large install that is still a
//! multi-second stall for whoever's tab happens to poll at the wrong
//! instant, even with the N+1 fix in `skill_router_stats` — SQLite's
//! synchronous scan cost doesn't disappear, it just stops being multiplied
//! by "number of chosen sessions".
//!
//! So a cache miss on an EXPIRED (not absent) snapshot returns the stale
//! snapshot immediately and kicks off a background refresh on a detached
//! `std::thread` that opens its own `Database` connection (the caller's `db`
//! is a request-scoped, non-`Send`-across-await connection — the refresh
//! thread must not borrow it). Concurrent callers hitting the same expired
//! entry share one in-flight refresh via the `refreshing` claim registry
//! (same one-owner-wins pattern as `enrich_state::try_claim`) — the second,
//! third, … caller just returns the same stale snapshot without spawning
//! its own refresh.
//!
//! Only the very FIRST request for a `db_path` (no snapshot at all yet) pays
//! the synchronous cost — there is nothing stale to fall back to. Every
//! request after that gets an answer in the time it takes to clone two
//! `HashMap`s out of a mutex, never a live SQLite scan.
//!
//! **Staleness bound**: data can now lag by up to `CACHE_TTL_SECS` PLUS the
//! duration of one background refresh (previously: just `CACHE_TTL_SECS`,
//! but every Nth caller paid full latency instead of getting a fast
//! answer). This is the deliberate trade this section makes.

use crate::core::db::{Database, RouterSkillStats};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const CACHE_TTL_SECS: u64 = 45;

/// `(router_stats_all, feedback_counts_all)` — named so `router_and_feedback_stats`'s
/// signature doesn't trip clippy's type-complexity lint.
pub(super) type RouterAndFeedbackStats = (
    HashMap<String, RouterSkillStats>,
    HashMap<String, (i64, i64)>,
);

struct Cached {
    computed_at: Instant,
    router_stats: HashMap<String, RouterSkillStats>,
    feedback_all: HashMap<String, (i64, i64)>,
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static REG: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Process-global "a background refresh is already in flight for this
/// db_path" claim set — same one-owner-wins shape as
/// `enrich_state::try_claim`, but boolean rather than TTL'd: the refresh
/// thread always removes its own entry on exit (success, error, or panic
/// during unwind), so there's no separate staleness window to reason about
/// here.
fn refreshing() -> &'static Mutex<std::collections::HashSet<PathBuf>> {
    static REG: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// RAII guard that releases this db_path's refresh claim on drop — including
/// on an unwinding panic inside the refresh thread's closure, so a panicking
/// refresh can never permanently wedge future refreshes for that path.
struct RefreshClaim(PathBuf);

impl Drop for RefreshClaim {
    fn drop(&mut self) {
        refreshing()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// Try to claim the background-refresh slot for `db_path`. Returns `Some`
/// guard (spawn the refresh, drop the guard when done) if this caller is the
/// one that gets to refresh; `None` if another refresh is already in
/// flight for this path.
fn try_claim_refresh(db_path: &Path) -> Option<RefreshClaim> {
    let mut reg = refreshing().lock().unwrap_or_else(|e| e.into_inner());
    if reg.insert(db_path.to_path_buf()) {
        Some(RefreshClaim(db_path.to_path_buf()))
    } else {
        None
    }
}

/// Recompute both aggregates from `db` and store them as the fresh snapshot
/// for `db_path`. Used both for the synchronous first-ever-request path and
/// for the background refresh thread (which opens its own `Database`).
fn compute_and_store(
    db: &Database,
    db_path: &Path,
    since_ts: i64,
) -> anyhow::Result<RouterAndFeedbackStats> {
    #[cfg(test)]
    std::thread::sleep(test_hooks::slowdown(db_path));

    let router_stats = db.skill_router_stats(since_ts)?;
    let feedback_all = db.skill_feedback_counts_all()?;

    #[cfg(test)]
    test_hooks::record_compute_call(db_path);

    registry().lock().unwrap_or_else(|e| e.into_inner()).insert(
        db_path.to_path_buf(),
        Cached {
            computed_at: Instant::now(),
            router_stats: router_stats.clone(),
            feedback_all: feedback_all.clone(),
        },
    );
    Ok((router_stats, feedback_all))
}

/// Spawn (at most one, process-wide, per db_path) a detached background
/// refresh. No-op if a refresh for this path is already in flight.
fn spawn_background_refresh(db_path: PathBuf, since_ts: i64) {
    let Some(claim) = try_claim_refresh(&db_path) else {
        return;
    };
    std::thread::spawn(move || {
        let _claim = claim; // released on drop, incl. on panic unwind
        match Database::open(&db_path) {
            Ok(db) => {
                if let Err(e) = compute_and_store(&db, &db_path, since_ts) {
                    eprintln!(
                        "stats_cache background refresh failed for {}: {e}",
                        db_path.display()
                    );
                }
            }
            Err(e) => eprintln!(
                "stats_cache background refresh: could not open {}: {e}",
                db_path.display()
            ),
        }
    });
}

/// Returns `(router_stats_all, feedback_counts_all)`.
///
/// - No snapshot yet for `db_path`: computes synchronously (nothing to fall
///   back to) and blocks the caller — this only happens once per `db_path`
///   per process lifetime.
/// - Fresh snapshot (younger than `CACHE_TTL_SECS`): returned immediately,
///   no DB work.
/// - Stale snapshot (older than `CACHE_TTL_SECS`): returned immediately AS
///   IS, and a background refresh is kicked off (deduped across concurrent
///   callers) to replace it for the NEXT caller. `since_ts` is the radar
///   window cutoff (`api_skill_detail`'s rolling 90 days) — passed straight
///   through to `Database::skill_router_stats` on both the synchronous and
///   background recompute paths.
pub(super) fn router_and_feedback_stats(
    db: &Database,
    db_path: &Path,
    since_ts: i64,
) -> anyhow::Result<RouterAndFeedbackStats> {
    {
        let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = reg.get(db_path) {
            let snapshot = (c.router_stats.clone(), c.feedback_all.clone());
            if c.computed_at.elapsed().as_secs() < CACHE_TTL_SECS {
                return Ok(snapshot);
            }
            drop(reg);
            spawn_background_refresh(db_path.to_path_buf(), since_ts);
            return Ok(snapshot);
        }
    }
    // First-ever request for this db_path: nothing stale to serve, pay the
    // synchronous cost once.
    compute_and_store(db, db_path, since_ts)
}

/// Test-only: back-date the cached entry for `db_path` so the next call
/// sees it as expired, without an actual `CACHE_TTL_SECS`-long sleep.
/// No-op if there is no entry yet.
#[cfg(test)]
fn force_stale(db_path: &Path) {
    if let Some(c) = registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_mut(db_path)
    {
        c.computed_at = Instant::now()
            .checked_sub(std::time::Duration::from_secs(CACHE_TTL_SECS + 1))
            .expect("process has been up longer than the TTL");
    }
}

#[cfg(test)]
mod test_hooks {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    static SLOWDOWN_MS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();
    static COMPUTE_CALLS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

    /// Make every `compute_and_store` call (sync AND background-thread)
    /// sleep this long before touching the DB, so tests can prove a caller
    /// returned WITHOUT waiting for a slow recompute.
    pub(super) fn set_slowdown_ms(db_path: &Path, ms: u64) {
        let mut slowdown = SLOWDOWN_MS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if ms == 0 {
            slowdown.remove(db_path);
        } else {
            slowdown.insert(db_path.to_path_buf(), ms);
        }
    }

    pub(super) fn slowdown(db_path: &Path) -> Duration {
        let ms = SLOWDOWN_MS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(db_path)
            .copied()
            .unwrap_or(0);
        Duration::from_millis(ms)
    }

    pub(super) fn record_compute_call(db_path: &Path) {
        let mut calls = COMPUTE_CALLS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *calls.entry(db_path.to_path_buf()).or_insert(0) += 1;
    }

    pub(super) fn compute_calls(db_path: &Path) -> u64 {
        COMPUTE_CALLS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(db_path)
            .copied()
            .unwrap_or(0)
    }

    pub(super) fn reset(db_path: &Path) {
        set_slowdown_ms(db_path, 0);
        COMPUTE_CALLS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(db_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hooks_are_scoped_by_db_path() {
        let root = tempfile::tempdir().unwrap();
        let path_a = root.path().join("hook-a.db");
        let path_b = root.path().join("hook-b.db");

        test_hooks::reset(&path_a);
        test_hooks::reset(&path_b);
        test_hooks::set_slowdown_ms(&path_a, 25);
        test_hooks::record_compute_call(&path_a);
        test_hooks::record_compute_call(&path_a);
        test_hooks::record_compute_call(&path_b);

        assert_eq!(test_hooks::slowdown(&path_a).as_millis(), 25);
        assert_eq!(test_hooks::slowdown(&path_b).as_millis(), 0);
        assert_eq!(test_hooks::compute_calls(&path_a), 2);
        assert_eq!(test_hooks::compute_calls(&path_b), 1);

        test_hooks::reset(&path_a);
        assert_eq!(test_hooks::compute_calls(&path_a), 0);
        assert_eq!(test_hooks::compute_calls(&path_b), 1);
        test_hooks::reset(&path_b);
    }

    #[test]
    fn second_call_within_ttl_returns_the_stale_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("stats_cache_test.db");
        let db = Database::open(&db_path).unwrap();
        db.record_skill_feedback(
            chrono::Utc::now().timestamp(),
            "cache-skill",
            None,
            None,
            None,
            None,
            1,
            None,
        )
        .unwrap();

        let (_, fb1) = router_and_feedback_stats(&db, &db_path, 0).unwrap();
        assert_eq!(fb1.get("cache-skill"), Some(&(1, 0)));

        // Mutate the underlying table directly — a call within the TTL
        // must NOT observe this; it should still hand back the cached
        // snapshot taken before the insert.
        db.record_skill_feedback(
            chrono::Utc::now().timestamp(),
            "cache-skill",
            None,
            None,
            None,
            None,
            1,
            None,
        )
        .unwrap();
        let (_, fb2) = router_and_feedback_stats(&db, &db_path, 0).unwrap();
        assert_eq!(
            fb2.get("cache-skill"),
            Some(&(1, 0)),
            "second call inside the TTL window must return the cached snapshot, not re-scan"
        );
    }

    #[test]
    fn different_db_paths_never_share_a_cache_entry() {
        let tmp_a = tempfile::tempdir().unwrap();
        let tmp_b = tempfile::tempdir().unwrap();
        let path_a = tmp_a.path().join("a.db");
        let path_b = tmp_b.path().join("b.db");
        let db_a = Database::open(&path_a).unwrap();
        let db_b = Database::open(&path_b).unwrap();
        db_a.record_skill_feedback(
            chrono::Utc::now().timestamp(),
            "only-in-a",
            None,
            None,
            None,
            None,
            1,
            None,
        )
        .unwrap();

        let (_, fb_a) = router_and_feedback_stats(&db_a, &path_a, 0).unwrap();
        let (_, fb_b) = router_and_feedback_stats(&db_b, &path_b, 0).unwrap();
        assert!(fb_a.contains_key("only-in-a"));
        assert!(
            !fb_b.contains_key("only-in-a"),
            "db B's cache entry must not see db A's feedback row"
        );
    }

    /// Poll until `pred` is true or `timeout` elapses. Used to observe the
    /// background refresh thread's effect without a fixed sleep (which
    /// would either flake under load or waste time).
    fn wait_until(timeout: std::time::Duration, mut pred: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        loop {
            if pred() {
                return true;
            }
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn expired_snapshot_returns_immediately_and_refreshes_in_background() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("swr_test.db");
        test_hooks::reset(&db_path);
        let db = Database::open(&db_path).unwrap();
        db.record_skill_feedback(1, "swr-skill", None, None, None, None, 1, None)
            .unwrap();

        // Prime the cache (first-ever call, synchronous, no slowdown yet).
        let (_, fb1) = router_and_feedback_stats(&db, &db_path, 0).unwrap();
        assert_eq!(fb1.get("swr-skill"), Some(&(1, 0)));

        // Mutate the underlying table and force the snapshot to look expired.
        db.record_skill_feedback(2, "swr-skill", None, None, None, None, 1, None)
            .unwrap();
        force_stale(&db_path);

        // Make the recompute artificially slow so a synchronous call would
        // provably block for at least this long.
        test_hooks::set_slowdown_ms(&db_path, 300);

        let start = Instant::now();
        let (_, fb2) = router_and_feedback_stats(&db, &db_path, 0).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(
            fb2.get("swr-skill"),
            Some(&(1, 0)),
            "an expired-but-present snapshot must be served AS IS, not blocked on"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(150),
            "expired-snapshot call must return near-instantly, not wait out the \
             300ms simulated slow recompute; took {elapsed:?}"
        );

        // The background refresh should eventually land the fresh count.
        let refreshed = wait_until(std::time::Duration::from_secs(3), || {
            let reg = registry().lock().unwrap();
            reg.get(&db_path)
                .map(|c| c.feedback_all.get("swr-skill") == Some(&(2, 0)))
                .unwrap_or(false)
        });
        assert!(
            refreshed,
            "background refresh must eventually update the cache to the fresh count"
        );
        test_hooks::reset(&db_path);
    }

    #[test]
    fn concurrent_expired_calls_dedupe_to_one_background_refresh() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("swr_dedupe_test.db");
        test_hooks::reset(&db_path);
        let db = Database::open(&db_path).unwrap();
        db.record_skill_feedback(1, "dedupe-skill", None, None, None, None, 1, None)
            .unwrap();

        let _ = router_and_feedback_stats(&db, &db_path, 0).unwrap();
        // One compute call so far (the priming call above).
        assert_eq!(test_hooks::compute_calls(&db_path), 1);

        force_stale(&db_path);
        test_hooks::set_slowdown_ms(&db_path, 200);

        // Two calls in quick succession while the entry is expired: both
        // must return the stale snapshot immediately, and only ONE of them
        // may win the background-refresh claim.
        let (_, fb_first) = router_and_feedback_stats(&db, &db_path, 0).unwrap();
        let (_, fb_second) = router_and_feedback_stats(&db, &db_path, 0).unwrap();
        assert_eq!(fb_first.get("dedupe-skill"), Some(&(1, 0)));
        assert_eq!(fb_second.get("dedupe-skill"), Some(&(1, 0)));

        // Give the (single) background refresh time to finish, then assert
        // the total compute-call count is exactly 2 (prime + one refresh),
        // never 3 — a second background thread would double-count.
        wait_until(std::time::Duration::from_secs(3), || {
            test_hooks::compute_calls(&db_path) >= 2
        });
        // Settle briefly past the slowdown window so a wrongly-spawned
        // second refresh (if any) has had time to also complete and bump
        // the counter, so we're not just catching the race mid-flight.
        std::thread::sleep(std::time::Duration::from_millis(250));
        assert_eq!(
            test_hooks::compute_calls(&db_path),
            2,
            "two callers racing an expired snapshot must share ONE background refresh"
        );
        test_hooks::reset(&db_path);
    }
}

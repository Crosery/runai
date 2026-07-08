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

/// Returns `(router_stats_all, feedback_counts_all)`, recomputing from `db`
/// only when there is no cached snapshot for `db_path` yet or the cached one
/// is older than `CACHE_TTL_SECS`. `since_ts` is the radar window cutoff
/// (`api_skill_detail`'s rolling 90 days) — passed straight through to
/// `Database::skill_router_stats` on a cache miss.
pub(super) fn router_and_feedback_stats(
    db: &Database,
    db_path: &Path,
    since_ts: i64,
) -> anyhow::Result<RouterAndFeedbackStats> {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = reg.get(db_path)
        && c.computed_at.elapsed().as_secs() < CACHE_TTL_SECS
    {
        return Ok((c.router_stats.clone(), c.feedback_all.clone()));
    }
    let router_stats = db.skill_router_stats(since_ts)?;
    let feedback_all = db.skill_feedback_counts_all()?;
    reg.insert(
        db_path.to_path_buf(),
        Cached {
            computed_at: Instant::now(),
            router_stats: router_stats.clone(),
            feedback_all: feedback_all.clone(),
        },
    );
    Ok((router_stats, feedback_all))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

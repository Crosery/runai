//! In-memory "富集中" (enrich-in-progress) registry — PLANNING real-time
//! enrichment, the third state the DB cannot express.
//!
//! Enrichment status was binary: `resource_ai_summary.summary` empty (未富集) or
//! non-empty (已富集). Enrichment itself is async — either a detached
//! `recommend enrich` child fired by `market::spawn_enrich` (upload / install /
//! file-watch) or an in-process `reevaluate_skill` call spawned on a detached
//! thread by `recommend.rs::handle_feedback` after a `/feedback` vote — so
//! between "triggered" and "summary written" there is a real third state the
//! dashboard wants to show: 富集中. This is also true on a RE-enrich: editing an
//! already-summarized SKILL.md should flip the tag back to 富集中 while the new
//! summary regenerates — hence the timestamp comparison below, not just
//! "summary present wins".
//!
//! Process-global, in-memory map of `skill name -> unix-secs when marked`.
//! Deliberately NOT persisted: it is server-runtime state, and on restart the
//! map clears (anything genuinely pending is re-triggered by the file watcher
//! or completes and shows 已富集 on the next `/api/skills`).
//!
//! `status_for` is the single decision point: it resolves the tag AND lazily
//! clears the mark once the enrich has demonstrably finished (summary written
//! at/after the mark) or aged out past `ENRICH_TTL_SECS`. `try_claim` is the
//! race-free "claim the in-flight slot" entry point for callers (like
//! `/feedback`) that need to know whether THEY are the one spawning the
//! re-enrich, vs. `mark_enriching` which is a plain unconditional set used by
//! callers (`market::spawn_enrich`) that already know they're about to spawn.

use dashmap::DashMap;
use std::sync::OnceLock;

/// How long a name may stay 富集中 before we assume the enrich child failed or
/// never ran (e.g. no provider configured) and let it fall back.
const ENRICH_TTL_SECS: i64 = 300;

fn registry() -> &'static DashMap<String, i64> {
    static R: OnceLock<DashMap<String, i64>> = OnceLock::new();
    R.get_or_init(DashMap::new)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Mark a skill as enrich-in-progress (called when an enrich is triggered).
pub(super) fn mark_enriching(name: &str) {
    registry().insert(name.to_string(), now_unix());
}

/// Atomically claim the in-flight slot for `name`: if it is not currently
/// marked (or its mark is stale, aged past `ENRICH_TTL_SECS`), mark it now
/// and return `true` — this call is the one that owns the re-enrich and
/// must go on to actually trigger it. If a fresh mark already exists,
/// leave it untouched and return `false` — the caller must NOT spawn a
/// duplicate re-enrich; two concurrent feedback votes on the same skill
/// collapse into one queued LLM call, not two.
///
/// Uses `DashMap::entry` (a per-shard lock) rather than a separate
/// check-then-insert, so two requests racing on the same name cannot both
/// observe "not marked" and both spawn a re-enrich.
pub(super) fn try_claim(name: &str) -> bool {
    use dashmap::mapref::entry::Entry;
    let now = now_unix();
    match registry().entry(name.to_string()) {
        Entry::Occupied(mut e) => {
            if now - *e.get() < ENRICH_TTL_SECS {
                false
            } else {
                e.insert(now);
                true
            }
        }
        Entry::Vacant(e) => {
            e.insert(now);
            true
        }
    }
}

/// Stop tracking a skill.
pub(super) fn clear(name: &str) {
    registry().remove(name);
}

/// Resolve the 3-state enrichment tag for a skill.
///
/// `has_summary` = its `resource_ai_summary.summary` is non-empty; `summary_ts`
/// = that row's `updated_at` (None when no summary). Logic:
///   - not in-flight → `enriched` if it has a summary, else `unenriched`.
///   - in-flight and the summary was written AT/AFTER the mark → the enrich
///     finished: clear the mark, report `enriched`.
///   - in-flight and within TTL → `enriching` (covers both "no summary yet" and
///     "stale summary being regenerated after a file edit").
///   - in-flight but aged out → clear, fall back to summary presence.
pub(super) fn status_for(name: &str, has_summary: bool, summary_ts: Option<i64>) -> &'static str {
    // Copy the marked timestamp out, dropping the DashMap Ref before any
    // remove() to avoid a self-deadlock on the shard lock.
    let marked = match registry().get(name) {
        Some(e) => *e.value(),
        None => {
            return if has_summary {
                "enriched"
            } else {
                "unenriched"
            };
        }
    };
    let finished = has_summary && summary_ts.map(|s| s >= marked).unwrap_or(false);
    if finished {
        clear(name);
        return "enriched";
    }
    if now_unix() - marked < ENRICH_TTL_SECS {
        return "enriching";
    }
    registry().remove(name);
    if has_summary {
        "enriched"
    } else {
        "unenriched"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_state_transitions() {
        let n = "enrich_state_unit_skill_z";
        clear(n);
        // never triggered
        assert_eq!(status_for(n, false, None), "unenriched");
        assert_eq!(status_for(n, true, Some(123)), "enriched");

        // triggered, no summary yet → 富集中
        mark_enriching(n);
        assert_eq!(status_for(n, false, None), "enriching");
        // triggered, but only an OLD summary exists (re-enrich after edit) → 富集中
        assert_eq!(status_for(n, true, Some(0)), "enriching");
        // a summary written at/after the mark → finished, clears
        assert_eq!(status_for(n, true, Some(i64::MAX)), "enriched");
        // mark is now cleared
        assert_eq!(status_for(n, false, None), "unenriched");
    }

    #[test]
    fn try_claim_is_race_free_single_owner() {
        let n = "enrich_state_unit_skill_claim";
        clear(n);
        assert!(try_claim(n), "first claim on an unmarked name must succeed");
        assert!(
            !try_claim(n),
            "a second claim while the first is still fresh must be refused"
        );
        // status reflects the claim as in-flight.
        assert_eq!(status_for(n, false, None), "enriching");
        clear(n);
        assert!(
            try_claim(n),
            "after clear(), a fresh claim must succeed again"
        );
        clear(n);
    }
}

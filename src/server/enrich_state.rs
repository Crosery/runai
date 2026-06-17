//! In-memory "富集中" (enrich-in-progress) registry — PLANNING real-time
//! enrichment, the third state the DB cannot express.
//!
//! Enrichment status was binary: `resource_ai_summary.summary` empty (未富集) or
//! non-empty (已富集). Enrichment itself is async (a detached `recommend enrich`
//! child fired by `market::spawn_enrich` on upload / install / file-watch), so
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
//! at/after the mark) or aged out past `ENRICH_TTL_SECS`.

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
}

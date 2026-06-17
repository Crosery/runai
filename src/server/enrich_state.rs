//! In-memory "富集中" (enrich-in-progress) registry — PLANNING real-time
//! enrichment, the third state the DB cannot express.
//!
//! Enrichment status was binary: `resource_ai_summary.summary` empty (未富集) or
//! non-empty (已富集). Enrichment itself is async (a detached `recommend enrich`
//! child fired by `market::spawn_enrich` on upload / install / file-watch), so
//! between "triggered" and "summary written" there is a real third state the
//! dashboard wants to show: 富集中.
//!
//! This module is the source of that signal. It is a **process-global, in-memory
//! set** of skill names currently being enriched, keyed by name with the time it
//! was marked. Deliberately NOT persisted:
//! - it is server-runtime state, not metadata;
//! - on restart the set clears, and anything genuinely still pending is
//!   re-triggered by the file watcher (or its enrich child completes and writes
//!   the summary, flipping it to 已富集 on the next `/api/skills`).
//!
//! Lifecycle: `mark_enriching` on trigger; `/api/skills` calls `clear` once a
//! skill's summary appears, and `is_enriching` (TTL-bounded) decides the tag.
//! A name stuck past `ENRICH_TTL` (enrich died / no provider) ages out to
//! 未富集 rather than spinning 富集中 forever.

use dashmap::DashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// How long a name may stay 富集中 before we assume the enrich child failed or
/// never ran (e.g. no provider configured) and let it fall back to 未富集.
const ENRICH_TTL: Duration = Duration::from_secs(300);

fn registry() -> &'static DashMap<String, Instant> {
    static R: OnceLock<DashMap<String, Instant>> = OnceLock::new();
    R.get_or_init(DashMap::new)
}

/// Mark a skill as enrich-in-progress (called when an enrich is triggered).
pub(super) fn mark_enriching(name: &str) {
    registry().insert(name.to_string(), Instant::now());
}

/// Stop tracking a skill (called once its summary row appears).
pub(super) fn clear(name: &str) {
    registry().remove(name);
}

/// True if the skill is currently being enriched and hasn't aged out.
/// Lazily evicts expired entries so the map can't grow unbounded.
pub(super) fn is_enriching(name: &str) -> bool {
    // Drop the read ref before any remove() to avoid a self-deadlock on the
    // shard lock.
    let expired = match registry().get(name) {
        Some(e) => e.value().elapsed() >= ENRICH_TTL,
        None => return false,
    };
    if expired {
        registry().remove(name);
        false
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_then_is_enriching_then_clear() {
        let n = "enrich_state_unit_skill_a";
        assert!(!is_enriching(n));
        mark_enriching(n);
        assert!(is_enriching(n));
        clear(n);
        assert!(!is_enriching(n));
    }
}

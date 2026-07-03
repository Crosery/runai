//! Idempotent event recording for the activation/feedback protocol
//! (PLANNING §1.3). Backs `POST /skills/use/{name}` and `POST /feedback`.
//!
//! Contract:
//!   - `record_usage_event(event_id, skill, payload_hash, session_id, user_id)`
//!     returns [`UsageOutcome`] — `First` (recorded), `Duplicate` (same id +
//!     same hash, no-op), or `Conflict` (same id, different hash).
//!   - `record_activation_usage_event(...)` is the stronger activation path:
//!     it inserts the idempotency row, increments `resources.usage_count`,
//!     and records the session adoption in one SQLite transaction.
//!   - The idempotency table is `usage_events`, keyed by
//!     `(event_id, kind)`. `kind` separates the usage and feedback
//!     namespaces so the same client event_id can be reused across the
//!     two surfaces without collision.
//!
//! Payload hash canonicalization lives in the handler (it parses the raw
//! body bytes); the db layer only compares opaque hash strings.

use super::Database;
use anyhow::Result;
use rusqlite::params;

/// Outcome of an idempotent event record. Drives the HTTP status code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageOutcome {
    /// First time we see this (event_id, kind). Side effects were applied.
    First,
    /// Same (event_id, kind) + same payload_hash. No-op success.
    Duplicate,
    /// Same (event_id, kind) but different payload_hash. 409 conflict.
    Conflict,
}

impl Database {
    /// Try to record an idempotent event. The `kind` discriminates usage
    /// vs feedback so a single client event_id can be reused across the
    /// two namespaces. `payload_hash` is an opaque canonical hash string
    /// the caller computes from the request body.
    ///
    /// This is the single idempotency primitive — both `/skills/use` and
    /// `/feedback` route through it. The handler must bump `usage_count`
    /// / run `reevaluate_skill` ONLY when this returns `First`.
    pub fn record_usage_event(
        &self,
        event_id: &str,
        kind: &str,
        skill_name: &str,
        payload_hash: &str,
        session_id: &str,
        user_id: Option<&str>,
    ) -> Result<UsageOutcome> {
        let now = chrono::Utc::now().timestamp();
        // INSERT ... ON CONFLICT DO NOTHING lets us detect "already seen"
        // via changes() == 0 without a separate SELECT round-trip. Then a
        // single SELECT fetches the stored hash to compare.
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO usage_events\n             (event_id, kind, skill_name, payload_hash, session_id, user_id, ts)\n             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![event_id, kind, skill_name, payload_hash, session_id, user_id, now],
        )?;
        if inserted > 0 {
            return Ok(UsageOutcome::First);
        }
        // Row already existed — compare the stored hash.
        let stored_hash: String = self.conn.query_row(
            "SELECT payload_hash FROM usage_events WHERE event_id=?1 AND kind=?2",
            params![event_id, kind],
            |r| r.get(0),
        )?;
        if stored_hash == payload_hash {
            Ok(UsageOutcome::Duplicate)
        } else {
            Ok(UsageOutcome::Conflict)
        }
    }

    /// Record an activation usage event and apply its side effects in a
    /// single SQLite transaction. This is the hard guarantee behind
    /// `runai-client activate`: once the server ACKs, the idempotency row
    /// and the usage/session side effects are committed together. A replay
    /// with the same payload returns `Duplicate` without incrementing again;
    /// a replay with a different payload returns `Conflict` without touching
    /// usage_count.
    pub fn record_activation_usage_event(
        &self,
        event_id: &str,
        skill_name: &str,
        payload_hash: &str,
        session_id: &str,
        user_id: Option<&str>,
        resource_id: &str,
    ) -> Result<(UsageOutcome, i64)> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<(UsageOutcome, i64)> {
            let inserted = self.conn.execute(
                "INSERT OR IGNORE INTO usage_events
                 (event_id, kind, skill_name, payload_hash, session_id, user_id, ts)
                 VALUES (?1, 'usage', ?2, ?3, ?4, ?5, ?6)",
                params![event_id, skill_name, payload_hash, session_id, user_id, now],
            )?;
            if inserted == 0 {
                let stored_hash: String = self.conn.query_row(
                    "SELECT payload_hash FROM usage_events WHERE event_id=?1 AND kind='usage'",
                    params![event_id],
                    |r| r.get(0),
                )?;
                let usage_count = self.conn.query_row(
                    "SELECT usage_count FROM resources WHERE id=?1",
                    params![resource_id],
                    |r| r.get::<_, i64>(0),
                )?;
                if stored_hash == payload_hash {
                    return Ok((UsageOutcome::Duplicate, usage_count));
                }
                return Ok((UsageOutcome::Conflict, usage_count));
            }

            let affected = self.conn.execute(
                "UPDATE resources
                 SET usage_count = usage_count + 1, last_used_at = ?1
                 WHERE id = ?2",
                params![now, resource_id],
            )?;
            if affected == 0 {
                anyhow::bail!("resource not found in DB: {resource_id}");
            }
            if !session_id.is_empty() {
                self.conn.execute(
                    "INSERT INTO router_session_adoptions (session_id, skill_name, ts)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(session_id, skill_name) DO UPDATE SET ts = excluded.ts",
                    params![session_id, skill_name, now],
                )?;
            }
            let usage_count = self.conn.query_row(
                "SELECT usage_count FROM resources WHERE id=?1",
                params![resource_id],
                |r| r.get::<_, i64>(0),
            )?;
            Ok((UsageOutcome::First, usage_count))
        })();

        match result {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Count of recorded events for a given (event_id, kind). Test helper
    /// and a guard the handler can use to assert "no row on conflict path".
    pub fn usage_event_count(&self, event_id: &str, kind: &str) -> Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM usage_events WHERE event_id=?1 AND kind=?2",
            params![event_id, kind],
            |r| r.get(0),
        )?;
        Ok(n)
    }
}

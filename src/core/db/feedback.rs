//! `skill_feedback` CRUD + router-derived per-skill funnel stats.
//!
//! `skill_feedback` is an event-sourced log of explicit +-1 verdicts on a
//! skill. Rows are append-only — `record_skill_feedback` always inserts a
//! new row, never updates one in place — so `recent_skill_feedback` can show
//! full history and the count getters are always a fresh aggregate over the
//! log rather than a mutable running total.
//!
//! `owner_user_id` follows the same owner-pool convention as
//! `resources.owner_user_id`: `None` = public-pool skill, `Some(uid)` = that
//! user's private skill. Lookups match it with SQLite's null-safe `IS ?` so
//! a bound `NULL` only matches public rows, never a private one.

use super::Database;
use super::types::{RouterSkillStats, SkillFeedbackRow};
use anyhow::{Result, bail};
use rusqlite::params;
use std::collections::{HashMap, HashSet};

impl Database {
    /// Append one feedback event. `verdict` must be exactly `1` or `-1` —
    /// anything else is rejected rather than silently clamped, since a
    /// caller passing e.g. `0` almost certainly has a logic bug upstream.
    #[allow(clippy::too_many_arguments)]
    pub fn record_skill_feedback(
        &self,
        ts: i64,
        skill_name: &str,
        owner_user_id: Option<&str>,
        user_id: Option<&str>,
        session_id: Option<&str>,
        event_id: Option<i64>,
        verdict: i64,
        note: Option<&str>,
    ) -> Result<i64> {
        if verdict != 1 && verdict != -1 {
            bail!("skill feedback verdict must be +1 or -1, got {verdict}");
        }
        self.conn.execute(
            "INSERT INTO skill_feedback
                (ts, skill_name, owner_user_id, user_id, session_id, event_id, verdict, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                ts,
                skill_name,
                owner_user_id,
                user_id,
                session_id,
                event_id,
                verdict,
                note
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// `(positive_count, negative_count)` for one skill, scoped to a single
    /// owner-pool instance (null-safe: `None` matches only public rows).
    pub fn skill_feedback_counts(
        &self,
        skill_name: &str,
        owner_user_id: Option<&str>,
    ) -> Result<(i64, i64)> {
        let pos: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM skill_feedback
             WHERE skill_name = ?1 AND owner_user_id IS ?2 AND verdict = 1",
            params![skill_name, owner_user_id],
            |r| r.get(0),
        )?;
        let neg: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM skill_feedback
             WHERE skill_name = ?1 AND owner_user_id IS ?2 AND verdict = -1",
            params![skill_name, owner_user_id],
            |r| r.get(0),
        )?;
        Ok((pos, neg))
    }

    /// `(positive, negative)` per skill name, aggregated across every owner
    /// pool — the router blends feedback for a skill regardless of which
    /// pool instance (public vs a particular user's private copy) produced
    /// it.
    pub fn skill_feedback_counts_all(&self) -> Result<HashMap<String, (i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT skill_name, verdict, COUNT(*)
             FROM skill_feedback
             GROUP BY skill_name, verdict",
        )?;
        let rows = stmt.query_map([], |r| {
            let name: String = r.get(0)?;
            let verdict: i64 = r.get(1)?;
            let count: i64 = r.get(2)?;
            Ok((name, verdict, count))
        })?;
        let mut out: HashMap<String, (i64, i64)> = HashMap::new();
        for row in rows {
            let (name, verdict, count) = row?;
            let entry = out.entry(name).or_insert((0, 0));
            if verdict == 1 {
                entry.0 += count;
            } else if verdict == -1 {
                entry.1 += count;
            }
        }
        Ok(out)
    }

    /// Most recent feedback rows for one skill, newest first.
    pub fn recent_skill_feedback(
        &self,
        skill_name: &str,
        limit: usize,
    ) -> Result<Vec<SkillFeedbackRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, skill_name, owner_user_id, user_id, session_id, event_id, verdict, note
             FROM skill_feedback
             WHERE skill_name = ?1
             ORDER BY ts DESC, id DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![skill_name, limit as i64], |r| {
            Ok(SkillFeedbackRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                skill_name: r.get(2)?,
                owner_user_id: r.get(3)?,
                user_id: r.get(4)?,
                session_id: r.get(5)?,
                event_id: r.get(6)?,
                verdict: r.get(7)?,
                note: r.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Per-skill router funnel stats since `since_ts`: how often a skill
    /// reached the BM25 candidate stage, how often the router chose it, in
    /// how many distinct sessions it was chosen, and in how many of those
    /// sessions it was actually adopted (a matching
    /// `router_session_adoptions` row).
    ///
    /// Both `bm25_candidates_json` and `chosen_skills_json` are flat JSON
    /// arrays of plain skill-name strings (confirmed against
    /// `recommend::router`'s writers) — parsed here in Rust rather than via
    /// SQL `json_each` so one malformed row degrades to "no candidates /
    /// no chosen" for that row instead of aborting the whole aggregation.
    pub fn skill_router_stats(&self, since_ts: i64) -> Result<HashMap<String, RouterSkillStats>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, chosen_skills_json, bm25_candidates_json
             FROM router_events
             WHERE ts >= ?1",
        )?;
        let rows = stmt.query_map(params![since_ts], |r| {
            let session_id: String = r.get(0)?;
            let chosen_json: String = r.get(1)?;
            let candidates_json: String = r.get(2)?;
            Ok((session_id, chosen_json, candidates_json))
        })?;

        let mut stats: HashMap<String, RouterSkillStats> = HashMap::new();
        // Track distinct sessions each skill was chosen in, so
        // `chosen_sessions` counts sessions rather than raw events.
        let mut chosen_sessions: HashMap<String, HashSet<String>> = HashMap::new();

        for row in rows {
            let (session_id, chosen_json, candidates_json) = row?;
            let candidates: Vec<String> =
                serde_json::from_str(&candidates_json).unwrap_or_default();
            for name in &candidates {
                stats.entry(name.clone()).or_default().candidate_events += 1;
            }
            let chosen: Vec<String> = serde_json::from_str(&chosen_json).unwrap_or_default();
            for name in &chosen {
                stats.entry(name.clone()).or_default().chosen_events += 1;
                if !session_id.is_empty() {
                    chosen_sessions
                        .entry(name.clone())
                        .or_default()
                        .insert(session_id.clone());
                }
            }
        }

        for (name, sessions) in &chosen_sessions {
            let entry = stats.entry(name.clone()).or_default();
            entry.chosen_sessions = sessions.len() as i64;
            let mut adopted = 0i64;
            for session_id in sessions {
                let exists: i64 = self.conn.query_row(
                    "SELECT COUNT(*) FROM router_session_adoptions
                     WHERE session_id = ?1 AND skill_name = ?2",
                    params![session_id, name],
                    |r| r.get(0),
                )?;
                if exists > 0 {
                    adopted += 1;
                }
            }
            entry.adopted_sessions = adopted;
        }

        Ok(stats)
    }
}

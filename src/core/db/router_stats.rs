//! Aggregate summaries + bucketed timeline over `router_events`, optionally
//! scoped per-user. Feeds the dashboard's Overview / Activity charts.

use super::Database;
use super::types::{RouterModelStat, RouterStatsSummary, TimelineBucket};
use anyhow::Result;
use rusqlite::params;

impl Database {
    pub fn router_stats_summary(&self, since_ts: Option<i64>) -> Result<RouterStatsSummary> {
        self.router_stats_summary_filtered(since_ts, None)
    }

    /// Per-user variant. `user_id_filter = Some(uid)` scopes the stats
    /// to that user's router_events; None = every row.
    pub fn router_stats_summary_filtered(
        &self,
        since_ts: Option<i64>,
        user_id_filter: Option<&str>,
    ) -> Result<RouterStatsSummary> {
        let (total_calls, total_prompt, total_completion, total_reasoning, total_tokens, errors): (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = self.conn.query_row(
            "SELECT
                COUNT(*),
                COALESCE(SUM(prompt_tokens), 0),
                COALESCE(SUM(completion_tokens), 0),
                COALESCE(SUM(reasoning_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(CASE WHEN status != 'ok' THEN 1 ELSE 0 END), 0)
             FROM router_events
             WHERE (?1 IS NULL OR ts >= ?1)
               AND (?2 IS NULL OR user_id = ?2)",
            params![since_ts, user_id_filter],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )?;
        let avg_latency_ms: Option<f64> = self.conn.query_row(
            "SELECT AVG(latency_ms) FROM router_events
             WHERE (?1 IS NULL OR ts >= ?1)
               AND (?2 IS NULL OR user_id = ?2)
               AND status = 'ok'",
            params![since_ts, user_id_filter],
            |r| r.get(0),
        ).ok().flatten();
        let mut per_model = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT model, COUNT(*), COALESCE(SUM(total_tokens), 0)
             FROM router_events
             WHERE (?1 IS NULL OR ts >= ?1)
               AND (?2 IS NULL OR user_id = ?2)
             GROUP BY model
             ORDER BY 3 DESC",
        )?;
        let rows = stmt.query_map(params![since_ts, user_id_filter], |r| {
            Ok(RouterModelStat {
                model: r.get(0)?,
                calls: r.get(1)?,
                total_tokens: r.get(2)?,
            })
        })?;
        for row in rows {
            per_model.push(row?);
        }
        Ok(RouterStatsSummary {
            total_calls,
            total_prompt_tokens: total_prompt,
            total_completion_tokens: total_completion,
            total_reasoning_tokens: total_reasoning,
            total_tokens,
            errors,
            avg_latency_ms,
            per_model,
        })
    }

    /// Bucketed timeline of router activity for the dashboard chart.
    /// Returns N buckets of `bucket_secs` width ending at `now`, oldest first.
    /// Each bucket reports the count of total/hit/error events that fell into it.
    pub fn router_timeline(&self, bucket_secs: i64, buckets: i64) -> Result<Vec<TimelineBucket>> {
        self.router_timeline_filtered(bucket_secs, buckets, None)
    }

    /// Per-user variant of `router_timeline`.
    pub fn router_timeline_filtered(
        &self,
        bucket_secs: i64,
        buckets: i64,
        user_id_filter: Option<&str>,
    ) -> Result<Vec<TimelineBucket>> {
        let now = chrono::Utc::now().timestamp();
        let start = now - bucket_secs * buckets;
        let mut stmt = self.conn.prepare(
            "SELECT
                CAST((ts - ?1) / ?2 AS INTEGER) AS bucket_idx,
                COUNT(*) AS total,
                SUM(CASE WHEN status = 'ok' AND chosen_skills_json != '[]' THEN 1 ELSE 0 END) AS hits,
                SUM(CASE WHEN status != 'ok' THEN 1 ELSE 0 END) AS errors,
                COALESCE(AVG(latency_ms), 0) AS avg_lat
             FROM router_events
             WHERE ts >= ?1 AND ts < ?3
               AND (?4 IS NULL OR user_id = ?4)
             GROUP BY bucket_idx
             ORDER BY bucket_idx",
        )?;
        let mut by_idx: std::collections::HashMap<i64, (i64, i64, i64, f64)> =
            std::collections::HashMap::new();
        let rows = stmt.query_map(params![start, bucket_secs, now, user_id_filter], |r| {
            let idx: i64 = r.get(0)?;
            let total: i64 = r.get(1)?;
            let hits: i64 = r.get(2).unwrap_or(0);
            let errors: i64 = r.get(3).unwrap_or(0);
            let avg_lat: f64 = r.get(4).unwrap_or(0.0);
            Ok((idx, total, hits, errors, avg_lat))
        })?;
        for row in rows {
            let (idx, total, hits, errors, avg_lat) = row?;
            by_idx.insert(idx, (total, hits, errors, avg_lat));
        }
        let mut out = Vec::with_capacity(buckets as usize);
        for i in 0..buckets {
            let ts_start = start + i * bucket_secs;
            let (total, hits, errors, avg_lat) = by_idx.get(&i).copied().unwrap_or((0, 0, 0, 0.0));
            out.push(TimelineBucket {
                ts_start,
                total,
                hits,
                errors,
                avg_latency_ms: avg_lat,
            });
        }
        Ok(out)
    }
}

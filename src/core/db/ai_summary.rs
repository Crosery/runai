//! `resource_ai_summary` / `resource_user_rating` CRUD: per-skill AI summaries
//! and LLM quality scores keyed by skill name.

use super::Database;
use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;

impl Database {
    /// Look up the LLM quality score (0-10) for one skill. Returns 5 when
    /// the skill has no summary row yet.
    pub fn skill_llm_score(&self, name: &str) -> Result<i64> {
        let llm: i64 = self
            .conn
            .query_row(
                "SELECT llm_score FROM resource_ai_summary WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .unwrap_or(5);
        Ok(llm)
    }

    /// Batch-load `name -> llm_score` for all skills with a summary row.
    /// Used by the router for the hybrid prefilter and by the dashboard.
    pub fn skill_llm_scores_all(&self) -> Result<HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, llm_score FROM resource_ai_summary")?;
        let rows = stmt.query_map([], |r| {
            let n: String = r.get(0)?;
            let s: i64 = r.get(1)?;
            Ok((n, s))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (n, s) = row?;
            out.insert(n, s);
        }
        Ok(out)
    }

    /// Set the LLM-generated summary AND quality score (0-10) for a skill
    /// in one atomic insert/upsert. Empty summary is rejected; score is
    /// clamped to [0,10].
    pub fn set_skill_ai_summary_scored(
        &self,
        name: &str,
        summary: &str,
        llm_score: i64,
    ) -> Result<()> {
        if summary.trim().is_empty() {
            anyhow::bail!("refusing to write empty summary for {name}");
        }
        let score = llm_score.clamp(0, 10);
        self.conn.execute(
            "INSERT INTO resource_ai_summary (name, summary, llm_score, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET
                summary = excluded.summary,
                llm_score = excluded.llm_score,
                updated_at = excluded.updated_at",
            params![name, summary, score, chrono::Utc::now().timestamp()],
        )?;
        Ok(())
    }

    /// Drop AI summary for a skill. Called from `trash_resource` so deleted
    /// skills don't leak scoring data into the dashboard's enrichment count.
    pub fn delete_skill_scoring(&self, name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM resource_ai_summary WHERE name = ?1",
            params![name],
        )?;
        Ok(())
    }

    /// Wipe all LLM summaries. Next enrich pass rebuilds.
    pub fn reset_summaries(&self) -> Result<usize> {
        let s = self.conn.execute("DELETE FROM resource_ai_summary", [])?;
        Ok(s)
    }

    /// Look up AI summary for one skill (by name). Returns empty string when
    /// no summary has been generated yet.
    pub fn skill_ai_summary(&self, name: &str) -> Result<String> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT summary FROM resource_ai_summary WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .ok();
        Ok(row.unwrap_or_default())
    }

    /// Batch-load all summaries as a `name -> summary` map. Called once at
    /// the start of a router call so each candidate row only costs an O(1)
    /// HashMap lookup instead of an SQL round-trip.
    /// Batch-load `name -> updated_at` for AI summaries. Used by the
    /// incremental enrich pass to compare SKILL.md mtime against the
    /// stored summary timestamp to decide which skills need re-enriching.
    pub fn skill_ai_summary_timestamps(&self) -> Result<HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, updated_at FROM resource_ai_summary")?;
        let rows = stmt.query_map([], |r| {
            let n: String = r.get(0)?;
            let ts: i64 = r.get(1)?;
            Ok((n, ts))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (n, ts) = row?;
            out.insert(n, ts);
        }
        Ok(out)
    }

    pub fn skill_ai_summary_all(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, summary FROM resource_ai_summary")?;
        let rows = stmt.query_map([], |r| {
            let n: String = r.get(0)?;
            let s: String = r.get(1)?;
            Ok((n, s))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (n, s) = row?;
            out.insert(n, s);
        }
        Ok(out)
    }

    /// Insert or replace a skill's AI summary. Empty summary is rejected
    /// because the caller should `delete` instead of overwrite with blank.
    pub fn set_skill_ai_summary(&self, name: &str, summary: &str) -> Result<()> {
        if summary.trim().is_empty() {
            anyhow::bail!("refusing to write empty summary for {name}");
        }
        self.conn.execute(
            "INSERT INTO resource_ai_summary (name, summary, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET summary = excluded.summary, updated_at = excluded.updated_at",
            params![name, summary, chrono::Utc::now().timestamp()],
        )?;
        Ok(())
    }

    /// (rows, oldest, newest) summary count + freshness, used by the
    /// dashboard's enrichment-progress card.
    pub fn skill_ai_summary_stats(&self) -> Result<(i64, Option<i64>, Option<i64>)> {
        let (n, oldest, newest): (i64, Option<i64>, Option<i64>) = self.conn.query_row(
            "SELECT COUNT(*), MIN(updated_at), MAX(updated_at) FROM resource_ai_summary",
            [],
            |r| Ok((r.get(0)?, r.get(1).ok(), r.get(2).ok())),
        )?;
        Ok((n, oldest, newest))
    }
}

//! `resource_ai_summary` / `resource_user_rating` CRUD: per-skill AI summaries
//! and LLM quality scores keyed by `(owner_user_id, skill name)`.
//!
//! `resource_ai_summary` now also stores the structured recommend index:
//! `search_doc` for BM25, `router_card` for the router prompt, and freshness
//! hashes so the enrich worker can skip unchanged skills cheaply.

use super::Database;
use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;

use crate::core::resource::Resource;

#[derive(Debug, Clone, Default)]
pub struct SkillAiIndex {
    pub summary: String,
    pub search_doc: String,
    pub router_card: String,
    pub llm_score: i64,
    pub updated_at: i64,
    pub source_hash: String,
    pub prompt_hash: String,
    pub format_key: String,
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn compact_text(text: &str, limit: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(limit)
        .collect()
}

fn normalize_index(name: &str, mut row: SkillAiIndex) -> SkillAiIndex {
    row.summary = row.summary.trim().to_string();
    if row.search_doc.trim().is_empty() {
        row.search_doc = compact_text(&format!("{name} {}", row.summary), 2000);
    } else {
        row.search_doc = compact_text(&row.search_doc, 2000);
    }
    if row.router_card.trim().is_empty() {
        row.router_card = compact_text(&row.summary, 260);
    } else {
        row.router_card = compact_text(&row.router_card, 260);
    }
    row.source_hash = row.source_hash.trim().to_string();
    row.prompt_hash = row.prompt_hash.trim().to_string();
    row.format_key = row.format_key.trim().to_string();
    row
}

fn index_from_summary(summary: &str, llm_score: i64) -> SkillAiIndex {
    SkillAiIndex {
        summary: summary.trim().to_string(),
        search_doc: String::new(),
        router_card: String::new(),
        llm_score,
        updated_at: now_ts(),
        source_hash: String::new(),
        prompt_hash: String::new(),
        format_key: String::new(),
    }
}

fn owner_key(owner_user_id: Option<&str>) -> &str {
    owner_user_id.unwrap_or("")
}

impl Database {
    /// Stable map key for owner-aware AI index lookups. Empty owner = public
    /// pool; non-empty owner = that user's private pool.
    pub fn skill_ai_index_key(owner_user_id: Option<&str>, name: &str) -> String {
        format!("{}\u{0}{name}", owner_key(owner_user_id))
    }

    pub fn skill_ai_index_key_for_resource(resource: &Resource) -> String {
        Self::skill_ai_index_key(resource.owner_user_id.as_deref(), &resource.name)
    }

    /// Look up the structured AI index for one skill. Returns `None` when
    /// the skill has no summary row yet.
    pub fn skill_ai_index(&self, name: &str) -> Result<Option<SkillAiIndex>> {
        self.skill_ai_index_scoped(name, None)
    }

    /// Owner-aware lookup. `owner_user_id = None` means public-pool summary.
    pub fn skill_ai_index_scoped(
        &self,
        name: &str,
        owner_user_id: Option<&str>,
    ) -> Result<Option<SkillAiIndex>> {
        let row = self
            .conn
            .query_row(
                "SELECT summary, search_doc, router_card, llm_score, updated_at, source_hash, prompt_hash, format_key \
                 FROM resource_ai_summary WHERE owner_user_id = ?1 AND name = ?2",
                params![owner_key(owner_user_id), name],
                |r| {
                    Ok(SkillAiIndex {
                        summary: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        search_doc: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        router_card: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        llm_score: r.get::<_, Option<i64>>(3)?.unwrap_or(5),
                        updated_at: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        source_hash: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        prompt_hash: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                        format_key: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    })
                },
            )
            .ok();
        Ok(row.map(|row| normalize_index(name, row)))
    }

    pub fn skill_ai_index_for_resource(&self, resource: &Resource) -> Result<Option<SkillAiIndex>> {
        self.skill_ai_index_scoped(&resource.name, resource.owner_user_id.as_deref())
    }

    /// Batch-load public-pool structured index rows keyed by skill name.
    pub fn skill_ai_index_all(&self) -> Result<HashMap<String, SkillAiIndex>> {
        self.skill_ai_index_all_visible(None)
    }

    /// Batch-load visible structured index rows keyed by skill name. Private
    /// rows for `owner_user_id` shadow public rows of the same name.
    pub fn skill_ai_index_all_visible(
        &self,
        owner_user_id: Option<&str>,
    ) -> Result<HashMap<String, SkillAiIndex>> {
        let owner = owner_key(owner_user_id);
        let mut out = HashMap::new();
        if owner.is_empty() {
            let mut stmt = self.conn.prepare(
                "SELECT owner_user_id, name, summary, search_doc, router_card, llm_score, updated_at, source_hash, prompt_hash, format_key \
                 FROM resource_ai_summary WHERE owner_user_id = '' ORDER BY name",
            )?;
            let rows = stmt.query_map([], |r| {
                let name: String = r.get(1)?;
                let row = SkillAiIndex {
                    summary: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    search_doc: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    router_card: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    llm_score: r.get::<_, Option<i64>>(5)?.unwrap_or(5),
                    updated_at: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    source_hash: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    prompt_hash: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    format_key: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                };
                Ok((name.clone(), normalize_index(&name, row)))
            })?;
            for row in rows {
                let (name, row) = row?;
                out.insert(name, row);
            }
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT owner_user_id, name, summary, search_doc, router_card, llm_score, updated_at, source_hash, prompt_hash, format_key \
                 FROM resource_ai_summary WHERE owner_user_id = '' OR owner_user_id = ?1 \
                 ORDER BY name, CASE WHEN owner_user_id = '' THEN 0 ELSE 1 END",
            )?;
            let rows = stmt.query_map(params![owner], |r| {
                let name: String = r.get(1)?;
                let row = SkillAiIndex {
                    summary: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    search_doc: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    router_card: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    llm_score: r.get::<_, Option<i64>>(5)?.unwrap_or(5),
                    updated_at: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    source_hash: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    prompt_hash: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                    format_key: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                };
                Ok((name.clone(), normalize_index(&name, row)))
            })?;
            for row in rows {
                let (name, row) = row?;
                out.insert(name, row);
            }
        }
        Ok(out)
    }

    /// Batch-load every structured index row keyed by owner/name. Used by
    /// admin and enrich flows where multiple users can have same-named skills.
    pub fn skill_ai_index_all_by_resource_key(&self) -> Result<HashMap<String, SkillAiIndex>> {
        let mut stmt = self.conn.prepare(
            "SELECT owner_user_id, name, summary, search_doc, router_card, llm_score, updated_at, source_hash, prompt_hash, format_key \
             FROM resource_ai_summary",
        )?;
        let rows = stmt.query_map([], |r| {
            let owner: String = r.get::<_, Option<String>>(0)?.unwrap_or_default();
            let name: String = r.get(1)?;
            let row = SkillAiIndex {
                summary: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                search_doc: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                router_card: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                llm_score: r.get::<_, Option<i64>>(5)?.unwrap_or(5),
                updated_at: r.get::<_, Option<i64>>(6)?.unwrap_or(0),
                source_hash: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                prompt_hash: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                format_key: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
            };
            Ok((
                Self::skill_ai_index_key(if owner.is_empty() { None } else { Some(&owner) }, &name),
                normalize_index(&name, row),
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (name, row) = row?;
            out.insert(name, row);
        }
        Ok(out)
    }

    /// Look up the LLM quality score (0-10) for one skill. Returns 5 when
    /// the skill has no summary row yet.
    pub fn skill_llm_score(&self, name: &str) -> Result<i64> {
        Ok(self
            .skill_ai_index(name)?
            .map(|row| row.llm_score)
            .unwrap_or(5))
    }

    pub fn skill_llm_score_for_resource(&self, resource: &Resource) -> Result<i64> {
        Ok(self
            .skill_ai_index_for_resource(resource)?
            .map(|row| row.llm_score)
            .unwrap_or(5))
    }

    /// Batch-load `name -> llm_score` for all skills with a summary row.
    /// Used by the router for the hybrid prefilter and by the dashboard.
    pub fn skill_llm_scores_all(&self) -> Result<HashMap<String, i64>> {
        Ok(self
            .skill_ai_index_all()?
            .into_iter()
            .map(|(name, row)| (name, row.llm_score))
            .collect())
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
        let row = index_from_summary(summary, llm_score.clamp(0, 10));
        self.set_skill_ai_index(name, &row)
    }

    pub fn set_skill_ai_summary_scored_scoped(
        &self,
        name: &str,
        owner_user_id: Option<&str>,
        summary: &str,
        llm_score: i64,
    ) -> Result<()> {
        if summary.trim().is_empty() {
            anyhow::bail!("refusing to write empty summary for {name}");
        }
        let row = index_from_summary(summary, llm_score.clamp(0, 10));
        self.set_skill_ai_index_scoped(name, owner_user_id, &row)
    }

    /// Set the full structured AI index row for a skill.
    pub fn set_skill_ai_index(&self, name: &str, index: &SkillAiIndex) -> Result<()> {
        self.set_skill_ai_index_scoped(name, None, index)
    }

    pub fn set_skill_ai_index_scoped(
        &self,
        name: &str,
        owner_user_id: Option<&str>,
        index: &SkillAiIndex,
    ) -> Result<()> {
        if index.summary.trim().is_empty() {
            anyhow::bail!("refusing to write empty summary for {name}");
        }
        let score = index.llm_score.clamp(0, 10);
        self.conn.execute(
            "INSERT INTO resource_ai_summary (
                owner_user_id, name, summary, search_doc, router_card, llm_score, updated_at,
                source_hash, prompt_hash, format_key
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(owner_user_id, name) DO UPDATE SET
                summary = excluded.summary,
                search_doc = excluded.search_doc,
                router_card = excluded.router_card,
                llm_score = excluded.llm_score,
                updated_at = excluded.updated_at,
                source_hash = excluded.source_hash,
                prompt_hash = excluded.prompt_hash,
                format_key = excluded.format_key",
            params![
                owner_key(owner_user_id),
                name,
                index.summary,
                index.search_doc,
                index.router_card,
                score,
                index.updated_at,
                index.source_hash,
                index.prompt_hash,
                index.format_key,
            ],
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

    pub fn delete_skill_scoring_for_resource(&self, resource: &Resource) -> Result<()> {
        self.conn.execute(
            "DELETE FROM resource_ai_summary WHERE owner_user_id = ?1 AND name = ?2",
            params![owner_key(resource.owner_user_id.as_deref()), resource.name],
        )?;
        Ok(())
    }

    /// Wipe all LLM summaries. Next enrich pass rebuilds.
    pub fn reset_summaries(&self) -> Result<usize> {
        let s = self.conn.execute("DELETE FROM resource_ai_summary", [])?;
        Ok(s)
    }

    /// Look up the public-pool AI summary for one skill. Returns empty string
    /// when no summary has been generated yet.
    pub fn skill_ai_summary(&self, name: &str) -> Result<String> {
        Ok(self
            .skill_ai_index(name)?
            .map(|row| row.summary)
            .unwrap_or_default())
    }

    /// Batch-load public-pool `name -> updated_at` values. Kept for legacy
    /// dashboard code; new enrich freshness uses source/prompt hashes.
    pub fn skill_ai_summary_timestamps(&self) -> Result<HashMap<String, i64>> {
        Ok(self
            .skill_ai_index_all()?
            .into_iter()
            .map(|(name, row)| (name, row.updated_at))
            .collect())
    }

    pub fn skill_ai_summary_all(&self) -> Result<HashMap<String, String>> {
        Ok(self
            .skill_ai_index_all()?
            .into_iter()
            .map(|(name, row)| (name, row.summary))
            .collect())
    }

    /// Insert or replace a public-pool skill's AI summary. Empty summary is
    /// rejected because the caller should `delete` instead of overwrite with
    /// blank.
    pub fn set_skill_ai_summary(&self, name: &str, summary: &str) -> Result<()> {
        if summary.trim().is_empty() {
            anyhow::bail!("refusing to write empty summary for {name}");
        }
        let row = SkillAiIndex {
            summary: summary.trim().to_string(),
            search_doc: compact_text(&format!("{name} {summary}"), 2000),
            router_card: compact_text(summary, 260),
            llm_score: 5,
            updated_at: now_ts(),
            source_hash: String::new(),
            prompt_hash: String::new(),
            format_key: String::new(),
        };
        self.set_skill_ai_index(name, &row)
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

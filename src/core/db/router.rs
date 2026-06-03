//! `router_events` + `router_session_adoptions` CRUD and per-session memory.
//!
//! INVARIANT: `row_to_router_event` reads columns POSITIONALLY. Every SELECT in
//! this file lists columns in the exact order the converter expects:
//! id, ts, provider, model, prompt_tokens, completion_tokens, reasoning_tokens,
//! total_tokens, cache_hit_tokens, cache_miss_tokens, latency_ms,
//! chosen_skills_json, candidate_count, status, error_msg, session_id, mode,
//! user_prompt, cwd, bm25_kept, llm_raw_response, hook_output, llm_input
//! [, user_id]. Do NOT reorder a SELECT without updating the converter.

use super::Database;
use super::types::RouterEvent;
use anyhow::Result;
use rusqlite::params;

/// Map a SELECT row to a RouterEvent. Column order MUST be:
/// id, ts, provider, model, prompt_tokens, completion_tokens, reasoning_tokens,
/// total_tokens, cache_hit_tokens, cache_miss_tokens, latency_ms,
/// chosen_skills_json, candidate_count, status, error_msg,
/// session_id, mode, user_prompt, cwd, bm25_kept.
pub(super) fn row_to_router_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<RouterEvent> {
    Ok(RouterEvent {
        id: r.get(0)?,
        ts: r.get(1)?,
        provider: r.get(2)?,
        model: r.get(3)?,
        prompt_tokens: r.get(4)?,
        completion_tokens: r.get(5)?,
        reasoning_tokens: r.get(6)?,
        total_tokens: r.get(7)?,
        cache_hit_tokens: r.get(8)?,
        cache_miss_tokens: r.get(9)?,
        latency_ms: r.get(10)?,
        chosen_skills_json: r.get(11)?,
        candidate_count: r.get(12)?,
        status: r.get(13)?,
        error_msg: r.get(14)?,
        session_id: r.get(15)?,
        mode: r.get(16)?,
        user_prompt: r.get(17)?,
        cwd: r.get(18)?,
        bm25_kept: r.get(19)?,
        llm_raw_response: r.get(20)?,
        hook_output: r.get(21)?,
        llm_input: r
            .get::<_, Option<String>>(22)
            .unwrap_or_default()
            .unwrap_or_default(),
        user_id: r.get::<_, Option<String>>(23).unwrap_or_default(),
    })
}

impl Database {
    /// Recent router_events that picked this skill (its name is in chosen
    /// skills array). Uses json_each so substring matches in other skills'
    /// names don't bleed in.
    pub fn router_events_for_skill(&self, name: &str, limit: usize) -> Result<Vec<RouterEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, provider, model, prompt_tokens, completion_tokens, reasoning_tokens,
                    total_tokens, cache_hit_tokens, cache_miss_tokens, latency_ms,
                    chosen_skills_json, candidate_count, status, error_msg,
                    session_id, mode, user_prompt, cwd, bm25_kept,
                    llm_raw_response, hook_output, llm_input
             FROM router_events re
             WHERE EXISTS (
                SELECT 1 FROM json_each(re.chosen_skills_json) je WHERE je.value = ?1
             )
             ORDER BY ts DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![name, limit as i64], row_to_router_event)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn insert_router_event(&self, ev: &RouterEvent) -> Result<()> {
        // Cap user_prompt to bound DB size — full prompts can be megabytes
        // when users paste long context. 2 KB is enough to recognise intent in
        // the dashboard.
        let user_prompt_capped: String = ev.user_prompt.chars().take(2000).collect();
        let llm_raw_capped: String = ev.llm_raw_response.chars().take(2000).collect();
        let hook_out_capped: String = ev.hook_output.chars().take(6000).collect();
        // llm_input is the full user-message string the router sent to the
        // LLM — keep the cap generous (64 KB) so the dashboard's "router LLM
        // 实际收到的完整输入" panel shows the entire prompt including all
        // candidate listings + project context. 16 KB was clipping past
        // candidate ~20 of 30, making users think the LLM saw a truncated
        // candidate set (it didn't — only the DB copy was clipped).
        let llm_input_capped: String = ev.llm_input.chars().take(65536).collect();
        self.conn.execute(
            "INSERT INTO router_events (
                ts, provider, model,
                prompt_tokens, completion_tokens, reasoning_tokens, total_tokens,
                cache_hit_tokens, cache_miss_tokens,
                latency_ms, chosen_skills_json, candidate_count, status, error_msg,
                session_id, mode,
                user_prompt, cwd, bm25_kept,
                llm_raw_response, hook_output, llm_input
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                ev.ts,
                ev.provider,
                ev.model,
                ev.prompt_tokens,
                ev.completion_tokens,
                ev.reasoning_tokens,
                ev.total_tokens,
                ev.cache_hit_tokens,
                ev.cache_miss_tokens,
                ev.latency_ms,
                ev.chosen_skills_json,
                ev.candidate_count,
                ev.status,
                ev.error_msg,
                ev.session_id,
                ev.mode,
                user_prompt_capped,
                ev.cwd,
                ev.bm25_kept,
                llm_raw_capped,
                hook_out_capped,
                llm_input_capped,
            ],
        )?;
        Ok(())
    }

    /// Return the deduped set of skill names this session has already had
    /// recommended AND actually adopted by the main Claude agent in this
    /// session. Used by the router to avoid re-pushing skills the agent
    /// already pulled in — but skills it was offered and *declined* stay
    /// eligible for future recommendations. Adoption is signalled by the
    /// `runai recommend used <name>` call.
    pub fn router_session_routed_skills(&self, session_id: &str) -> Result<Vec<String>> {
        if session_id.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT skill_name FROM router_session_adoptions
             WHERE session_id = ?1
             ORDER BY ts DESC
             LIMIT 100",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            let s: String = r.get(0)?;
            Ok(s)
        })?;
        let mut seen = std::collections::BTreeSet::new();
        for row in rows {
            seen.insert(row?);
        }
        Ok(seen.into_iter().collect())
    }

    /// Every distinct skill name the router has *recommended* (not necessarily
    /// adopted) in this session, newest-first. Distinct from
    /// `router_session_routed_skills` which is the adoption set: a skill the
    /// router proposed but the agent didn't Read still shows up here. Used
    /// by the hook output to remind the main agent which skills it already
    /// saw this session — encourages dedup without strictly suppressing
    /// re-recommendation.
    pub fn router_session_recommended_skills(&self, session_id: &str) -> Result<Vec<String>> {
        if session_id.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT chosen_skills_json FROM router_events
             WHERE session_id = ?1 AND status = 'ok'
             ORDER BY ts DESC
             LIMIT 50",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            let s: String = r.get(0)?;
            Ok(s)
        })?;
        let mut seen = std::collections::BTreeSet::new();
        let mut order: Vec<String> = Vec::new();
        for row in rows {
            let json = row?;
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(&json) {
                for name in arr {
                    if seen.insert(name.clone()) {
                        order.push(name);
                    }
                }
            }
        }
        Ok(order)
    }

    /// Return (llm_input, llm_raw_response) pairs for every successful
    /// router call in this session, ordered by ts ASC. Used by the
    /// Conversation `session_mode` to rebuild a chat-history messages
    /// array fed back to the router LLM. Limit caps cost — at default
    /// 30 turns the conversation payload stays bounded.
    pub fn router_session_turn_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        if session_id.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT llm_input, llm_raw_response FROM router_events
             WHERE session_id = ?1 AND status = 'ok'
             ORDER BY ts ASC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |r| {
            let i: String = r.get(0)?;
            let o: String = r.get(1)?;
            Ok((i, o))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Record that a skill was adopted (Read + acted on by the main agent)
    /// in a given Claude Code session. Called by `runai recommend used`.
    /// Idempotent: PRIMARY KEY (session_id, skill_name) collapses repeated
    /// adoption signals within one session to a single row.
    pub fn record_session_adoption(&self, session_id: &str, skill_name: &str) -> Result<()> {
        if session_id.is_empty() || skill_name.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO router_session_adoptions (session_id, skill_name, ts)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(session_id, skill_name) DO UPDATE SET ts = excluded.ts",
            params![session_id, skill_name, now],
        )?;
        Ok(())
    }

    pub fn router_recent_events(&self, limit: usize) -> Result<Vec<RouterEvent>> {
        self.router_events_paged(None, limit, 0, None, false)
    }

    /// Paginated query used by the web dashboard. `since_ts` filters to events
    /// after that UTC seconds timestamp; `model` filters by exact model name;
    /// `hit_only` keeps only ok-status rows with a non-empty chosen array.
    pub fn router_events_paged(
        &self,
        since_ts: Option<i64>,
        limit: usize,
        offset: usize,
        model: Option<&str>,
        hit_only: bool,
    ) -> Result<Vec<RouterEvent>> {
        self.router_events_paged_filtered(since_ts, limit, offset, model, hit_only, None)
    }

    /// Like `router_events_paged` but adds an optional `user_id_filter`.
    /// Some("uid") → only that user's events. None → every row (admin /
    /// compat view). v15 multi-user: Activity tab default is the
    /// authenticated user's own events so no one accidentally sees
    /// other tenants' prompts / cwd.
    pub fn router_events_paged_filtered(
        &self,
        since_ts: Option<i64>,
        limit: usize,
        offset: usize,
        model: Option<&str>,
        hit_only: bool,
        user_id_filter: Option<&str>,
    ) -> Result<Vec<RouterEvent>> {
        let mut sql = String::from(
            "SELECT id, ts, provider, model, prompt_tokens, completion_tokens, reasoning_tokens,
                    total_tokens, cache_hit_tokens, cache_miss_tokens, latency_ms,
                    chosen_skills_json, candidate_count, status, error_msg,
                    session_id, mode, user_prompt, cwd, bm25_kept,
                    llm_raw_response, hook_output, llm_input, user_id
             FROM router_events WHERE 1=1",
        );
        if since_ts.is_some() {
            sql.push_str(" AND ts >= ?1");
        } else {
            sql.push_str(" AND (?1 IS NULL OR 1=1)");
        }
        if model.is_some() {
            sql.push_str(" AND model = ?2");
        } else {
            sql.push_str(" AND (?2 IS NULL OR 1=1)");
        }
        if hit_only {
            sql.push_str(" AND status = 'ok' AND chosen_skills_json != '[]'");
        }
        if user_id_filter.is_some() {
            sql.push_str(" AND user_id = ?5");
        } else {
            sql.push_str(" AND (?5 IS NULL OR 1=1)");
        }
        sql.push_str(" ORDER BY ts DESC LIMIT ?3 OFFSET ?4");

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![
                since_ts,
                model,
                limit as i64,
                offset as i64,
                user_id_filter
            ],
            row_to_router_event,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Return total count matching the same filters as `router_events_paged`,
    /// used by the dashboard to render pagination controls.
    pub fn router_events_count(
        &self,
        since_ts: Option<i64>,
        model: Option<&str>,
        hit_only: bool,
    ) -> Result<i64> {
        self.router_events_count_filtered(since_ts, model, hit_only, None)
    }

    pub fn router_events_count_filtered(
        &self,
        since_ts: Option<i64>,
        model: Option<&str>,
        hit_only: bool,
        user_id_filter: Option<&str>,
    ) -> Result<i64> {
        let mut sql = String::from("SELECT COUNT(*) FROM router_events WHERE 1=1");
        if since_ts.is_some() {
            sql.push_str(" AND ts >= ?1");
        } else {
            sql.push_str(" AND (?1 IS NULL OR 1=1)");
        }
        if model.is_some() {
            sql.push_str(" AND model = ?2");
        } else {
            sql.push_str(" AND (?2 IS NULL OR 1=1)");
        }
        if hit_only {
            sql.push_str(" AND status = 'ok' AND chosen_skills_json != '[]'");
        }
        if user_id_filter.is_some() {
            sql.push_str(" AND user_id = ?3");
        } else {
            sql.push_str(" AND (?3 IS NULL OR 1=1)");
        }
        let n: i64 = self.conn.query_row(
            &sql,
            params![since_ts, model, user_id_filter],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// All router_events newer than `since_ts`, ordered (session_id, ts).
    /// Used by feedback mining to walk consecutive same-session events and
    /// treat the next-event's `user_prompt` as feedback on the prior chosen.
    pub fn router_events_since_ordered(&self, since_ts: i64) -> Result<Vec<RouterEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, provider, model, prompt_tokens, completion_tokens, reasoning_tokens,
                    total_tokens, cache_hit_tokens, cache_miss_tokens, latency_ms,
                    chosen_skills_json, candidate_count, status, error_msg,
                    session_id, mode, user_prompt, cwd, bm25_kept,
                    llm_raw_response, hook_output, llm_input
             FROM router_events
             WHERE ts >= ?1 AND session_id != ''
             ORDER BY session_id, ts",
        )?;
        let rows = stmt.query_map(params![since_ts], row_to_router_event)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn router_event_by_id(&self, id: i64) -> Result<Option<RouterEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts, provider, model, prompt_tokens, completion_tokens, reasoning_tokens,
                    total_tokens, cache_hit_tokens, cache_miss_tokens, latency_ms,
                    chosen_skills_json, candidate_count, status, error_msg,
                    session_id, mode, user_prompt, cwd, bm25_kept,
                    llm_raw_response, hook_output, llm_input
             FROM router_events WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_router_event)?;
        if let Some(row) = rows.next() {
            return Ok(Some(row?));
        }
        Ok(None)
    }
}

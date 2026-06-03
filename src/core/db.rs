use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::cli_target::CliTarget;
use crate::core::resource::{Resource, ResourceKind, Source, TrashEntry, UsageStat};

#[derive(Debug, Clone)]
pub struct RouterEvent {
    /// SQLite rowid. None when constructed for insert; Some when read back.
    pub id: Option<i64>,
    pub ts: i64,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub reasoning_tokens: i64,
    pub total_tokens: i64,
    pub cache_hit_tokens: i64,
    pub cache_miss_tokens: i64,
    pub latency_ms: i64,
    pub chosen_skills_json: String,
    pub candidate_count: i64,
    pub status: String,
    pub error_msg: Option<String>,
    pub session_id: String,
    pub mode: String,
    /// Original user prompt that triggered this router call. Empty for legacy
    /// rows written before schema v7. Capped at ~2 KB on insert to bound DB size.
    pub user_prompt: String,
    /// Working directory the hook was invoked in (cwd from Claude Code hook JSON).
    /// Empty for legacy rows.
    pub cwd: String,
    /// How many candidates remained after BM25 prefilter (= candidate_count when
    /// prefilter was bypassed). Lets dashboards see prefilter efficacy.
    pub bm25_kept: i64,
    /// Raw text the router LLM returned (the first ~2 KB) — the mode tag line
    /// plus skill names, before any post-processing. Empty for legacy rows.
    /// Lets users see "what did the model literally say" in the dashboard.
    pub llm_raw_response: String,
    /// The hook stdout that runai injected into Claude Code (the markdown block
    /// the main agent receives). Capped at ~6 KB. Empty for rows where the
    /// hook didn't inject anything (chosen=[]) or pre-schema-v8.
    pub hook_output: String,
    /// Full user-message string sent to the router LLM (history block +
    /// already_routed list + candidate listing + current user prompt).
    /// Capped at ~16 KB. Empty for pre-schema-v13 rows. Useful for
    /// diagnosing mis-routes — answers "what did the model see?".
    pub llm_input: String,
    /// Authenticated user_id this event belongs to. None for pre-schema-v15
    /// rows and for unauthenticated requests during the compat window
    /// (prefs.require_auth=false). Per-user dashboard views filter on this.
    pub user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct User {
    pub user_id: String,
    pub username: String,
    pub password_hash: String,
    pub api_key_hash: String,
    pub is_admin: bool,
    pub disabled: bool,
    pub prefs_json: String,
    pub created_at: i64,
}

#[derive(Debug, Clone)]
pub struct RouterModelStat {
    pub model: String,
    pub calls: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone)]
pub struct TimelineBucket {
    pub ts_start: i64,
    pub total: i64,
    pub hits: i64,
    pub errors: i64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Clone)]
pub struct RouterStatsSummary {
    pub total_calls: i64,
    pub total_prompt_tokens: i64,
    pub total_completion_tokens: i64,
    pub total_reasoning_tokens: i64,
    pub total_tokens: i64,
    pub errors: i64,
    pub avg_latency_ms: Option<f64>,
    pub per_model: Vec<RouterModelStat>,
}

pub struct Database {
    conn: Connection,
}

/// Map a SELECT row to a RouterEvent. Column order MUST be:
/// id, ts, provider, model, prompt_tokens, completion_tokens, reasoning_tokens,
/// total_tokens, cache_hit_tokens, cache_miss_tokens, latency_ms,
/// chosen_skills_json, candidate_count, status, error_msg,
/// session_id, mode, user_prompt, cwd, bm25_kept.
fn row_to_router_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<RouterEvent> {
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
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Borrow the underlying SQLite connection. Use only when the typed
    /// CRUD on this struct can't express what you need (e.g. ad-hoc joins
    /// over `router_events.user_id`).
    pub fn conn_ref(&self) -> &Connection {
        &self.conn
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS resources (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('skill', 'mcp')),
                description TEXT,
                directory TEXT NOT NULL,
                source_type TEXT NOT NULL,
                source_meta TEXT,
                installed_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS resource_targets (
                resource_id TEXT NOT NULL,
                cli_target TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (resource_id, cli_target),
                FOREIGN KEY (resource_id) REFERENCES resources(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS group_members (
                group_id TEXT NOT NULL,
                resource_id TEXT NOT NULL,
                PRIMARY KEY (group_id, resource_id),
                FOREIGN KEY (resource_id) REFERENCES resources(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS trash_entries (
                id TEXT PRIMARY KEY,
                resource_id TEXT NOT NULL,
                name TEXT NOT NULL,
                kind TEXT NOT NULL CHECK (kind IN ('skill', 'mcp')),
                deleted_at INTEGER NOT NULL,
                payload_json TEXT NOT NULL
            );",
        )?;

        // Schema versioning
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
        )?;

        let version: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )?;

        if version < 2 {
            // Recreate group_members without FK constraint
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS group_members_new (
                    group_id TEXT NOT NULL,
                    resource_id TEXT NOT NULL,
                    PRIMARY KEY (group_id, resource_id)
                );
                INSERT OR IGNORE INTO group_members_new SELECT group_id, resource_id FROM group_members;
                DROP TABLE IF EXISTS group_members;
                ALTER TABLE group_members_new RENAME TO group_members;

                DELETE FROM schema_version;
                INSERT INTO schema_version VALUES (2);"
            )?;
        }

        if version < 3 {
            self.conn.execute_batch(
                "ALTER TABLE resources ADD COLUMN usage_count INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE resources ADD COLUMN last_used_at INTEGER;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (3);",
            )?;
        }

        if version < 4 {
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS trash_entries (
                    id TEXT PRIMARY KEY,
                    resource_id TEXT NOT NULL,
                    name TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK (kind IN ('skill', 'mcp')),
                    deleted_at INTEGER NOT NULL,
                    payload_json TEXT NOT NULL
                 );
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (4);",
            )?;
        }

        if version < 5 {
            // Router LLM call telemetry. Records every runai recommend run so
            // users can audit token spend, latency, and which skills the
            // external router model picked. Privacy-safe: no prompt text.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS router_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts INTEGER NOT NULL,
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    prompt_tokens INTEGER NOT NULL DEFAULT 0,
                    completion_tokens INTEGER NOT NULL DEFAULT 0,
                    reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                    total_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_hit_tokens INTEGER NOT NULL DEFAULT 0,
                    cache_miss_tokens INTEGER NOT NULL DEFAULT 0,
                    latency_ms INTEGER NOT NULL DEFAULT 0,
                    chosen_skills_json TEXT NOT NULL DEFAULT '[]',
                    candidate_count INTEGER NOT NULL DEFAULT 0,
                    status TEXT NOT NULL DEFAULT 'ok',
                    error_msg TEXT
                 );
                 CREATE INDEX IF NOT EXISTS idx_router_events_ts ON router_events(ts);
                 CREATE INDEX IF NOT EXISTS idx_router_events_model ON router_events(model);
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (5);",
            )?;
        }

        if version < 6 {
            // Per-session router memory + mode tag. session_id lets the router
            // see which skills it has already pushed in the same Claude Code
            // session, so it can avoid re-recommending the same skill on every
            // turn. mode records whether the picked set was tagged as
            // 'compatible' (skills can co-load) or 'exclusive' (user must pick
            // one), defaulting to 'exclusive' for legacy rows.
            self.conn.execute_batch(
                "ALTER TABLE router_events ADD COLUMN session_id TEXT NOT NULL DEFAULT '';
                 ALTER TABLE router_events ADD COLUMN mode TEXT NOT NULL DEFAULT 'exclusive';
                 CREATE INDEX IF NOT EXISTS idx_router_events_session ON router_events(session_id);
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (6);",
            )?;
        }

        if version < 7 {
            // Web dashboard needs the original user_prompt and cwd to render
            // per-event detail. bm25_kept records how many candidates the BM25
            // prefilter kept (= candidate_count when prefilter bypassed) so
            // dashboards can show prefilter efficacy.
            self.conn.execute_batch(
                "ALTER TABLE router_events ADD COLUMN user_prompt TEXT NOT NULL DEFAULT '';
                 ALTER TABLE router_events ADD COLUMN cwd TEXT NOT NULL DEFAULT '';
                 ALTER TABLE router_events ADD COLUMN bm25_kept INTEGER NOT NULL DEFAULT 0;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (7);",
            )?;
        }

        if version < 8 {
            // Capture what the router LLM literally returned plus the exact
            // markdown block we injected into Claude Code's hook stdout, so
            // the dashboard can show "the model said X, we injected Y".
            self.conn.execute_batch(
                "ALTER TABLE router_events ADD COLUMN llm_raw_response TEXT NOT NULL DEFAULT '';
                 ALTER TABLE router_events ADD COLUMN hook_output TEXT NOT NULL DEFAULT '';
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (8);",
            )?;
        }

        if version < 9 {
            // Per-skill AI-generated summary used to enrich BM25 doc text so
            // cross-language queries can hit English-only descriptions. Keyed
            // by skill name (stable across reinstall / re-adopt) rather than
            // resource_id (which changes with source type).
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS resource_ai_summary (
                    name TEXT PRIMARY KEY,
                    summary TEXT NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (9);",
            )?;
        }

        if version < 10 {
            // LLM-side quality score (0-100) generated by the enrich pass +
            // user-side star ratings (1-5) collected via the dashboard. The
            // router blends them into a combined signal it shows the LLM
            // alongside each candidate.
            self.conn.execute_batch(
                "ALTER TABLE resource_ai_summary ADD COLUMN llm_score INTEGER NOT NULL DEFAULT 50;
                 CREATE TABLE IF NOT EXISTS resource_user_rating (
                    name TEXT PRIMARY KEY,
                    stars INTEGER NOT NULL CHECK (stars >= 1 AND stars <= 5),
                    note TEXT NOT NULL DEFAULT '',
                    updated_at INTEGER NOT NULL
                 );
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (10);",
            )?;
        }

        if version < 11 {
            // Simplify scoring: both LLM and user score on a unified 0-10
            // scale (was 0-100 for LLM, 1-5 stars for user). Re-create the
            // user-rating table to relax the CHECK constraint; rescale
            // existing data lossily-but-deterministically (1-5 stars *2,
            // 0-100 llm /10).
            self.conn.execute_batch(
                "UPDATE resource_ai_summary SET llm_score = MAX(0, MIN(10, llm_score / 10));

                 CREATE TABLE IF NOT EXISTS resource_user_rating_new (
                    name TEXT PRIMARY KEY,
                    score INTEGER NOT NULL CHECK (score >= 1 AND score <= 10),
                    note TEXT NOT NULL DEFAULT '',
                    updated_at INTEGER NOT NULL
                 );
                 INSERT INTO resource_user_rating_new (name, score, note, updated_at)
                   SELECT name, MIN(10, MAX(1, stars * 2)), note, updated_at
                   FROM resource_user_rating;
                 DROP TABLE resource_user_rating;
                 ALTER TABLE resource_user_rating_new RENAME TO resource_user_rating;

                 UPDATE resource_ai_summary SET llm_score = 5 WHERE llm_score = 5;

                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (11);",
            )?;
            // Adjust default. SQLite can't easily ALTER COLUMN DEFAULT
            // without recreate; the default only matters for fresh inserts
            // which set_skill_ai_summary_scored always supplies explicitly,
            // so leaving the on-disk default at 50 is harmless — application
            // code never relies on it.
        }

        if version < 12 {
            // Distinguish user-entered ratings (network /api/skills/.../rating
            // POST) from auto-mined ratings (feedback signals dug out of
            // same-session next-prompt text). 'manual' wins over 'auto' so
            // the mining pass never overwrites what the user typed in the
            // dashboard.
            self.conn.execute_batch(
                "ALTER TABLE resource_user_rating ADD COLUMN source TEXT NOT NULL DEFAULT 'manual';
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (12);",
            )?;
        }

        if version < 13 {
            // Capture the full user-message string sent to the router LLM
            // (system prompt + history + already_routed + candidate listing +
            // current prompt). Lets the dashboard show "what the model
            // literally saw" so users can diagnose mis-routes. Capped on
            // insert to ~16 KB so the DB doesn't bloat on long sessions.
            self.conn.execute_batch(
                "ALTER TABLE router_events ADD COLUMN llm_input TEXT NOT NULL DEFAULT '';
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (13);",
            )?;
        }

        if version < 14 {
            // Per-session adoption log: records skills the main agent
            // actually pulled in (via `runai recommend used <name>`).
            // Replaces "this session already saw the skill in router_events"
            // as the dedup signal — only adopted skills are suppressed from
            // future recommendations within the same session.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS router_session_adoptions (
                    session_id TEXT NOT NULL,
                    skill_name TEXT NOT NULL,
                    ts INTEGER NOT NULL,
                    PRIMARY KEY (session_id, skill_name)
                 );
                 CREATE INDEX IF NOT EXISTS idx_session_adoptions_session
                   ON router_session_adoptions(session_id);
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (14);",
            )?;
        }

        if version < 15 {
            // Multi-user support: per-user accounts, per-user skill ownership
            // (resources.owner_user_id NULL = public pool admin-owned, set =
            // private user-owned at ~/.runai/users/<username>/skills/<name>),
            // optional library "favorites" table for subscribing to public
            // skills, and user_id stamp on router_events for per-user
            // dashboard views. router_events.user_id stays NULL for
            // pre-migration rows and for unauthenticated requests during the
            // compat window (prefs.require_auth=false). Auth + library logic
            // lives in src/core/auth.rs and src/core/manager.rs.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS users (
                    user_id TEXT PRIMARY KEY,
                    username TEXT NOT NULL UNIQUE,
                    password_hash TEXT NOT NULL,
                    api_key_hash TEXT NOT NULL,
                    is_admin INTEGER NOT NULL DEFAULT 0,
                    disabled INTEGER NOT NULL DEFAULT 0,
                    prefs_json TEXT NOT NULL DEFAULT '{}',
                    created_at INTEGER NOT NULL
                 );
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_users_username ON users(username);
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_users_api_key_hash ON users(api_key_hash);

                 CREATE TABLE IF NOT EXISTS user_skill_library (
                    user_id TEXT NOT NULL,
                    skill_name TEXT NOT NULL,
                    added_at INTEGER NOT NULL,
                    PRIMARY KEY (user_id, skill_name)
                 );
                 CREATE INDEX IF NOT EXISTS idx_user_library_user
                   ON user_skill_library(user_id);

                 ALTER TABLE resources ADD COLUMN owner_user_id TEXT;
                 CREATE INDEX IF NOT EXISTS idx_resources_owner
                   ON resources(owner_user_id);

                 ALTER TABLE router_events ADD COLUMN user_id TEXT;
                 CREATE INDEX IF NOT EXISTS idx_router_events_user_id
                   ON router_events(user_id);

                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (15);",
            )?;
        }

        Ok(())
    }

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

    pub fn insert_resource(&self, res: &Resource) -> Result<()> {
        self.conn.execute(
            "INSERT INTO resources (id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                description = excluded.description,
                directory = excluded.directory,
                source_type = excluded.source_type,
                source_meta = excluded.source_meta,
                owner_user_id = excluded.owner_user_id",
            params![
                res.id,
                res.name,
                res.kind.as_str(),
                res.description,
                res.directory.to_string_lossy().to_string(),
                res.source.source_type(),
                res.source.to_meta_json(),
                res.installed_at,
                res.usage_count as i64,
                res.last_used_at,
                res.owner_user_id,
            ],
        )?;
        Ok(())
    }

    /// Collapse duplicate skill rows that share the same `name`.
    ///
    /// Background: a skill can accumulate multiple DB rows over time (e.g.
    /// installed once via GitHub then re-adopted by `runai scan` after the
    /// user moved the dir). Two rows with the same name diverge `resource_count()`
    /// (counts all rows) from `list_resources()` (dedupes by name) — the user
    /// then sees "280 skills" in the header but only 278 in the list. Worse,
    /// `status()` overcounts and `enable_resource(id)` may target the wrong row.
    ///
    /// Strategy: keep the row with the largest `installed_at`. For losers,
    /// retarget any `group_members` rows to the keeper id (INSERT OR IGNORE
    /// to dodge PK conflicts), then delete `resource_targets` and `resources`
    /// rows for losers. Returns the number of rows removed.
    pub fn dedupe_skills_by_name(&self) -> Result<usize> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM resources WHERE kind = 'skill' \
             GROUP BY name HAVING COUNT(*) > 1",
        )?;
        let dup_names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        let mut total_removed = 0usize;
        for name in dup_names {
            // Pick keeper = max(installed_at), tiebreak by id (stable).
            let keeper_id: String = self.conn.query_row(
                "SELECT id FROM resources WHERE kind = 'skill' AND name = ?1 \
                 ORDER BY installed_at DESC, id ASC LIMIT 1",
                params![name],
                |row| row.get(0),
            )?;

            // Loser ids = same name, not the keeper.
            let mut id_stmt = self.conn.prepare(
                "SELECT id FROM resources WHERE kind = 'skill' AND name = ?1 AND id != ?2",
            )?;
            let loser_ids: Vec<String> = id_stmt
                .query_map(params![name, keeper_id], |row| row.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            drop(id_stmt);

            for loser in &loser_ids {
                // Re-point group_members from loser to keeper. INSERT OR IGNORE
                // handles the PK collision when the keeper is already in the
                // same group (we just want the loser row gone).
                self.conn.execute(
                    "INSERT OR IGNORE INTO group_members (group_id, resource_id) \
                     SELECT group_id, ?1 FROM group_members WHERE resource_id = ?2",
                    params![keeper_id, loser],
                )?;
                self.conn.execute(
                    "DELETE FROM group_members WHERE resource_id = ?1",
                    params![loser],
                )?;
                self.conn.execute(
                    "DELETE FROM resource_targets WHERE resource_id = ?1",
                    params![loser],
                )?;
                self.conn
                    .execute("DELETE FROM resources WHERE id = ?1", params![loser])?;
                total_removed += 1;
            }
        }
        Ok(total_removed)
    }

    pub fn get_resource(&self, id: &str) -> Result<Option<Resource>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id
             FROM resources WHERE id = ?1"
        )?;

        let mut rows = stmt.query(params![id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(None),
        };

        let kind_str: String = row.get(2)?;
        let source_type: String = row.get(5)?;
        let source_meta: String = row.get::<_, Option<String>>(6)?.unwrap_or_default();

        Ok(Some(Resource {
            id: row.get(0)?,
            name: row.get(1)?,
            kind: kind_str.parse().unwrap_or(ResourceKind::Skill),
            description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            directory: PathBuf::from(row.get::<_, String>(4)?),
            source: Source::from_meta_json(&source_type, &source_meta).unwrap_or(Source::Local {
                path: PathBuf::new(),
            }),
            installed_at: row.get(7)?,
            enabled: HashMap::new(),
            usage_count: row.get::<_, Option<i64>>(8)?.unwrap_or(0) as u64,
            last_used_at: row.get(9)?,
            owner_user_id: row.get::<_, Option<String>>(10)?,
        }))
    }

    pub fn list_resources(
        &self,
        kind: Option<ResourceKind>,
        _enabled_for: Option<CliTarget>,
    ) -> Result<Vec<Resource>> {
        let mut resources = match kind {
            Some(k) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id
                     FROM resources WHERE kind = ?1 ORDER BY name"
                )?;
                self.collect_resources(&mut stmt, params![k.as_str()])?
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id
                     FROM resources ORDER BY name"
                )?;
                self.collect_resources(&mut stmt, params![])?
            }
        };
        for res in &mut resources {
            res.enabled = HashMap::new();
        }
        Ok(resources)
    }

    /// Owner-scoped variant of [`list_resources`].
    ///
    /// `owner = None` → public-pool resources only (`owner_user_id IS NULL`).
    /// `owner = Some(uid)` → public pool ∪ this user's private resources.
    /// `owner = Some("*")` → everything (admin override; matches every row).
    pub fn list_resources_for_user(
        &self,
        kind: Option<ResourceKind>,
        owner: Option<&str>,
    ) -> Result<Vec<Resource>> {
        // Build the `owner_user_id` predicate once; SQL stays static-shaped so
        // sqlite can cache the plan.
        let owner_pred = match owner {
            None => "owner_user_id IS NULL",
            Some("*") => "1=1",
            Some(_) => "(owner_user_id IS NULL OR owner_user_id = ?2)",
        };
        let mut resources = match kind {
            Some(k) => {
                let sql = format!(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id
                     FROM resources WHERE kind = ?1 AND {owner_pred} ORDER BY name"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                match owner {
                    Some(uid) if uid != "*" => {
                        self.collect_resources(&mut stmt, params![k.as_str(), uid])?
                    }
                    _ => self.collect_resources(&mut stmt, params![k.as_str()])?,
                }
            }
            None => {
                let sql = format!(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id
                     FROM resources WHERE {} ORDER BY name",
                    owner_pred.replace("?2", "?1")
                );
                let mut stmt = self.conn.prepare(&sql)?;
                match owner {
                    Some(uid) if uid != "*" => self.collect_resources(&mut stmt, params![uid])?,
                    _ => self.collect_resources(&mut stmt, params![])?,
                }
            }
        };
        for res in &mut resources {
            res.enabled = HashMap::new();
        }
        Ok(resources)
    }

    /// Look up a resource by `(name, owner)`. Private rows win over public
    /// ones with the same name when `owner` is Some — that matches the
    /// runtime semantic ("my private skill shadows the public one of the
    /// same name"). `owner = None` matches the public pool exclusively.
    /// `owner = Some("*")` is the admin scope — matches any owner.
    pub fn find_resource_by_name_for_user(
        &self,
        kind: ResourceKind,
        name: &str,
        owner: Option<&str>,
    ) -> Result<Option<Resource>> {
        match owner {
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id
                     FROM resources
                     WHERE kind = ?1 AND name = ?2 AND owner_user_id IS NULL
                     ORDER BY installed_at DESC LIMIT 1",
                )?;
                let mut resources = self.collect_resources(&mut stmt, params![kind.as_str(), name])?;
                Ok(resources.pop())
            }
            Some("*") => {
                // Admin scope: any owner. Prefer the most recently installed
                // row so the dashboard "drill into a skill" picks the
                // freshest copy (private installs win over an older public).
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id
                     FROM resources
                     WHERE kind = ?1 AND name = ?2
                     ORDER BY CASE WHEN owner_user_id IS NULL THEN 1 ELSE 0 END, installed_at DESC LIMIT 1",
                )?;
                let mut resources = self.collect_resources(&mut stmt, params![kind.as_str(), name])?;
                Ok(resources.pop())
            }
            Some(uid) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at, owner_user_id
                     FROM resources
                     WHERE kind = ?1 AND name = ?2 AND (owner_user_id IS NULL OR owner_user_id = ?3)
                     ORDER BY CASE WHEN owner_user_id IS NULL THEN 1 ELSE 0 END, installed_at DESC LIMIT 1",
                )?;
                let mut resources = self.collect_resources(&mut stmt, params![kind.as_str(), name, uid])?;
                Ok(resources.pop())
            }
        }
    }

    fn collect_resources(
        &self,
        stmt: &mut rusqlite::Statement,
        params: impl rusqlite::Params,
    ) -> Result<Vec<Resource>> {
        let rows = stmt.query_map(params, |row| {
            let kind_str: String = row.get(2)?;
            let source_type: String = row.get(5)?;
            let source_meta: String = row.get::<_, Option<String>>(6)?.unwrap_or_default();

            Ok(Resource {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: kind_str.parse().unwrap_or(ResourceKind::Skill),
                description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                directory: PathBuf::from(row.get::<_, String>(4)?),
                source: Source::from_meta_json(&source_type, &source_meta).unwrap_or(
                    Source::Local {
                        path: PathBuf::new(),
                    },
                ),
                installed_at: row.get(7)?,
                enabled: HashMap::new(),
                usage_count: row.get::<_, Option<i64>>(8)?.unwrap_or(0) as u64,
                last_used_at: row.get(9)?,
                owner_user_id: row.get::<_, Option<String>>(10)?,
            })
        })?;

        let mut resources = Vec::new();
        for row in rows {
            resources.push(row?);
        }
        Ok(resources)
    }

    /// Increment usage_count and set last_used_at. Returns rows affected (0 if id not found).
    pub fn record_usage(&self, id: &str) -> Result<usize> {
        let now = chrono::Utc::now().timestamp();
        let affected = self.conn.execute(
            "UPDATE resources SET usage_count = usage_count + 1, last_used_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(affected)
    }

    /// Return usage stats for all resources, sorted by usage_count DESC.
    pub fn get_usage_stats(&self) -> Result<Vec<UsageStat>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, usage_count, last_used_at FROM resources ORDER BY usage_count DESC, name ASC"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(UsageStat {
                id: row.get(0)?,
                name: row.get(1)?,
                count: row.get::<_, i64>(2)? as u64,
                last_used_at: row.get(3)?,
            })
        })?;
        let mut stats = Vec::new();
        for row in rows {
            stats.push(row?);
        }
        Ok(stats)
    }

    pub fn update_description(&self, id: &str, description: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE resources SET description = ?1 WHERE id = ?2",
            params![description, id],
        )?;
        Ok(())
    }

    pub fn insert_trash_entry(&self, entry: &TrashEntry) -> Result<()> {
        let payload_json = serde_json::to_string(entry)?;
        self.conn.execute(
            "INSERT INTO trash_entries (id, resource_id, name, kind, deleted_at, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
                resource_id = excluded.resource_id,
                name = excluded.name,
                kind = excluded.kind,
                deleted_at = excluded.deleted_at,
                payload_json = excluded.payload_json",
            params![
                entry.id,
                entry.resource_id,
                entry.name,
                entry.kind.as_str(),
                entry.deleted_at,
                payload_json,
            ],
        )?;
        Ok(())
    }

    pub fn get_trash_entry(&self, id: &str) -> Result<Option<TrashEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload_json FROM trash_entries WHERE id = ?1")?;
        let mut rows = stmt.query(params![id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(None),
        };
        let payload_json: String = row.get(0)?;
        Ok(Some(serde_json::from_str(&payload_json)?))
    }

    pub fn list_trash_entries(&self) -> Result<Vec<TrashEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload_json FROM trash_entries ORDER BY deleted_at DESC, name ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

        let mut entries = Vec::new();
        for row in rows {
            let payload_json = row?;
            entries.push(serde_json::from_str(&payload_json)?);
        }
        Ok(entries)
    }

    pub fn delete_trash_entry(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM trash_entries WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn delete_resource(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM resources WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn add_group_member(&self, group_id: &str, resource_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO group_members (group_id, resource_id) VALUES (?1, ?2)",
            params![group_id, resource_id],
        )?;
        Ok(())
    }

    pub fn remove_group_member(&self, group_id: &str, resource_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM group_members WHERE group_id = ?1 AND resource_id = ?2",
            params![group_id, resource_id],
        )?;
        Ok(())
    }

    pub fn get_group_members(&self, group_id: &str) -> Result<Vec<Resource>> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.name, r.kind, r.description, r.directory, r.source_type, r.source_meta, r.installed_at, r.usage_count, r.last_used_at, r.owner_user_id
             FROM resources r JOIN group_members gm ON r.id = gm.resource_id
             WHERE gm.group_id = ?1 ORDER BY r.name"
        )?;

        let mut resources = self.collect_resources(&mut stmt, params![group_id])?;
        for res in &mut resources {
            res.enabled = HashMap::new();
        }
        Ok(resources)
    }

    pub fn get_groups_for_resource(&self, resource_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT group_id FROM group_members WHERE resource_id = ?1")?;
        let rows = stmt.query_map(params![resource_id], |row| row.get(0))?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    /// Batch-load every (resource_id → group_ids) mapping in one round-trip.
    /// The router calls this once per request to splice `[group:X,Y]` tags
    /// into the candidate listing without N+1 queries.
    pub fn groups_for_all_resources(&self) -> Result<HashMap<String, Vec<String>>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource_id, group_id FROM group_members ORDER BY resource_id, group_id",
        )?;
        let rows = stmt.query_map([], |row| {
            let rid: String = row.get(0)?;
            let gid: String = row.get(1)?;
            Ok((rid, gid))
        })?;
        let mut out: HashMap<String, Vec<String>> = HashMap::new();
        for row in rows {
            let (rid, gid) = row?;
            out.entry(rid).or_default().push(gid);
        }
        Ok(out)
    }

    pub fn take_groups_for_resource(&self, resource_id: &str) -> Result<Vec<String>> {
        let groups = self.get_groups_for_resource(resource_id)?;
        self.conn.execute(
            "DELETE FROM group_members WHERE resource_id = ?1",
            params![resource_id],
        )?;
        Ok(groups)
    }

    pub fn resource_count(&self) -> Result<(usize, usize)> {
        let skills: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM resources WHERE kind = 'skill'",
            [],
            |r| r.get(0),
        )?;
        Ok((skills as usize, 0))
    }

    pub fn schema_version(&self) -> i64 {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0)
    }

    /// Get group member IDs without joining resources table.
    /// Returns raw resource_id strings like "local:foo" or "mcp:bar".
    pub fn get_group_member_ids(&self, group_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT resource_id FROM group_members WHERE group_id = ?1")?;
        let rows = stmt.query_map(params![group_id], |row| row.get(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    pub fn skill_count(&self) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM resources WHERE kind = 'skill'",
            [],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    // ===================================================================
    //  Users (schema v15+): username / password / api_key auth and per-user
    //  ownership of skills. owner_user_id NULL on resources = public pool.
    // ===================================================================

    pub fn create_user(
        &self,
        user_id: &str,
        username: &str,
        password_hash: &str,
        api_key_hash: &str,
        is_admin: bool,
    ) -> Result<()> {
        let ts = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO users (user_id, username, password_hash, api_key_hash,
                                is_admin, disabled, prefs_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, '{}', ?6)",
            params![
                user_id,
                username,
                password_hash,
                api_key_hash,
                if is_admin { 1 } else { 0 },
                ts,
            ],
        )?;
        Ok(())
    }

    pub fn find_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, username, password_hash, api_key_hash,
                    is_admin, disabled, prefs_json, created_at
             FROM users WHERE username = ?1",
        )?;
        let user = stmt
            .query_row(params![username], row_to_user)
            .ok();
        Ok(user)
    }

    pub fn find_user_by_api_key_hash(&self, api_key_hash: &str) -> Result<Option<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, username, password_hash, api_key_hash,
                    is_admin, disabled, prefs_json, created_at
             FROM users WHERE api_key_hash = ?1",
        )?;
        let user = stmt
            .query_row(params![api_key_hash], row_to_user)
            .ok();
        Ok(user)
    }

    pub fn find_user_by_id(&self, user_id: &str) -> Result<Option<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, username, password_hash, api_key_hash,
                    is_admin, disabled, prefs_json, created_at
             FROM users WHERE user_id = ?1",
        )?;
        let user = stmt
            .query_row(params![user_id], row_to_user)
            .ok();
        Ok(user)
    }

    pub fn list_users(&self) -> Result<Vec<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, username, password_hash, api_key_hash,
                    is_admin, disabled, prefs_json, created_at
             FROM users ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_user)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn set_user_admin(&self, user_id: &str, is_admin: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET is_admin = ?1 WHERE user_id = ?2",
            params![if is_admin { 1 } else { 0 }, user_id],
        )?;
        Ok(())
    }

    pub fn set_user_disabled(&self, user_id: &str, disabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET disabled = ?1 WHERE user_id = ?2",
            params![if disabled { 1 } else { 0 }, user_id],
        )?;
        Ok(())
    }

    pub fn update_user_prefs(&self, user_id: &str, prefs_json: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET prefs_json = ?1 WHERE user_id = ?2",
            params![prefs_json, user_id],
        )?;
        Ok(())
    }

    pub fn rotate_api_key(&self, user_id: &str, new_api_key_hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET api_key_hash = ?1 WHERE user_id = ?2",
            params![new_api_key_hash, user_id],
        )?;
        Ok(())
    }

    // ===================================================================
    //  user_skill_library: a user's "favorites" subscription to public-pool
    //  skills (resources.owner_user_id IS NULL). Private skills the user
    //  owns are NOT tracked here — they're identified via owner_user_id.
    // ===================================================================

    pub fn library_add(&self, user_id: &str, skill_name: &str) -> Result<()> {
        let ts = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT OR IGNORE INTO user_skill_library (user_id, skill_name, added_at)
             VALUES (?1, ?2, ?3)",
            params![user_id, skill_name, ts],
        )?;
        Ok(())
    }

    pub fn library_remove(&self, user_id: &str, skill_name: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM user_skill_library WHERE user_id = ?1 AND skill_name = ?2",
            params![user_id, skill_name],
        )?;
        Ok(())
    }

    /// Drop every user's library reference to this `skill_name`. Used by
    /// `trash_resource` when a public-pool skill is deleted — every
    /// subscriber loses the orphan entry so the UI doesn't show a row
    /// that 404s on click.
    pub fn library_remove_for_all(&self, skill_name: &str) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM user_skill_library WHERE skill_name = ?1",
            params![skill_name],
        )?;
        Ok(n)
    }

    /// Sweep `user_skill_library` for entries pointing at skills that no
    /// longer exist in `resources` (kind='skill'). Returns the row count
    /// removed. Run at startup so a database imported from an older
    /// release (pre-`library_remove_for_all`-on-trash) doesn't leave the
    /// dashboard with "我的库 N" rows that 404 on click.
    pub fn cleanup_orphan_library_entries(&self) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM user_skill_library
             WHERE skill_name NOT IN (
                 SELECT name FROM resources WHERE kind = 'skill'
             )",
            [],
        )?;
        Ok(n)
    }

    pub fn library_list(&self, user_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT skill_name FROM user_skill_library
             WHERE user_id = ?1 ORDER BY added_at DESC",
        )?;
        let rows = stmt.query_map(params![user_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn library_contains(&self, user_id: &str, skill_name: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM user_skill_library
             WHERE user_id = ?1 AND skill_name = ?2",
            params![user_id, skill_name],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn library_clear(&self, user_id: &str) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM user_skill_library WHERE user_id = ?1",
            params![user_id],
        )?;
        Ok(n)
    }

    pub fn library_count(&self, user_id: &str) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM user_skill_library WHERE user_id = ?1",
            params![user_id],
            |r| r.get(0),
        )?;
        Ok(count as usize)
    }

    /// Top N public skills by global usage_count, used to pre-fill a new
    /// user's library so their first /recommend isn't empty.
    pub fn top_public_skills(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT name FROM resources
             WHERE kind = 'skill' AND owner_user_id IS NULL
             ORDER BY usage_count DESC, installed_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

fn row_to_user(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        user_id: r.get(0)?,
        username: r.get(1)?,
        password_hash: r.get(2)?,
        api_key_hash: r.get(3)?,
        is_admin: r.get::<_, i64>(4)? != 0,
        disabled: r.get::<_, i64>(5)? != 0,
        prefs_json: r.get(6)?,
        created_at: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_creates_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        let version: i64 = db
            .conn
            .query_row("SELECT version FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 15);
    }

    #[test]
    fn migration_v3_adds_usage_columns() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        let version = db.schema_version();
        assert!(version >= 3, "schema version should be >= 3, got {version}");

        // Verify columns exist by inserting and reading back
        let source = crate::core::resource::Source::Local {
            path: PathBuf::from("/tmp"),
        };
        let res = Resource {
            id: "local:test".into(),
            name: "test".into(),
            kind: ResourceKind::Skill,
            description: String::new(),
            directory: PathBuf::from("/tmp"),
            source,
            installed_at: 0,
            enabled: std::collections::HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: None,
        };
        db.insert_resource(&res).unwrap();

        let loaded = db.get_resource("local:test").unwrap().unwrap();
        assert_eq!(loaded.usage_count, 0);
        assert_eq!(loaded.last_used_at, None);
    }

    #[test]
    fn record_usage_increments_count() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        let source = crate::core::resource::Source::Local {
            path: PathBuf::from("/tmp"),
        };
        let res = Resource {
            id: "local:foo".into(),
            name: "foo".into(),
            kind: ResourceKind::Skill,
            description: String::new(),
            directory: PathBuf::from("/tmp"),
            source,
            installed_at: 0,
            enabled: std::collections::HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: None,
        };
        db.insert_resource(&res).unwrap();

        db.record_usage("local:foo").unwrap();
        db.record_usage("local:foo").unwrap();

        let loaded = db.get_resource("local:foo").unwrap().unwrap();
        assert_eq!(loaded.usage_count, 2);
        assert!(loaded.last_used_at.is_some());
    }

    #[test]
    fn record_usage_unknown_resource_returns_zero_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        // Should not error, but affect 0 rows
        let affected = db.record_usage("nonexistent").unwrap();
        assert_eq!(affected, 0);
    }

    #[test]
    fn get_usage_stats_sorted_by_count() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        for (id, name) in &[("local:a", "a"), ("local:b", "b"), ("local:c", "c")] {
            let source = crate::core::resource::Source::Local {
                path: PathBuf::from("/tmp"),
            };
            let res = Resource {
                id: id.to_string(),
                name: name.to_string(),
                kind: ResourceKind::Skill,
                description: String::new(),
                directory: PathBuf::from("/tmp"),
                source,
                installed_at: 0,
                enabled: std::collections::HashMap::new(),
                usage_count: 0,
                last_used_at: None,
                owner_user_id: None,
            };
            db.insert_resource(&res).unwrap();
        }

        // b: 3 uses, a: 1 use, c: 0 uses
        db.record_usage("local:b").unwrap();
        db.record_usage("local:b").unwrap();
        db.record_usage("local:b").unwrap();
        db.record_usage("local:a").unwrap();

        let stats = db.get_usage_stats().unwrap();
        assert_eq!(stats.len(), 3);
        assert_eq!(stats[0].id, "local:b");
        assert_eq!(stats[0].count, 3);
        assert_eq!(stats[1].id, "local:a");
        assert_eq!(stats[1].count, 1);
        assert_eq!(stats[2].id, "local:c");
        assert_eq!(stats[2].count, 0);
    }

    #[test]
    fn insert_resource_preserves_usage_on_conflict() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        let source = crate::core::resource::Source::Local {
            path: PathBuf::from("/tmp"),
        };
        let res = Resource {
            id: "local:x".into(),
            name: "x".into(),
            kind: ResourceKind::Skill,
            description: "v1".into(),
            directory: PathBuf::from("/tmp"),
            source: source.clone(),
            installed_at: 0,
            enabled: std::collections::HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: None,
        };
        db.insert_resource(&res).unwrap();

        // Record usage
        db.record_usage("local:x").unwrap();
        db.record_usage("local:x").unwrap();

        // Re-insert with updated description (simulates re-scan)
        let res2 = Resource {
            id: "local:x".into(),
            name: "x".into(),
            kind: ResourceKind::Skill,
            description: "v2".into(),
            directory: PathBuf::from("/tmp/new"),
            source,
            installed_at: 0,
            enabled: std::collections::HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: None,
        };
        db.insert_resource(&res2).unwrap();

        // Usage should be preserved, description should be updated
        let loaded = db.get_resource("local:x").unwrap().unwrap();
        assert_eq!(
            loaded.usage_count, 2,
            "usage_count should survive re-insert"
        );
        assert!(
            loaded.last_used_at.is_some(),
            "last_used_at should survive re-insert"
        );
        assert_eq!(loaded.description, "v2", "description should be updated");
    }

    #[test]
    fn trash_entries_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        let entry = TrashEntry {
            id: "trash:1".into(),
            resource_id: "local:foo".into(),
            name: "foo".into(),
            kind: ResourceKind::Skill,
            description: "desc".into(),
            directory: PathBuf::from("/tmp/foo"),
            source: Source::Local {
                path: PathBuf::from("/tmp/foo"),
            },
            installed_at: 1,
            usage_count: 3,
            last_used_at: Some(4),
            owner_user_id: None,
            deleted_at: 2,
            payload_path: Some(PathBuf::from("/tmp/trash/foo")),
            enabled_targets: vec![CliTarget::Claude, CliTarget::Codex],
            group_ids: vec!["grp".into()],
            mcp_configs: HashMap::new(),
            disabled_backup: None,
        };

        db.insert_trash_entry(&entry).unwrap();

        let listed = db.list_trash_entries().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "trash:1");
        assert_eq!(listed[0].enabled_targets.len(), 2);

        let loaded = db.get_trash_entry("trash:1").unwrap().unwrap();
        assert_eq!(loaded.name, "foo");
        assert_eq!(loaded.group_ids, vec!["grp".to_string()]);

        db.delete_trash_entry("trash:1").unwrap();
        assert!(db.get_trash_entry("trash:1").unwrap().is_none());
    }

    #[test]
    fn migration_preserves_group_members() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");

        // Create old schema with FK (disable FK enforcement to insert mcp: row without resources entry)
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "PRAGMA foreign_keys = OFF;
                 CREATE TABLE resources (id TEXT PRIMARY KEY, name TEXT, kind TEXT, description TEXT, directory TEXT, source_type TEXT, source_meta TEXT, installed_at INTEGER);
                 CREATE TABLE group_members (group_id TEXT, resource_id TEXT, PRIMARY KEY(group_id, resource_id), FOREIGN KEY(resource_id) REFERENCES resources(id));
                 INSERT INTO resources VALUES ('local:foo','foo','skill','','/tmp','local','{}',0);
                 INSERT INTO group_members VALUES ('grp1','local:foo');
                 INSERT INTO group_members VALUES ('grp1','mcp:bar');"
            ).unwrap();
        }

        // Open with migration
        let db = Database::open(&db_path).unwrap();
        let ids = db.get_group_member_ids("grp1").unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"local:foo".to_string()));
        assert!(ids.contains(&"mcp:bar".to_string()));
    }

    // -------- v15 multi-user tests --------

    #[test]
    fn schema_at_v15_after_open() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("v15.db")).unwrap();
        let version: i64 = db
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 15);

        // Tables must exist
        for tbl in &["users", "user_skill_library"] {
            let count: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![tbl],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {} missing", tbl);
        }

        // resources.owner_user_id must exist
        let owner_col: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('resources') WHERE name='owner_user_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(owner_col, 1);

        // router_events.user_id must exist
        let user_id_col: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('router_events') WHERE name='user_id'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(user_id_col, 1);
    }

    #[test]
    fn user_crud_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("users.db")).unwrap();

        db.create_user("u1", "alice", "phash1", "akhash1", false)
            .unwrap();

        // Look up by username
        let u = db.find_user_by_username("alice").unwrap().unwrap();
        assert_eq!(u.user_id, "u1");
        assert_eq!(u.username, "alice");
        assert!(!u.is_admin);
        assert!(!u.disabled);

        // Look up by api_key_hash
        let u2 = db.find_user_by_api_key_hash("akhash1").unwrap().unwrap();
        assert_eq!(u2.user_id, "u1");

        // Username uniqueness enforced
        let dup = db.create_user("u2", "alice", "phash2", "akhash2", false);
        assert!(dup.is_err(), "duplicate username must fail");

        // Admin promotion
        db.set_user_admin("u1", true).unwrap();
        let promoted = db.find_user_by_id("u1").unwrap().unwrap();
        assert!(promoted.is_admin);

        // Disable
        db.set_user_disabled("u1", true).unwrap();
        let disabled = db.find_user_by_id("u1").unwrap().unwrap();
        assert!(disabled.disabled);

        // Prefs update
        db.update_user_prefs("u1", r#"{"allow_public_recommend":true}"#)
            .unwrap();
        let with_prefs = db.find_user_by_id("u1").unwrap().unwrap();
        assert!(with_prefs.prefs_json.contains("allow_public_recommend"));

        // Rotate api key
        db.rotate_api_key("u1", "akhash1_new").unwrap();
        assert!(db.find_user_by_api_key_hash("akhash1").unwrap().is_none());
        assert!(db.find_user_by_api_key_hash("akhash1_new").unwrap().is_some());

        // List
        db.create_user("u2", "bob", "phash2", "akhash2", false)
            .unwrap();
        let list = db.list_users().unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn library_crud_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("lib.db")).unwrap();

        db.create_user("u1", "alice", "p", "k", false).unwrap();

        // Empty by default
        assert_eq!(db.library_count("u1").unwrap(), 0);
        assert!(!db.library_contains("u1", "bolder").unwrap());

        // Add
        db.library_add("u1", "bolder").unwrap();
        db.library_add("u1", "delight").unwrap();
        assert_eq!(db.library_count("u1").unwrap(), 2);
        assert!(db.library_contains("u1", "bolder").unwrap());

        // Idempotent add (INSERT OR IGNORE)
        db.library_add("u1", "bolder").unwrap();
        assert_eq!(db.library_count("u1").unwrap(), 2);

        // Remove one
        db.library_remove("u1", "bolder").unwrap();
        assert!(!db.library_contains("u1", "bolder").unwrap());
        assert_eq!(db.library_count("u1").unwrap(), 1);

        // List in DESC order of added_at
        let list = db.library_list("u1").unwrap();
        assert_eq!(list, vec!["delight"]);

        // Clear
        let cleared = db.library_clear("u1").unwrap();
        assert_eq!(cleared, 1);
        assert_eq!(db.library_count("u1").unwrap(), 0);

        // Per-user isolation
        db.create_user("u2", "bob", "p", "k2", false).unwrap();
        db.library_add("u1", "bolder").unwrap();
        db.library_add("u2", "overdrive").unwrap();
        assert_eq!(db.library_list("u1").unwrap(), vec!["bolder"]);
        assert_eq!(db.library_list("u2").unwrap(), vec!["overdrive"]);
    }

    // =========================================================================
    //  Phase B: owner_user_id write-path + per-user query helpers
    // =========================================================================

    fn mk_skill(id: &str, name: &str, owner: Option<&str>) -> Resource {
        Resource {
            id: id.into(),
            name: name.into(),
            kind: ResourceKind::Skill,
            description: "x".into(),
            directory: PathBuf::from("/tmp"),
            source: crate::core::resource::Source::Local {
                path: PathBuf::from("/tmp"),
            },
            installed_at: 0,
            enabled: HashMap::new(),
            usage_count: 0,
            last_used_at: None,
            owner_user_id: owner.map(String::from),
        }
    }

    #[test]
    fn insert_resource_persists_owner_user_id() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        db.insert_resource(&mk_skill("local:pub", "pub", None)).unwrap();
        db.insert_resource(&mk_skill("u:usr_alice:local:priv", "priv", Some("usr_alice")))
            .unwrap();

        let pub_row = db.get_resource("local:pub").unwrap().unwrap();
        let priv_row = db.get_resource("u:usr_alice:local:priv").unwrap().unwrap();
        assert_eq!(pub_row.owner_user_id, None, "public row stays NULL");
        assert_eq!(
            priv_row.owner_user_id.as_deref(),
            Some("usr_alice"),
            "private row carries owner"
        );
    }

    #[test]
    fn list_resources_for_user_filters_by_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        db.insert_resource(&mk_skill("local:pubA", "pubA", None)).unwrap();
        db.insert_resource(&mk_skill("local:pubB", "pubB", None)).unwrap();
        db.insert_resource(&mk_skill(
            "u:usr_alice:local:apriv",
            "apriv",
            Some("usr_alice"),
        ))
        .unwrap();
        db.insert_resource(&mk_skill(
            "u:usr_bob:local:bpriv",
            "bpriv",
            Some("usr_bob"),
        ))
        .unwrap();

        // owner = None: public only.
        let public_only =
            db.list_resources_for_user(Some(ResourceKind::Skill), None).unwrap();
        let names: Vec<_> = public_only.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["pubA", "pubB"]);

        // owner = Some(alice): public ∪ alice's private.
        let alice = db
            .list_resources_for_user(Some(ResourceKind::Skill), Some("usr_alice"))
            .unwrap();
        let names: Vec<_> = alice.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["apriv", "pubA", "pubB"]);

        // owner = Some(bob): public ∪ bob's private, NOT alice's.
        let bob = db
            .list_resources_for_user(Some(ResourceKind::Skill), Some("usr_bob"))
            .unwrap();
        let names: Vec<_> = bob.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["bpriv", "pubA", "pubB"]);
        assert!(bob.iter().all(|r| r.name != "apriv"));

        // owner = Some("*"): admin sees everything across all owners.
        let admin = db
            .list_resources_for_user(Some(ResourceKind::Skill), Some("*"))
            .unwrap();
        let names: Vec<_> = admin.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["apriv", "bpriv", "pubA", "pubB"]);
    }

    #[test]
    fn same_name_private_skills_coexist_across_users() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        // Same `name` (foo), different ids that include the owner prefix.
        db.insert_resource(&mk_skill(
            &Resource::generate_id(
                &crate::core::resource::Source::Local {
                    path: PathBuf::from("/tmp"),
                },
                "foo",
                Some("usr_alice"),
            ),
            "foo",
            Some("usr_alice"),
        ))
        .unwrap();
        db.insert_resource(&mk_skill(
            &Resource::generate_id(
                &crate::core::resource::Source::Local {
                    path: PathBuf::from("/tmp"),
                },
                "foo",
                Some("usr_bob"),
            ),
            "foo",
            Some("usr_bob"),
        ))
        .unwrap();

        // Each owner sees only their own foo (no public version exists).
        let alice = db
            .list_resources_for_user(Some(ResourceKind::Skill), Some("usr_alice"))
            .unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].name, "foo");
        assert_eq!(alice[0].owner_user_id.as_deref(), Some("usr_alice"));

        let bob = db
            .list_resources_for_user(Some(ResourceKind::Skill), Some("usr_bob"))
            .unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].owner_user_id.as_deref(), Some("usr_bob"));

        // Public scope sees neither (both are private).
        let public_only = db
            .list_resources_for_user(Some(ResourceKind::Skill), None)
            .unwrap();
        assert!(public_only.is_empty(), "no public foo exists");
    }

    #[test]
    fn find_by_name_for_user_private_wins_over_public() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        // Public foo, and alice's private foo. Alice sees the private one.
        db.insert_resource(&mk_skill("local:foo", "foo", None)).unwrap();
        db.insert_resource(&mk_skill(
            "u:usr_alice:local:foo",
            "foo",
            Some("usr_alice"),
        ))
        .unwrap();

        // Anonymous lookup → public.
        let pub_hit = db
            .find_resource_by_name_for_user(ResourceKind::Skill, "foo", None)
            .unwrap()
            .unwrap();
        assert_eq!(pub_hit.owner_user_id, None);

        // Alice's lookup → her private one (shadows public).
        let alice_hit = db
            .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some("usr_alice"))
            .unwrap()
            .unwrap();
        assert_eq!(alice_hit.owner_user_id.as_deref(), Some("usr_alice"));

        // Bob's lookup → public foo (he has no private one).
        let bob_hit = db
            .find_resource_by_name_for_user(ResourceKind::Skill, "foo", Some("usr_bob"))
            .unwrap()
            .unwrap();
        assert_eq!(bob_hit.owner_user_id, None);

        // Unknown name → None.
        assert!(
            db.find_resource_by_name_for_user(ResourceKind::Skill, "nope", Some("usr_alice"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn generate_id_distinguishes_public_from_private() {
        let src = crate::core::resource::Source::Local {
            path: PathBuf::from("/tmp"),
        };
        let pub_id = Resource::generate_id(&src, "foo", None);
        let alice_id = Resource::generate_id(&src, "foo", Some("usr_alice"));
        let bob_id = Resource::generate_id(&src, "foo", Some("usr_bob"));

        assert_eq!(pub_id, "local:foo", "public id is back-compat with pre-v15");
        assert_eq!(alice_id, "u:usr_alice:local:foo");
        assert_eq!(bob_id, "u:usr_bob:local:foo");
        assert_ne!(alice_id, bob_id);
        assert_ne!(alice_id, pub_id);
    }

    #[test]
    fn trash_entry_owner_user_id_survives_serde_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();

        let entry = TrashEntry {
            id: "trash:priv".into(),
            resource_id: "u:usr_alice:local:secret".into(),
            name: "secret".into(),
            kind: ResourceKind::Skill,
            description: "alice's".into(),
            directory: PathBuf::from("/tmp/secret"),
            source: crate::core::resource::Source::Local {
                path: PathBuf::from("/tmp/secret"),
            },
            installed_at: 1,
            usage_count: 0,
            last_used_at: None,
            owner_user_id: Some("usr_alice".into()),
            deleted_at: 2,
            payload_path: None,
            enabled_targets: vec![],
            group_ids: vec![],
            mcp_configs: HashMap::new(),
            disabled_backup: None,
        };
        db.insert_trash_entry(&entry).unwrap();

        let loaded = db.get_trash_entry("trash:priv").unwrap().unwrap();
        assert_eq!(loaded.owner_user_id.as_deref(), Some("usr_alice"));
    }

    #[test]
    fn pre_v15_trash_payload_decodes_with_owner_default_none() {
        // Older payload_json blobs predate the owner_user_id field. They
        // must still decode (serde(default)) and surface as public-pool
        // trash so the restore path treats them as public.
        let pre_v15_json = r#"{
            "id": "trash:legacy",
            "resource_id": "local:legacy",
            "name": "legacy",
            "kind": "skill",
            "description": "",
            "directory": "/tmp/legacy",
            "source": { "type": "local", "path": "/tmp/legacy" },
            "installed_at": 0,
            "usage_count": 0,
            "last_used_at": null,
            "deleted_at": 0,
            "payload_path": null
        }"#;
        let entry: TrashEntry = serde_json::from_str(pre_v15_json).unwrap();
        assert_eq!(entry.owner_user_id, None);
        assert_eq!(entry.id, "trash:legacy");
    }
}

//! Schema creation + ALL migrations, monolithic.
//!
//! INVARIANT: keep `init_schema` and every versioned migration in this one
//! file. Migrations run on every `open()` with no version lock; splitting them
//! across files risks a half-applied schema. Do not factor per-version files.

use super::Database;
use anyhow::Result;

impl Database {
    pub(super) fn init_schema(&self) -> Result<()> {
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

        if version < 16 {
            // PLANNING.md §1.4 — community market.
            // Tracks user-uploaded skills available for cross-user discovery.
            // Physical payload lives at `<data>/community/<uploader_uid>/<name>/`
            // (a normal directory tree, NOT a gz archive — the download
            // endpoint re-tars on demand). PK is `(uploader_uid, name)` so
            // the same name can coexist across uploaders. `version` is a
            // monotonic bump applied on every re-upload by the same uploader.
            // `installs_total` counts how many times any user has called
            // POST /api/community/install/<uid>/<name>.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS community_skills (
                    uploader_uid TEXT NOT NULL,
                    name TEXT NOT NULL,
                    version TEXT NOT NULL,
                    installs_total INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    PRIMARY KEY(uploader_uid, name)
                 );
                 CREATE INDEX IF NOT EXISTS idx_community_skills_name
                   ON community_skills(name);
                 CREATE INDEX IF NOT EXISTS idx_community_skills_uploader
                   ON community_skills(uploader_uid);

                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (16);",
            )?;
        }

        if version < 20 {
            // PLANNING §1.4 rewrite — private upload → enrich → publish-request
            // → admin approve workflow. publish_status tracks the state for
            // private skills (owner_user_id IS NOT NULL):
            //   - 'draft'    fresh upload, hasn't been submitted yet
            //   - 'pending'  user called publish-request, awaiting admin
            //   - 'approved' admin signed off + copied to community pool
            //   - 'rejected' admin declined, reason stored in publish_reason
            //
            // Public-pool rows (owner_user_id IS NULL) are stamped 'draft'
            // and ignored by the publish workflow.
            //
            // Version jump 16 → 20 covers DBs that previously rode an
            // ahead-of-schema binary (e.g. a feature branch that wrote
            // schema_version=19 with enrich_in_flight / enrich_started_at
            // / enrich_last_error columns) — the gap doesn't break us
            // because those rogue columns are orthogonal to publish_status.
            // ALTER will fail if a stray run of this exact migration
            // already added the columns, so we tolerate the per-statement
            // error and only trust the index + version bump as the
            // canonical "we ran" marker.
            let _ = self.conn.execute(
                "ALTER TABLE resources ADD COLUMN publish_status TEXT NOT NULL DEFAULT 'draft'",
                [],
            );
            let _ = self
                .conn
                .execute("ALTER TABLE resources ADD COLUMN publish_reason TEXT", []);
            self.conn.execute_batch(
                "CREATE INDEX IF NOT EXISTS idx_resources_publish_status
                   ON resources(publish_status);

                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (20);",
            )?;
        }

        if version < 21 {
            // Structured recommend index:
            // - owner_user_id: '' for public-pool summary, uid for a user's
            //   private same-named skill
            // - summary: user-facing / publish gate summary
            // - search_doc: BM25 retriever text
            // - router_card: short router-facing candidate card
            // - source_hash: content freshness (name + description + SKILL.md)
            // - prompt_hash: layout freshness (summary_lang + output layout)
            // - format_key: human-readable layout signature for debugging
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS resource_ai_summary_v21 (
                    owner_user_id TEXT NOT NULL DEFAULT '',
                    name TEXT NOT NULL,
                    summary TEXT NOT NULL,
                    updated_at INTEGER NOT NULL,
                    llm_score INTEGER NOT NULL DEFAULT 5,
                    search_doc TEXT NOT NULL DEFAULT '',
                    router_card TEXT NOT NULL DEFAULT '',
                    source_hash TEXT NOT NULL DEFAULT '',
                    prompt_hash TEXT NOT NULL DEFAULT '',
                    format_key TEXT NOT NULL DEFAULT '',
                    PRIMARY KEY (owner_user_id, name)
                 );
                 INSERT OR REPLACE INTO resource_ai_summary_v21 (
                    owner_user_id, name, summary, updated_at, llm_score
                 )
                 SELECT '', name, summary, updated_at, llm_score
                   FROM resource_ai_summary;
                 DROP TABLE resource_ai_summary;
                 ALTER TABLE resource_ai_summary_v21 RENAME TO resource_ai_summary;
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (21);",
            )?;
        }

        if version < 22 {
            // Issue #35 — browser sessions decouple from the api_key.
            // Dashboard login mints an independent `rnai_sess_...` token
            // (hash stored here) instead of rotating the api_key, so a web
            // login no longer revokes every installed hook client's
            // ~/.runai-identity. NULL = no active browser session.
            // Tolerate the ALTER failing in case a stray run already added
            // the column; the version bump is the canonical marker.
            let _ = self
                .conn
                .execute("ALTER TABLE users ADD COLUMN session_key_hash TEXT", []);
            self.conn.execute_batch(
                "DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (22);",
            )?;
        }

        if version < 23 {
            // Activation/feedback idempotency (PLANNING §1.3 protocol):
            // every `POST /skills/use/{name}` and `POST /feedback` carries
            // a client-generated `X-Runai-Event-Id`. The first request with
            // a given id applies the side effect (usage_count bump, session
            // adoption, feedback reevaluation); a replay with the SAME
            // payload hash is a 200 no-op; a replay with a DIFFERENT payload
            // hash is a 409 conflict. This table is the durable idempotency
            // store — surviving server restarts, not in-memory — so a
            // client retrying after a network blip can never double-count.
            //
            // `kind` distinguishes usage vs feedback events so the same
            // event_id namespace can serve both without collision (a usage
            // event_id and a feedback event_id are independent spaces).
            // `payload_hash` is the sha256 of the canonical JSON body
            // (sorted keys) so field-order drift does not manufacture a
            // false conflict.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS usage_events (
                    event_id TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK (kind IN ('usage', 'feedback')),
                    skill_name TEXT NOT NULL,
                    payload_hash TEXT NOT NULL,
                    session_id TEXT NOT NULL DEFAULT '',
                    user_id TEXT,
                    ts INTEGER NOT NULL,
                    PRIMARY KEY (event_id, kind)
                 );
                 CREATE INDEX IF NOT EXISTS idx_usage_events_skill
                   ON usage_events(skill_name);
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (23);",
            )?;
        }

        if version < 24 {
            // Per-session bounded intent memory for the recommend router.
            // Rows are scoped by (session_id, user_id, client_kind) so Pi,
            // Codex, Claude, and other hosts do not leak short-memory hints
            // into one another. The router keeps only the newest configured
            // N rows per scope; no raw transcript history is stored here.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS router_intent_memory (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts INTEGER NOT NULL,
                    session_id TEXT NOT NULL,
                    user_id TEXT NOT NULL DEFAULT '',
                    client_kind TEXT NOT NULL DEFAULT '',
                    memory TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_router_intent_memory_scope
                   ON router_intent_memory(session_id, user_id, client_kind, id);
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (24);",
            )?;
        }

        if version < 25 {
            // Two-stage recommend telemetry. Stage 1 asks the same recommend
            // model to compress the user's current turn into a compact BM25
            // intent artifact; Stage 2 routes over the retrieved candidates.
            // Keep both prompts/outputs visible in router_events so the
            // dashboard can show the two waves separately.
            let has_router_events: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='router_events'",
                [],
                |r| r.get(0),
            )?;
            if has_router_events > 0 {
                self.conn.execute_batch(
                    "ALTER TABLE router_events ADD COLUMN intent_llm_input TEXT NOT NULL DEFAULT '';
                     ALTER TABLE router_events ADD COLUMN intent_llm_output TEXT NOT NULL DEFAULT '';
                     ALTER TABLE router_events ADD COLUMN intent_status TEXT NOT NULL DEFAULT '';
                     ALTER TABLE router_events ADD COLUMN intent_error_msg TEXT;
                     ALTER TABLE router_events ADD COLUMN bm25_candidates_json TEXT NOT NULL DEFAULT '[]';",
                )?;
            }
            self.conn.execute_batch(
                "DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (25);",
            )?;
        }

        if version < 26 {
            // Skill feedback radar: event-sourced ±1 verdicts on individual
            // skills. Rows are append-only (never updated in place) so the
            // full history survives for `recent_skill_feedback` and
            // aggregate counts are always a fresh COUNT over the log.
            // `owner_user_id` follows the same owner-pool convention as
            // `resources.owner_user_id` (NULL = public pool, uid = that
            // user's private skill); `user_id` / `session_id` / `event_id`
            // are all optional so unauthenticated or session-less feedback
            // still records. `event_id` loosely references the
            // `router_events` row that produced the judged recommendation,
            // when known — no FK constraint, mirroring `trash_entries`.
            self.conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS skill_feedback (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ts INTEGER NOT NULL,
                    skill_name TEXT NOT NULL,
                    owner_user_id TEXT,
                    user_id TEXT,
                    session_id TEXT,
                    event_id INTEGER,
                    verdict INTEGER NOT NULL,
                    note TEXT
                 );
                 CREATE INDEX IF NOT EXISTS idx_skill_feedback_name ON skill_feedback(skill_name);
                 DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (26);",
            )?;
        }

        if version < 27 {
            // Covering indexes for the /api/summary aggregations (17s cold-read
            // fix). `router_events` rows are very wide (llm_input caps at 64 KB,
            // hook_output/llm_raw_response several KB each), so a full-table
            // SCAN for COUNT/SUM(total_tokens)/AVG(latency_ms)/GROUP BY model
            // faults in the entire ~260 MB table via random reads — ~17s cold
            // on a real install, 0s once the OS page cache is warm.
            //
            // Two narrow covering indexes let those aggregations run as
            // index-only scans (each ~400 KB vs the 260 MB table):
            //  - idx_router_events_summary_cover: the main COUNT/SUM aggregation
            //    and the ok-only AVG(latency_ms) become COVERING INDEX scans.
            //  - idx_router_events_permodel_cover: the GROUP BY model query
            //    (which also computes hits from chosen_skills_json + AVG
            //    latency) becomes a COVERING INDEX scan; `model` leads so the
            //    GROUP BY needs no separate sort of the table, and the narrow
            //    chosen_skills_json (a short JSON array of skill names) is
            //    included so the hits CASE never touches the wide rows.
            // CREATE INDEX IF NOT EXISTS is idempotent.
            let has_router_events: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='router_events'",
                [],
                |r| r.get(0),
            )?;
            if has_router_events > 0 {
                self.conn.execute_batch(
                    "CREATE INDEX IF NOT EXISTS idx_router_events_summary_cover
                        ON router_events(ts, user_id, status, model, latency_ms,
                                         total_tokens, prompt_tokens, completion_tokens, reasoning_tokens);
                     CREATE INDEX IF NOT EXISTS idx_router_events_permodel_cover
                        ON router_events(model, ts, user_id, total_tokens, latency_ms, chosen_skills_json);",
                )?;
            }
            self.conn.execute_batch(
                "DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (27);",
            )?;
        }

        if version < 28 {
            let has_router_events: i64 = self.conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='router_events'",
                [],
                |r| r.get(0),
            )?;
            if has_router_events > 0 {
                self.conn.execute_batch(
                    "ALTER TABLE router_events ADD COLUMN routing_mode TEXT NOT NULL DEFAULT '';
                     ALTER TABLE router_events ADD COLUMN empty_reason TEXT NOT NULL DEFAULT '';
                     ALTER TABLE router_events ADD COLUMN retrieval_query TEXT NOT NULL DEFAULT '';
                     ALTER TABLE router_events ADD COLUMN parsed_candidates_json TEXT NOT NULL DEFAULT '[]';
                     ALTER TABLE router_events ADD COLUMN filtered_candidates_json TEXT NOT NULL DEFAULT '[]';
                     ALTER TABLE router_events ADD COLUMN parser_recovery INTEGER NOT NULL DEFAULT 0;
                     ALTER TABLE router_events ADD COLUMN llm_call_count INTEGER NOT NULL DEFAULT 0;",
                )?;
            }
            self.conn.execute_batch(
                "DELETE FROM schema_version;
                 INSERT INTO schema_version VALUES (28);",
            )?;
        }

        Ok(())
    }
}

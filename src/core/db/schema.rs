//! Schema creation + ALL migrations (v1–v16), monolithic.
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

        Ok(())
    }
}

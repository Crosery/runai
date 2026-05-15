use anyhow::Result;
use rusqlite::{Connection, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::core::cli_target::CliTarget;
use crate::core::resource::{Resource, ResourceKind, Source, TrashEntry, UsageStat};

#[derive(Debug, Clone)]
pub struct RouterEvent {
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
}

#[derive(Debug, Clone)]
pub struct RouterModelStat {
    pub model: String,
    pub calls: i64,
    pub total_tokens: i64,
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

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
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

        Ok(())
    }

    pub fn insert_router_event(&self, ev: &RouterEvent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO router_events (
                ts, provider, model,
                prompt_tokens, completion_tokens, reasoning_tokens, total_tokens,
                cache_hit_tokens, cache_miss_tokens,
                latency_ms, chosen_skills_json, candidate_count, status, error_msg
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
            ],
        )?;
        Ok(())
    }

    pub fn router_stats_summary(&self, since_ts: Option<i64>) -> Result<RouterStatsSummary> {
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
             WHERE (?1 IS NULL OR ts >= ?1)",
            params![since_ts],
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
            "SELECT AVG(latency_ms) FROM router_events WHERE (?1 IS NULL OR ts >= ?1) AND status = 'ok'",
            params![since_ts],
            |r| r.get(0),
        ).ok().flatten();
        let mut per_model = Vec::new();
        let mut stmt = self.conn.prepare(
            "SELECT model, COUNT(*), COALESCE(SUM(total_tokens), 0)
             FROM router_events
             WHERE (?1 IS NULL OR ts >= ?1)
             GROUP BY model
             ORDER BY 3 DESC",
        )?;
        let rows = stmt.query_map(params![since_ts], |r| {
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
        let mut stmt = self.conn.prepare(
            "SELECT ts, provider, model, prompt_tokens, completion_tokens, reasoning_tokens,
                    total_tokens, cache_hit_tokens, cache_miss_tokens, latency_ms,
                    chosen_skills_json, candidate_count, status, error_msg
             FROM router_events
             ORDER BY ts DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(RouterEvent {
                ts: r.get(0)?,
                provider: r.get(1)?,
                model: r.get(2)?,
                prompt_tokens: r.get(3)?,
                completion_tokens: r.get(4)?,
                reasoning_tokens: r.get(5)?,
                total_tokens: r.get(6)?,
                cache_hit_tokens: r.get(7)?,
                cache_miss_tokens: r.get(8)?,
                latency_ms: r.get(9)?,
                chosen_skills_json: r.get(10)?,
                candidate_count: r.get(11)?,
                status: r.get(12)?,
                error_msg: r.get(13)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn insert_resource(&self, res: &Resource) -> Result<()> {
        self.conn.execute(
            "INSERT INTO resources (id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                description = excluded.description,
                directory = excluded.directory,
                source_type = excluded.source_type,
                source_meta = excluded.source_meta",
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
            "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at
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
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at
                     FROM resources WHERE kind = ?1 ORDER BY name"
                )?;
                self.collect_resources(&mut stmt, params![k.as_str()])?
            }
            None => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at, usage_count, last_used_at
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
            "SELECT r.id, r.name, r.kind, r.description, r.directory, r.source_type, r.source_meta, r.installed_at, r.usage_count, r.last_used_at
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
        assert_eq!(version, 5);
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
}

//! `resources` table CRUD: insert/get/list, owner-aware queries, usage
//! counters, dedupe, and counts.
//!
//! INVARIANT: `collect_resources` reads columns positionally. Every SELECT in
//! this file (and in `groups.rs`, which reuses `collect_resources`) lists the
//! columns in the exact order:
//! id, name, kind, description, directory, source_type, source_meta,
//! installed_at, usage_count, last_used_at, owner_user_id.

use super::Database;
use crate::core::cli_target::CliTarget;
use crate::core::resource::{Resource, ResourceKind, Source, UsageStat};
use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;
use std::path::PathBuf;

impl Database {
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

    pub(super) fn collect_resources(
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

    pub fn resource_count(&self) -> Result<(usize, usize)> {
        let skills: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM resources WHERE kind = 'skill'",
            [],
            |r| r.get(0),
        )?;
        Ok((skills as usize, 0))
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

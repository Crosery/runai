use std::collections::HashMap;
use std::path::{Path, PathBuf};
use anyhow::Result;
use rusqlite::{Connection, params};

use crate::core::resource::{Resource, ResourceKind, Source};
use crate::core::cli_target::CliTarget;

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
            );"
        )?;

        // Schema versioning
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);"
        )?;

        let version: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0)
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

        Ok(())
    }

    pub fn insert_resource(&self, res: &Resource) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO resources (id, name, kind, description, directory, source_type, source_meta, installed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                res.id,
                res.name,
                res.kind.as_str(),
                res.description,
                res.directory.to_string_lossy().to_string(),
                res.source.source_type(),
                res.source.to_meta_json(),
                res.installed_at,
            ],
        )?;

        for (target, enabled) in &res.enabled {
            self.set_target_enabled(&res.id, *target, *enabled)?;
        }
        Ok(())
    }

    pub fn get_resource(&self, id: &str) -> Result<Option<Resource>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at
             FROM resources WHERE id = ?1"
        )?;

        let mut rows = stmt.query(params![id])?;
        let row = match rows.next()? {
            Some(r) => r,
            None => return Ok(None),
        };

        let id: String = row.get(0)?;
        let kind_str: String = row.get(2)?;
        let source_type: String = row.get(5)?;
        let source_meta: String = row.get::<_, Option<String>>(6)?.unwrap_or_default();
        let enabled = self.get_targets_for_resource(&id)?;

        Ok(Some(Resource {
            id: id.clone(),
            name: row.get(1)?,
            kind: ResourceKind::from_str(&kind_str).unwrap_or(ResourceKind::Skill),
            description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            directory: PathBuf::from(row.get::<_, String>(4)?),
            source: Source::from_meta_json(&source_type, &source_meta)
                .unwrap_or(Source::Local { path: PathBuf::new() }),
            installed_at: row.get(7)?,
            enabled,
        }))
    }

    pub fn list_resources(
        &self,
        kind: Option<ResourceKind>,
        enabled_for: Option<CliTarget>,
    ) -> Result<Vec<Resource>> {
        let mut resources = match (kind, enabled_for) {
            (Some(k), Some(t)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT r.id, r.name, r.kind, r.description, r.directory, r.source_type, r.source_meta, r.installed_at
                     FROM resources r JOIN resource_targets rt ON r.id = rt.resource_id
                     WHERE r.kind = ?1 AND rt.cli_target = ?2 AND rt.enabled = 1 ORDER BY r.name"
                )?;
                self.collect_resources(&mut stmt, params![k.as_str(), t.name()])?
            }
            (Some(k), None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at
                     FROM resources WHERE kind = ?1 ORDER BY name"
                )?;
                self.collect_resources(&mut stmt, params![k.as_str()])?
            }
            (None, Some(t)) => {
                let mut stmt = self.conn.prepare(
                    "SELECT r.id, r.name, r.kind, r.description, r.directory, r.source_type, r.source_meta, r.installed_at
                     FROM resources r JOIN resource_targets rt ON r.id = rt.resource_id
                     WHERE rt.cli_target = ?1 AND rt.enabled = 1 ORDER BY r.name"
                )?;
                self.collect_resources(&mut stmt, params![t.name()])?
            }
            (None, None) => {
                let mut stmt = self.conn.prepare(
                    "SELECT id, name, kind, description, directory, source_type, source_meta, installed_at
                     FROM resources ORDER BY name"
                )?;
                self.collect_resources(&mut stmt, params![])?
            }
        };

        for res in &mut resources {
            res.enabled = self.get_targets_for_resource(&res.id)?;
        }
        Ok(resources)
    }

    fn collect_resources(&self, stmt: &mut rusqlite::Statement, params: impl rusqlite::Params) -> Result<Vec<Resource>> {
        let rows = stmt.query_map(params, |row| {
            let kind_str: String = row.get(2)?;
            let source_type: String = row.get(5)?;
            let source_meta: String = row.get::<_, Option<String>>(6)?.unwrap_or_default();

            Ok(Resource {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: ResourceKind::from_str(&kind_str).unwrap_or(ResourceKind::Skill),
                description: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                directory: PathBuf::from(row.get::<_, String>(4)?),
                source: Source::from_meta_json(&source_type, &source_meta)
                    .unwrap_or(Source::Local { path: PathBuf::new() }),
                installed_at: row.get(7)?,
                enabled: HashMap::new(),
            })
        })?;

        let mut resources = Vec::new();
        for row in rows {
            resources.push(row?);
        }
        Ok(resources)
    }

    pub fn delete_resource(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM resources WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_target_enabled(&self, resource_id: &str, target: CliTarget, enabled: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO resource_targets (resource_id, cli_target, enabled)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (resource_id, cli_target)
             DO UPDATE SET enabled = ?3",
            params![resource_id, target.name(), enabled as i32],
        )?;
        Ok(())
    }

    fn get_targets_for_resource(&self, resource_id: &str) -> Result<HashMap<CliTarget, bool>> {
        let mut stmt = self.conn.prepare(
            "SELECT cli_target, enabled FROM resource_targets WHERE resource_id = ?1"
        )?;
        let rows = stmt.query_map(params![resource_id], |row| {
            let target_str: String = row.get(0)?;
            let enabled: bool = row.get(1)?;
            Ok((target_str, enabled))
        })?;

        let mut map = HashMap::new();
        for row in rows {
            let (target_str, enabled) = row?;
            if let Some(target) = CliTarget::from_str(&target_str) {
                map.insert(target, enabled);
            }
        }
        Ok(map)
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
            "SELECT r.id, r.name, r.kind, r.description, r.directory, r.source_type, r.source_meta, r.installed_at
             FROM resources r JOIN group_members gm ON r.id = gm.resource_id
             WHERE gm.group_id = ?1 ORDER BY r.name"
        )?;

        let mut resources = self.collect_resources(&mut stmt, params![group_id])?;
        for res in &mut resources {
            res.enabled = self.get_targets_for_resource(&res.id)?;
        }
        Ok(resources)
    }

    pub fn get_groups_for_resource(&self, resource_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT group_id FROM group_members WHERE resource_id = ?1"
        )?;
        let rows = stmt.query_map(params![resource_id], |row| row.get(0))?;
        let mut groups = Vec::new();
        for row in rows {
            groups.push(row?);
        }
        Ok(groups)
    }

    pub fn resource_count(&self) -> Result<(usize, usize)> {
        let skills: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM resources WHERE kind = 'skill'", [], |r| r.get(0)
        )?;
        let mcps: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM resources WHERE kind = 'mcp'", [], |r| r.get(0)
        )?;
        Ok((skills as usize, mcps as usize))
    }

    pub fn enabled_count(&self, target: CliTarget) -> Result<(usize, usize)> {
        let skills = self.enabled_skill_count(target)?;
        let mcps: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM resources r JOIN resource_targets rt ON r.id = rt.resource_id
             WHERE r.kind = 'mcp' AND rt.cli_target = ?1 AND rt.enabled = 1",
            params![target.name()], |r| r.get(0)
        )?;
        Ok((skills, mcps as usize))
    }

    /// Count only enabled skills for a target (MCP status is read from config files).
    pub fn schema_version(&self) -> i64 {
        self.conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version", [], |r| r.get(0)
        ).unwrap_or(0)
    }

    /// Get group member IDs without joining resources table.
    /// Returns raw resource_id strings like "local:foo" or "mcp:bar".
    pub fn get_group_member_ids(&self, group_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT resource_id FROM group_members WHERE group_id = ?1"
        )?;
        let rows = stmt.query_map(params![group_id], |row| row.get(0))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row?);
        }
        Ok(ids)
    }

    pub fn enabled_skill_count(&self, target: CliTarget) -> Result<usize> {
        let skills: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM resources r JOIN resource_targets rt ON r.id = rt.resource_id
             WHERE r.kind = 'skill' AND rt.cli_target = ?1 AND rt.enabled = 1",
            params![target.name()], |r| r.get(0)
        )?;
        Ok(skills as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_creates_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Database::open(&tmp.path().join("test.db")).unwrap();
        let version: i64 = db.conn.query_row(
            "SELECT version FROM schema_version", [], |r| r.get(0)
        ).unwrap();
        assert_eq!(version, 2);
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

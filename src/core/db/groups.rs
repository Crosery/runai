//! `group_members` associations: link resources to groups and query both ways.
//!
//! `get_group_members` reuses `Database::collect_resources` (defined in
//! `resources.rs`, `pub(super)`) so the resource SELECT here keeps the same
//! positional column order.

use super::Database;
use crate::core::resource::Resource;
use anyhow::Result;
use rusqlite::params;
use std::collections::HashMap;

impl Database {
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
}

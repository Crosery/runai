//! `trash_entries` CRUD (serde-JSON payloads) + the raw `delete_resource`
//! row-delete used by the trash-first delete flow.

use super::Database;
use crate::core::resource::TrashEntry;
use anyhow::Result;
use rusqlite::params;

impl Database {
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
}

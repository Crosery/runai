//! `community_skills` table CRUD (schema v16+): the team-mode social pool of
//! user-uploaded skills. PK is `(uploader_uid, name)` so the same name can
//! coexist across uploaders. Physical payload lives at
//! `<data>/community/<uploader_uid>/<name>/` as a normal directory tree; the
//! `/api/community/download` handler re-tars + gzips on demand.
//!
//! INVARIANT: `row_to_community_skill` reads columns positionally. Every
//! SELECT here lists them as: uploader_uid, name, version, installs_total,
//! created_at, updated_at.

use super::Database;
use anyhow::Result;
use rusqlite::params;

/// One row of the `community_skills` table — a skill uploaded by a user
/// to the community pool. PK is `(uploader_uid, name)`.
#[derive(Debug, Clone)]
pub struct CommunitySkill {
    pub uploader_uid: String,
    pub name: String,
    pub version: String,
    pub installs_total: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Database {
    /// Insert a fresh row. Errors on PK conflict — callers that want
    /// upsert semantics should use [`upsert_community_skill`] instead.
    pub fn insert_community_skill(
        &self,
        uploader_uid: &str,
        name: &str,
        version: &str,
    ) -> Result<()> {
        let ts = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO community_skills
                (uploader_uid, name, version, installs_total, created_at, updated_at)
             VALUES (?1, ?2, ?3, 0, ?4, ?4)",
            params![uploader_uid, name, version, ts],
        )?;
        Ok(())
    }

    /// Insert-or-replace by `(uploader_uid, name)`. Preserves `installs_total`
    /// and `created_at` on conflict; bumps `version` + `updated_at`. Used
    /// when the same uploader re-uploads a skill of the same name (the
    /// new version's `version` string should be monotonic, e.g.
    /// timestamp-derived, but the table itself doesn't enforce order).
    pub fn upsert_community_skill(
        &self,
        uploader_uid: &str,
        name: &str,
        version: &str,
    ) -> Result<()> {
        let ts = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO community_skills
                (uploader_uid, name, version, installs_total, created_at, updated_at)
             VALUES (?1, ?2, ?3, 0, ?4, ?4)
             ON CONFLICT(uploader_uid, name) DO UPDATE SET
                version = excluded.version,
                updated_at = excluded.updated_at",
            params![uploader_uid, name, version, ts],
        )?;
        Ok(())
    }

    pub fn get_community_skill(
        &self,
        uploader_uid: &str,
        name: &str,
    ) -> Result<Option<CommunitySkill>> {
        let mut stmt = self.conn.prepare(
            "SELECT uploader_uid, name, version, installs_total, created_at, updated_at
             FROM community_skills WHERE uploader_uid = ?1 AND name = ?2",
        )?;
        let row = stmt
            .query_row(params![uploader_uid, name], row_to_community_skill)
            .ok();
        Ok(row)
    }

    /// Sort orders accepted by `list_community_skills`.
    /// - `installs`: highest installs_total first, ties broken by `updated_at` desc.
    /// - `created`: newest `created_at` first.
    /// - `name`: alphabetical by `name`.
    pub fn list_community_skills(
        &self,
        sort: CommunitySort,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<CommunitySkill>> {
        let order = match sort {
            CommunitySort::Installs => "installs_total DESC, updated_at DESC",
            CommunitySort::Created => "created_at DESC",
            CommunitySort::Name => "name ASC, uploader_uid ASC",
        };
        let sql = format!(
            "SELECT uploader_uid, name, version, installs_total, created_at, updated_at
             FROM community_skills
             ORDER BY {order}
             LIMIT ?1 OFFSET ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], row_to_community_skill)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn count_community_skills(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM community_skills", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn increment_community_installs(&self, uploader_uid: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE community_skills
             SET installs_total = installs_total + 1
             WHERE uploader_uid = ?1 AND name = ?2",
            params![uploader_uid, name],
        )?;
        Ok(())
    }

    /// Delete a row. Returns true if a row was actually removed.
    pub fn delete_community_skill(&self, uploader_uid: &str, name: &str) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM community_skills WHERE uploader_uid = ?1 AND name = ?2",
            params![uploader_uid, name],
        )?;
        Ok(n > 0)
    }
}

/// Sort order for `list_community_skills`. Maps to the `?sort=` query string
/// on `/api/community/list`.
#[derive(Debug, Clone, Copy)]
pub enum CommunitySort {
    Installs,
    Created,
    Name,
}

impl CommunitySort {
    /// Parse the `?sort=` query parameter. Default: `Installs`.
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("").trim().to_ascii_lowercase().as_str() {
            "created" | "created_at" | "new" | "newest" => Self::Created,
            "name" | "alpha" | "alphabetical" => Self::Name,
            _ => Self::Installs,
        }
    }
}

fn row_to_community_skill(r: &rusqlite::Row<'_>) -> rusqlite::Result<CommunitySkill> {
    Ok(CommunitySkill {
        uploader_uid: r.get(0)?,
        name: r.get(1)?,
        version: r.get(2)?,
        installs_total: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

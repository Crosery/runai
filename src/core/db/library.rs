//! `user_skill_library`: a user's "favorites" subscription to public-pool
//! skills (resources.owner_user_id IS NULL). Private skills the user owns are
//! NOT tracked here — they're identified via owner_user_id.
//!
//! Also hosts `top_public_skills`, used to pre-fill a new user's library.

use super::Database;
use anyhow::Result;
use rusqlite::params;

impl Database {
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

    /// Sweep `user_skill_library` for subscriptions owned by users that no
    /// longer exist in `users`. Returns the row count removed. Deleting a user
    /// used to leave their whole library set behind (the delete path only did
    /// `library_clear` + `DELETE users` before the cascade landed, and older
    /// DBs predate the cascade entirely) — those rows are never read but inflate
    /// nothing-good, so sweep them at startup alongside the skill-orphan pass.
    pub fn cleanup_orphan_library_for_deleted_users(&self) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM user_skill_library
             WHERE user_id NOT IN (SELECT user_id FROM users)",
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

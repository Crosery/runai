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

    /// Drop every user's library reference to this `skill_name` — but ONLY
    /// when no PUBLIC-pool skill of that name still exists. Used by
    /// `trash_resource` / `delete_user_cascade` / `doctor --fix` after they
    /// `delete_resource`, so the deleted row is already gone by the time this
    /// runs.
    ///
    /// C4 (scan_findings.md): `user_skill_library` only ever tracks
    /// public-pool subscriptions (see the module doc). The owner-pool design
    /// lets a private skill share a name with an unrelated public one, so a
    /// name-only `DELETE` would wipe every OTHER user's subscription to the
    /// still-existing PUBLIC skill whenever ANY user trashed a private skill
    /// of that name. Gating on "no public row of this name remains" makes the
    /// call a no-op for a private trash (public row still there → subscribers
    /// kept) while still sweeping subscribers once the public skill itself is
    /// gone (its row was deleted first, so the guard passes).
    pub fn library_remove_for_all(&self, skill_name: &str) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM user_skill_library
             WHERE skill_name = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM resources
                   WHERE name = ?1 AND kind = 'skill' AND owner_user_id IS NULL
               )",
            params![skill_name],
        )?;
        Ok(n)
    }

    /// Sweep `user_skill_library` for entries pointing at PUBLIC-pool skills
    /// that no longer exist. Returns the row count removed. Run at startup so
    /// a database imported from an older release (pre-`library_remove_for_all`-
    /// on-trash) doesn't leave the dashboard with "我的库 N" rows that 404 on
    /// click.
    ///
    /// C4 (scan_findings.md): the "still exists" subquery is filtered to
    /// `owner_user_id IS NULL`. `user_skill_library` only tracks public-pool
    /// subscriptions, so a private skill of the same name must NOT keep an
    /// orphaned public subscription alive — without the filter, a subscriber's
    /// public `foo` being trashed while a different user's private `foo`
    /// survives left the genuinely-orphaned row un-swept (it kept 404ing on
    /// click), defeating the exact purpose of this sweep.
    pub fn cleanup_orphan_library_entries(&self) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM user_skill_library
             WHERE skill_name NOT IN (
                 SELECT name FROM resources
                 WHERE kind = 'skill' AND owner_user_id IS NULL
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

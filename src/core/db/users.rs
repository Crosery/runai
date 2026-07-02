//! `users` table CRUD (schema v15+): username / password / api_key auth and
//! per-user ownership of skills. owner_user_id NULL on resources = public pool.
//!
//! INVARIANT: `row_to_user` reads columns positionally. Every SELECT here lists
//! the columns as: user_id, username, password_hash, api_key_hash, is_admin,
//! disabled, prefs_json, created_at.

use super::Database;
use super::types::User;
use anyhow::Result;
use rusqlite::params;

impl Database {
    pub fn create_user(
        &self,
        user_id: &str,
        username: &str,
        password_hash: &str,
        api_key_hash: &str,
        is_admin: bool,
    ) -> Result<()> {
        let ts = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO users (user_id, username, password_hash, api_key_hash,
                                is_admin, disabled, prefs_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, '{}', ?6)",
            params![
                user_id,
                username,
                password_hash,
                api_key_hash,
                if is_admin { 1 } else { 0 },
                ts,
            ],
        )?;
        Ok(())
    }

    pub fn find_user_by_username(&self, username: &str) -> Result<Option<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, username, password_hash, api_key_hash,
                    is_admin, disabled, prefs_json, created_at
             FROM users WHERE username = ?1",
        )?;
        let user = stmt.query_row(params![username], row_to_user).ok();
        Ok(user)
    }

    pub fn find_user_by_api_key_hash(&self, api_key_hash: &str) -> Result<Option<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, username, password_hash, api_key_hash,
                    is_admin, disabled, prefs_json, created_at
             FROM users WHERE api_key_hash = ?1",
        )?;
        let user = stmt.query_row(params![api_key_hash], row_to_user).ok();
        Ok(user)
    }

    pub fn find_user_by_id(&self, user_id: &str) -> Result<Option<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, username, password_hash, api_key_hash,
                    is_admin, disabled, prefs_json, created_at
             FROM users WHERE user_id = ?1",
        )?;
        let user = stmt.query_row(params![user_id], row_to_user).ok();
        Ok(user)
    }

    pub fn list_users(&self) -> Result<Vec<User>> {
        let mut stmt = self.conn.prepare(
            "SELECT user_id, username, password_hash, api_key_hash,
                    is_admin, disabled, prefs_json, created_at
             FROM users ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_user)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn set_user_admin(&self, user_id: &str, is_admin: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET is_admin = ?1 WHERE user_id = ?2",
            params![if is_admin { 1 } else { 0 }, user_id],
        )?;
        Ok(())
    }

    pub fn set_user_disabled(&self, user_id: &str, disabled: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET disabled = ?1 WHERE user_id = ?2",
            params![if disabled { 1 } else { 0 }, user_id],
        )?;
        Ok(())
    }

    pub fn update_user_prefs(&self, user_id: &str, prefs_json: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET prefs_json = ?1 WHERE user_id = ?2",
            params![prefs_json, user_id],
        )?;
        Ok(())
    }

    pub fn rotate_api_key(&self, user_id: &str, new_api_key_hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET api_key_hash = ?1 WHERE user_id = ?2",
            params![new_api_key_hash, user_id],
        )?;
        Ok(())
    }

    /// Reset a user's credentials in one atomic UPDATE: overwrite the argon2
    /// `password_hash` AND rotate the `api_key_hash`. Used by the admin
    /// "reset any user's password" surface (server `POST /api/admin/users/
    /// {user_id}/reset-password` + local CLI `runai admin reset-password`).
    ///
    /// Rotating the api_key alongside the password is load-bearing: a reset
    /// implies the old secret is compromised / forgotten, so every previously
    /// issued Bearer (browser cookie + `~/.runai-identity`) must die and the
    /// user must log in again with the new password to mint a fresh key.
    /// Callers pass the SHA-256 hash of a freshly minted `new_api_key()`; the
    /// plaintext key is intentionally NOT persisted or returned — the user
    /// obtains a new one only by logging in.
    pub fn set_user_credentials(
        &self,
        user_id: &str,
        password_hash: &str,
        new_api_key_hash: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE users SET password_hash = ?1, api_key_hash = ?2 WHERE user_id = ?3",
            params![password_hash, new_api_key_hash, user_id],
        )?;
        Ok(())
    }

    /// Delete the `users` row. Auth-only deletion — callers that delete a user's
    /// owned resources / physical subtree must do that via the manager-level
    /// cascade (`SkillManager::delete_user_cascade`) BEFORE calling this.
    pub fn delete_user(&self, user_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM users WHERE user_id = ?1", [user_id])?;
        Ok(())
    }

    /// Distinct `owner_user_id`s on `resources` that no longer exist in
    /// `users` — orphan owners left by the pre-cascade delete path (or a DB
    /// imported from an older release). `doctor --fix` reaps each via
    /// `SkillManager::delete_user_cascade`.
    pub fn orphan_owner_user_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT owner_user_id FROM resources
             WHERE owner_user_id IS NOT NULL
               AND owner_user_id NOT IN (SELECT user_id FROM users)",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Null out `router_events.user_id` for a (being-deleted) user so their
    /// telemetry survives for aggregate stats but is no longer attributable.
    pub fn anonymize_router_events_for_user(&self, user_id: &str) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE router_events SET user_id = NULL WHERE user_id = ?1",
            [user_id],
        )?;
        Ok(n)
    }
}

fn row_to_user(r: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    Ok(User {
        user_id: r.get(0)?,
        username: r.get(1)?,
        password_hash: r.get(2)?,
        api_key_hash: r.get(3)?,
        is_admin: r.get::<_, i64>(4)? != 0,
        disabled: r.get::<_, i64>(5)? != 0,
        prefs_json: r.get(6)?,
        created_at: r.get(7)?,
    })
}

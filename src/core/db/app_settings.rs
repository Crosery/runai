//! Process-local application settings that are not user accounts.
//!
//! Owner-mode dashboard preferences live here so the synthetic owner can
//! persist `UserPrefs` without creating a row in `users`. Keeping owner state
//! out of `users` preserves first-real-user admin and owner-register invariants.

use super::Database;
use anyhow::Result;
use rusqlite::{OptionalExtension, params};

impl Database {
    pub fn app_setting(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT value FROM app_settings WHERE key = ?1")?;
        Ok(stmt.query_row(params![key], |row| row.get(0)).optional()?)
    }

    pub fn set_app_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

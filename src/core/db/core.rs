//! `Database` struct + connection lifecycle (open / borrow / schema version).
//!
//! Schema creation lives in `schema.rs` (`init_schema`), invoked here from
//! `open`. The domain CRUD methods are `impl Database` blocks in the sibling
//! files (resources / router / users / library / groups / trash / ai_summary).

use anyhow::Result;
use rusqlite::Connection;
use std::path::Path;
use std::time::Duration;

pub struct Database {
    pub(super) conn: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // The dashboard server opens one rusqlite connection per request.
        // Protocol endpoints such as `/skills/use/{name}` can receive the
        // same idempotency key concurrently, so let SQLite serialize short
        // write bursts instead of surfacing immediate SQLITE_BUSY failures.
        conn.busy_timeout(Duration::from_secs(5))?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Borrow the underlying SQLite connection. Use only when the typed
    /// CRUD on this struct can't express what you need (e.g. ad-hoc joins
    /// over `router_events.user_id`).
    pub fn conn_ref(&self) -> &Connection {
        &self.conn
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
}

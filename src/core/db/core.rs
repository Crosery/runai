//! `Database` struct + connection lifecycle (open / borrow / schema version).
//!
//! Schema creation lives in `schema.rs` (`init_schema`), invoked here from
//! `open`. The domain CRUD methods are `impl Database` blocks in the sibling
//! files (resources / router / users / library / groups / trash / ai_summary).

use anyhow::Result;
use rusqlite::Connection;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

pub struct Database {
    pub(super) conn: Connection,
}

/// Set of DB files this process has already migrated. `init_schema` is a
/// 26-migration batch of `CREATE TABLE IF NOT EXISTS` + guarded `ALTER`s; the
/// dashboard opens one connection PER REQUEST, so re-running the whole batch on
/// every open was the dominant per-request cost under concurrency. We run it
/// once per (process, file) and skip on later opens of the same file. It stays
/// idempotent, so a fresh process still migrates on its first open, and a DB
/// upgraded out-of-band is picked up the next time a process starts.
fn migrated_files() -> &'static Mutex<HashSet<PathBuf>> {
    static MIGRATED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    MIGRATED.get_or_init(|| Mutex::new(HashSet::new()))
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        // The dashboard server opens one rusqlite connection per request.
        // Protocol endpoints such as `/skills/use/{name}` can receive the
        // same idempotency key concurrently, so let SQLite serialize short
        // write bursts instead of surfacing immediate SQLITE_BUSY failures.
        conn.busy_timeout(Duration::from_secs(5))?;
        // `foreign_keys` is a PER-CONNECTION pragma (it does not persist and is
        // OFF by default). It used to be the first line of `init_schema`; now
        // that later opens of an already-migrated file skip `init_schema`, it
        // MUST be set here on every connection, or migration-skipped
        // connections would run with the two `ON DELETE CASCADE` foreign keys
        // (resource_targets → resources) unenforced.
        conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        // Read-path tuning for the telemetry-heavy dashboard: memory-map the
        // DB file and grow the page cache so repeated cold reads over a large
        // `router_events` table don't re-fault every open. Per-connection and
        // cheap to set; failure is non-fatal (older SQLite / restricted FS).
        let _ = conn.execute_batch(
            "PRAGMA mmap_size = 268435456;
             PRAGMA cache_size = -16000;",
        );
        let db = Self { conn };
        // Canonicalize so relative/absolute spellings of the same file share a
        // key; the file exists by now (Connection::open created it).
        let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let first_open = migrated_files().lock().unwrap().insert(key);
        if first_open {
            // Migration failure must retry on the next open, so drop the key.
            if let Err(e) = db.init_schema() {
                let k = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                migrated_files().lock().unwrap().remove(&k);
                return Err(e);
            }
        }
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

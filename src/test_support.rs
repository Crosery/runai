//! Test-only shared utilities. Accessible to any `#[cfg(test)]` module via
//! `crate::test_support::*`.

use std::sync::Mutex;

/// Process-wide lock for tests that mutate or depend on the `HOME` env var.
///
/// A test that *sets* HOME (e.g. `with_home` in `core::manager::tests`) must
/// hold this lock to serialize with other setters.
///
/// A test that *reads* HOME indirectly — through `dirs::home_dir()` — and then
/// holds state that references that path (open DB connections, tempdirs
/// rooted at home, etc.) must ALSO hold this lock while that state is alive.
/// Otherwise a concurrent setter can swap HOME to a tempdir mid-test; when
/// that tempdir is dropped, any open handle points at a deleted directory and
/// the next SQLite write trips `SQLITE_READONLY_DBMOVED` (ext. code 1032).
pub static HOME_LOCK: Mutex<()> = Mutex::new(());

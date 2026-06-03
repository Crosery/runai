//! SQLite schema + migrations + query layer.
//!
//! Thin facade: the `Database` struct lives in `core.rs`, its CRUD is split
//! into `impl Database` blocks across the domain submodules below, and the
//! pure value types live in `types.rs`. Every item external code reaches as
//! `crate::core::db::X` is re-exported here unchanged.
//!
//! INVARIANTS (do not break silently):
//! - `schema.rs` keeps `init_schema` + every v1–v15 migration MONOLITHIC.
//! - Row converters (`router.rs::row_to_router_event`, `users.rs::row_to_user`,
//!   `resources.rs::collect_resources`) read columns POSITIONALLY — each lives
//!   in the same file as the SELECTs whose column order it depends on. Moving a
//!   query without its column order is a silent bug.

mod ai_summary;
mod core;
mod groups;
mod library;
mod resources;
mod router;
mod router_stats;
mod schema;
mod trash;
mod types;
mod users;

pub use core::Database;
pub use types::{RouterEvent, RouterModelStat, RouterStatsSummary, TimelineBucket, User};

#[cfg(test)]
mod tests;

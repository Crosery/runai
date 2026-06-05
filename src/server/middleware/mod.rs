//! Cross-cutting axum middleware that doesn't belong to any single route family.
//!
//! Currently:
//! - `rate_limit` — per-IP / per-user fixed-window counters used to throttle
//!   abuse-prone endpoints (PLANNING §2.3 item 6).

pub(super) mod rate_limit;

//! Library subsystem coverage placeholder.
//!
//! The audited APIs (`api_library_list`, `api_library_mutate`, `api_library_clear`,
//! `api_library_fill`) live in `src/server/library.rs`, which is only introduced
//! after the multiuser server refactor (commit `3e94190 [refactor] 解耦 server`).
//! This worktree was branched off `544f256` — well before that refactor, and
//! before the multiuser commit `fdee38e` itself. There is no `src/server/library.rs`,
//! no `LibraryView`, no `user_skill_library` table, and `src/server.rs`
//! contains zero matches for `library` (verified via grep).
//!
//! Per the task contract: "If feature is genuinely impossible (signature changed,
//! depends on something not in HEAD, can't be tested without src change), add to
//! skipped with reason. Don't fabricate tests."
//!
//! All four features are recorded as `skipped` in the structured output.
//! This file exists only so the chunk reports a real path; it intentionally
//! contains no test functions.

#[test]
fn library_subsystem_absent_in_this_worktree() {
    // Sanity check: the worktree truly does not ship the library subsystem.
    // If a future rebase pulls in `src/server/library.rs`, this test will fail
    // and signal that real coverage tests should be written.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let library_path = std::path::Path::new(manifest_dir).join("src/server/library.rs");
    assert!(
        !library_path.exists(),
        "src/server/library.rs now exists — replace this placeholder with real coverage tests",
    );
}

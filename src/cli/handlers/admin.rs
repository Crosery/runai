//! `runai admin ...` CLI dispatch — local machine-owner operations that
//! write the local `runai.db` directly (no HTTP, no server required).
//!
//! Currently just `reset-password`: the 正规 replacement for hand-editing the
//! `users` table via SQL when a user forgets their password. It runs as the
//! machine owner (whoever can run the binary against `~/.runai/runai.db`),
//! so it does NOT authenticate — the filesystem is the trust boundary. It
//! reuses the exact same argon2 hashing + api_key rotation the server's
//! `POST /api/admin/users/{id}/reset-password` uses, so a password reset via
//! either path behaves identically.

use anyhow::{Result, bail};

use crate::cli::command_enums::AdminCommands;
use crate::core::auth::{self, BearerToken};
use crate::core::manager::SkillManager;

pub(crate) fn handle_admin(mgr: &SkillManager, cmd: AdminCommands) -> Result<()> {
    match cmd {
        AdminCommands::ResetPassword { username, password } => {
            reset_password(mgr, &username, password)
        }
    }
}

fn reset_password(mgr: &SkillManager, username: &str, password: Option<String>) -> Result<()> {
    let db = mgr.db();

    // Look the user up first so an unknown username errors cleanly (non-panic)
    // BEFORE we ever prompt for / read a password.
    let user = db
        .find_user_by_username(username)?
        .ok_or_else(|| anyhow::anyhow!("no such user: {username}"))?;

    let new_password = match password {
        Some(p) => p,
        None => {
            let p1 = rpassword::prompt_password(format!("New password for {username}: "))?;
            let p2 = rpassword::prompt_password("Confirm new password: ")?;
            if p1 != p2 {
                bail!("passwords do not match");
            }
            p1
        }
    };

    // Same validation the server enforces (min 6 chars).
    auth::validate_password(&new_password)?;

    let password_hash = auth::hash_password(&new_password)?;
    // Rotate the api_key alongside the password: the old secret is now
    // considered compromised/forgotten, so every previously-issued Bearer
    // (browser cookie / ~/.runai-identity) must die. The plaintext key is
    // intentionally discarded — the user logs in with the new password to
    // mint a fresh one.
    let new_key = auth::new_api_key();
    let api_key_hash = auth::key_hash(&BearerToken(new_key));
    db.set_user_credentials(&user.user_id, &password_hash, &api_key_hash)?;

    println!(
        "password reset for {username} (user_id {}); api_key rotated — the user must log in again with the new password",
        user.user_id
    );
    Ok(())
}

//! Community-market TUI tab (PLANNING.md §1.5 + §1.4).
//!
//! Fetches `GET /api/community/list` from the local dashboard server and
//! renders the result in the Community tab. Uses the same datasource as
//! the dashboard's community pane so a skill uploaded via the browser
//! shows up in the TUI on the next reload.
//!
//! ## Why HTTP instead of reading the DB directly
//! The community table (`community_skills`) is owned by the server
//! handler in `src/server/community.rs` — that handler enriches each row
//! with an uploader username via `users.username` join + applies
//! sort/limit/pagination semantics. Recreating that here would
//! double-source the contract. Instead the TUI talks to the same HTTP
//! endpoint browser clients use, which means:
//!   - team-mode 200 returns rows (auth required for upload, list is
//!     public on team mode);
//!   - owner-mode 404 returns empty (the local dashboard does not expose
//!     `/api/community/*` in owner mode — see PLANNING §1.4 + §1.1);
//!   - server down: connection refused becomes a friendly
//!     `community_error` string rather than panicking.
//!
//! Network calls happen in `reqwest::blocking` on a worker thread; the
//! result is delivered back to the UI thread through `community_loading`
//! flipping + `community_skills` being repopulated under a short-lived
//! lock. Since the TUI is single-threaded and we want to keep the panel
//! simple, we use a synchronous fetch with a 3 s timeout — for typical
//! local-loopback queries this returns in single-digit ms. Switching to
//! a `mpsc::Receiver` polling model (like the Market tab uses) is a
//! later refinement if the latency budget tightens.

use super::model::{App, CommunitySkill, UploadCandidate, UploadSource};

/// Default port the dashboard binds to (see `cli/mod.rs::Commands::Server`
/// + `tui::run_tui`'s auto-spawn path).
///
/// The community fetch follows the same default — there is intentionally no
/// per-user override here. A future iteration can read this from
/// `RUNAI_DASHBOARD_PORT`.
pub const COMMUNITY_PORT: u16 = 17888;

impl App {
    /// Synchronous fetch of `/api/community/list?sort=installs&limit=200`.
    /// Best-effort: errors land in `community_error` and the previously
    /// loaded rows stay visible (we'd rather show stale data than blank
    /// the table on a transient hiccup).
    ///
    /// Called from `data::reload` when switching into the Community tab.
    pub fn reload_community(&mut self) {
        self.community_loading = true;
        self.community_error.clear();
        // The dashboard server is normally already running because the
        // TUI auto-spawns it (see `cli::Commands` default path). If for
        // whatever reason it isn't, try once to bring it up — this is
        // the same idempotent helper the TUI startup uses.
        let _ = crate::server::ensure_running("127.0.0.1", COMMUNITY_PORT);
        let url = format!(
            "http://127.0.0.1:{}/api/community/list?sort=installs&limit=200",
            COMMUNITY_PORT
        );
        let resp = match reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .and_then(|c| c.get(&url).send())
        {
            Ok(r) => r,
            Err(e) => {
                self.community_loading = false;
                self.community_error = format!("connect: {e}");
                return;
            }
        };
        let status = resp.status();
        let body: Result<CommunityListJson, _> = resp.json();
        self.community_loading = false;
        match (status.as_u16(), body) {
            (200, Ok(payload)) => {
                self.community_skills = payload.skills;
                self.community_error.clear();
                if self.community_idx >= self.community_skills.len() {
                    self.community_idx = self.community_skills.len().saturating_sub(1);
                }
            }
            (404, _) => {
                // Owner mode — community endpoints are off by design.
                self.community_skills.clear();
                self.community_error = "owner 模式不暴露社区市场（仅 team 模式可用）".into();
            }
            (code, _) => {
                self.community_error = format!("HTTP {code}");
            }
        }
    }
}

/// Server response wrapper. We only care about the `skills` array; the
/// pagination metadata (`offset` / `limit` / `total`) is not surfaced in
/// the TUI today.
#[derive(Debug, serde::Deserialize)]
struct CommunityListJson {
    #[serde(default)]
    skills: Vec<CommunitySkill>,
}

impl App {
    /// Scan `~/.claude/skills/<name>/` and `<cwd>/.claude/skills/<name>/`
    /// for subdirectories containing `SKILL.md`. Populates
    /// `App::upload_candidates`; opens the picker via the caller flipping
    /// `mode` to `InputMode::CommunityUploadPicker`.
    ///
    /// Cheap scan: just reads two directories. No git, no recursion. A
    /// missing source dir is silently skipped (empty home / non-runai
    /// project).
    pub fn scan_upload_candidates(&mut self) {
        let mut out = Vec::new();
        if let Some(home) = dirs::home_dir() {
            collect_skills(
                &home.join(".claude/skills"),
                UploadSource::ClaudeUser,
                &mut out,
            );
        }
        if let Ok(cwd) = std::env::current_dir() {
            collect_skills(
                &cwd.join(".claude/skills"),
                UploadSource::ProjectCwd,
                &mut out,
            );
        }
        // Stable order: by source then by name (USER before PROJECT, alpha within).
        out.sort_by(|a, b| {
            a.source
                .short_label()
                .cmp(b.source.short_label())
                .then_with(|| a.name.cmp(&b.name))
        });
        self.upload_candidates = out;
        if self.upload_idx >= self.upload_candidates.len() {
            self.upload_idx = self.upload_candidates.len().saturating_sub(1);
        }
        self.upload_message.clear();
    }

    /// POST the currently-selected `upload_candidates[upload_idx]` to
    /// `/api/users/me/skills/upload` (private pool, `publish_status='draft'`
    /// — PLANNING §1.4 rewrite, issue #29; NOT the old direct-to-community
    /// `/api/community/upload`, which is admin-only now). Blocks the UI
    /// thread — uploads are small (single skill dir, typically &lt;1 MB
    /// tar.gz) and the local loopback loop closes in tens of ms; we accept
    /// the brief stall to avoid dragging a worker thread + channel for one
    /// POST.
    ///
    /// On success: reloads the community list (unaffected by this draft —
    /// it stays private until a publish-request is approved) and sets a
    /// success message reminding the user to run `runai community publish`.
    /// On failure: keeps the picker open with the error in `upload_message`
    /// so the user can pick a different candidate or fix and retry.
    pub fn upload_selected_candidate(&mut self) {
        if self.upload_busy {
            return;
        }
        let Some(cand) = self.upload_candidates.get(self.upload_idx).cloned() else {
            self.upload_message = "no candidate selected".into();
            return;
        };
        self.upload_busy = true;
        self.upload_message = format!("uploading {} …", cand.name);
        let result = do_upload(&cand);
        self.upload_busy = false;
        match result {
            Ok(_) => {
                self.upload_message = format!(
                    "uploaded {} — {}",
                    cand.name,
                    self.t().community_upload_draft_hint()
                );
                self.reload_community();
            }
            Err(e) => {
                self.upload_message = format!("upload failed: {e}");
            }
        }
    }
}

/// List immediate subdirs of `parent` that contain a `SKILL.md`. Errors
/// (missing parent, permission denied) silently produce zero entries —
/// the picker should not surface filesystem errors for a directory the
/// user might not even have set up yet.
fn collect_skills(parent: &std::path::Path, source: UploadSource, out: &mut Vec<UploadCandidate>) {
    let Ok(rd) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if !path.join("SKILL.md").is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        out.push(UploadCandidate {
            path: path.clone(),
            name: name.to_string(),
            source,
        });
    }
}

/// Synchronous upload helper. Mirrors `cli::handlers::community::upload`'s
/// tar.gz + multipart POST shape — we copy the logic instead of calling
/// across into the cli handlers module because the cli handlers reads
/// `~/.runai-identity` for the api_key, and the TUI on the server box
/// just talks to its own dashboard which authenticates the local user
/// via a different path (the dashboard's local-user session). We pull
/// the api_key from the same identity file when present, and fall back
/// to a bare POST (server returns 401 in team mode without auth — that's
/// the surfaced error message in the picker).
///
/// PLANNING §1.4 rewrite (issue #29): posts to `/api/users/me/skills/upload`
/// — the caller's PRIVATE pool, landing as `publish_status='draft'` — not
/// the old direct-to-community-pool `/api/community/upload` (now
/// admin-only). Use `runai community publish <name>` (or the future
/// dashboard "申请上架" action) to submit the draft for admin review.
fn do_upload(cand: &UploadCandidate) -> anyhow::Result<()> {
    let mut buf = Vec::new();
    {
        let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        tar.append_dir_all(&cand.name, &cand.path)?;
        tar.finish()?;
    }
    let url = format!("http://127.0.0.1:{COMMUNITY_PORT}/api/users/me/skills/upload");
    let client = reqwest::blocking::Client::new();
    let form = reqwest::blocking::multipart::Form::new()
        .text("name", cand.name.clone())
        .part(
            "bundle",
            reqwest::blocking::multipart::Part::bytes(buf)
                .file_name(format!("{}.tar.gz", cand.name))
                .mime_str("application/gzip")?,
        );
    let mut req = client.post(&url);
    if let Some(key) = read_identity_key_opt() {
        req = req.bearer_auth(key);
    }
    let resp = req.multipart(form).send()?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().unwrap_or_default();
        anyhow::bail!("HTTP {status} {body}");
    }
    Ok(())
}

/// Best-effort api_key read. Missing file / malformed JSON returns None —
/// caller treats that as "send unauthed and let the server 401 with a
/// clear error in the picker footer".
fn read_identity_key_opt() -> Option<String> {
    let path = dirs::home_dir()?.join(".runai-identity");
    let contents = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(contents.trim()).ok()?;
    v.get("api_key")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

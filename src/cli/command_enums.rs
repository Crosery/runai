use crate::core::server_mode::ServerMode;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "runai",
    version,
    about = "AI CLI resource manager for skills and MCP servers"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Scan CLI directories and adopt unmanaged skills
    Scan,
    /// Discover all SKILL.md files on disk (fast recursive search)
    Discover {
        /// Root directory to search (default: home directory)
        #[arg(long)]
        root: Option<String>,
    },
    /// List resources
    List {
        #[arg(long)]
        group: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        target: Option<String>,
    },
    /// Enable a resource or group
    Enable {
        name: String,
        #[arg(long, default_value = "claude")]
        target: String,
    },
    /// Disable a resource or group
    Disable {
        name: String,
        #[arg(long, default_value = "claude")]
        target: String,
    },
    /// Install a skill from GitHub
    Install { source: String },
    /// Install a skill from market
    MarketInstall {
        name: String,
        #[arg(long)]
        source: Option<String>,
    },
    /// Uninstall a resource
    Uninstall { name: String },
    /// Trash management
    Trash {
        #[command(subcommand)]
        command: TrashCommands,
    },
    /// Restore from backup (uses latest backup by default)
    Restore {
        /// Backup timestamp (omit for latest)
        #[arg(long)]
        timestamp: Option<String>,
    },
    /// Create a backup now
    Backup,
    /// List available backups (newest first)
    Backups,
    /// Search across installed resources and market
    Search { query: String },
    /// Browse market skills
    Market {
        /// Filter by source label or repo
        #[arg(long)]
        source: Option<String>,
        /// Search keyword in name/repo path/source label
        #[arg(long)]
        search: Option<String>,
    },
    /// Group management
    Group {
        #[command(subcommand)]
        command: GroupCommands,
    },
    /// Show status summary
    Status {
        #[arg(long, default_value = "claude")]
        target: String,
    },
    /// Start MCP server (stdio)
    McpServe,
    /// Start HTTP dashboard for router telemetry on localhost
    Server {
        /// Port to bind (default: 17888)
        #[arg(long, default_value_t = 17888)]
        port: u16,
        /// Host to bind (default: 127.0.0.1 — localhost only).
        /// Use 0.0.0.0 to expose on LAN, but note the DB contains user prompts.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Idempotent "ensure-running": exit immediately if the port is
        /// already serving; otherwise spawn the server as a detached
        /// background process and return. Designed for SessionStart / shell
        /// rc auto-launch — call it every session, it stays cheap.
        #[arg(long)]
        ensure: bool,
        /// Install a SessionStart hook in ~/.claude/settings.json that runs
        /// `runai server --ensure --port <port>` on every new Claude Code
        /// session so the dashboard auto-launches. Idempotent.
        #[arg(long, conflicts_with = "uninstall_hook")]
        install_hook: bool,
        /// Remove the SessionStart hook installed by `--install-hook`.
        #[arg(long)]
        uninstall_hook: bool,
        /// Register the server as an OS-level login auto-start so it
        /// runs in the background from boot. macOS = LaunchAgent plist
        /// loaded with `launchctl load -w`; Linux = systemd user unit
        /// enabled with `systemctl --user enable --now`; Windows = not
        /// implemented (use Task Scheduler manually). Idempotent.
        #[arg(long, conflicts_with_all = ["uninstall_autostart", "install_hook", "uninstall_hook", "ensure"])]
        install_autostart: bool,
        /// Remove the OS-level login auto-start created by `--install-autostart`.
        #[arg(long, conflicts_with_all = ["install_autostart", "install_hook", "uninstall_hook", "ensure"])]
        uninstall_autostart: bool,
        /// Server mode — `owner` (default, single-user self-serve) or `team`
        /// (multi-tenant; opens register/install endpoints). Picks the
        /// behavior of every route family explicitly so the dashboard isn't
        /// guessing based on DB state. See PLANNING.md §1.1.
        #[arg(long, value_enum, default_value_t = ServerMode::Owner)]
        mode: ServerMode,
        /// Path to a PEM-encoded TLS certificate. Required when --mode team
        /// binds a non-loopback host (cleartext over the network is rejected
        /// at startup). Used together with --tls-key. See PLANNING.md §2.3
        /// item 2 ("强制 HTTPS"). Actual rustls wiring lands in P5; this
        /// flag is the scaffold that carries the path through to AppState.
        #[arg(long, value_name = "PATH")]
        tls_cert: Option<PathBuf>,
        /// Path to the PEM-encoded TLS private key matching --tls-cert.
        #[arg(long, value_name = "PATH")]
        tls_key: Option<PathBuf>,
    },
    /// Register runai as MCP server in all CLI configs
    Register,
    /// Unregister runai from all CLI configs
    Unregister,
    /// Show usage statistics (most used skills/MCPs)
    Usage {
        /// Show only top N entries
        #[arg(long)]
        top: Option<usize>,
    },
    /// Update runai to the latest version
    Update,
    /// Run health checks on runai installation
    Doctor {
        /// Repair what can be repaired automatically: prune dangling
        /// `~/.{claude,codex,gemini,opencode}/skills/` symlinks and re-run
        /// the skill-row dedupe pass.
        #[arg(long)]
        fix: bool,
    },
    /// LLM-driven skill router (off by default; run `runai recommend setup`).
    Recommend {
        #[command(subcommand)]
        command: Option<RecommendCommands>,
        /// Run router for the given prompt (positional, no subcommand)
        prompt: Option<String>,
    },
    /// Community market (team mode): upload, list, install, delete shared skills.
    Community {
        #[command(subcommand)]
        command: CommunityCommands,
    },
    /// Local machine-owner admin operations on this box's runai.db.
    Admin {
        #[command(subcommand)]
        command: AdminCommands,
    },
}

#[derive(Subcommand)]
pub enum AdminCommands {
    /// Reset a user's password directly in the local runai.db (no HTTP).
    ///
    /// The 正规 replacement for hand-editing the `users` table via SQL when a
    /// user forgets their password. Operates as the machine owner on
    /// `~/.runai/runai.db` (honors RUNE_DATA_DIR): looks the user up by
    /// username, validates + argon2-hashes the new password, writes it, and
    /// rotates the api_key_hash so every previously-issued Bearer dies. The
    /// user must log in again with the new password to obtain a fresh key.
    ///
    /// Omit `--password` for an interactive hidden prompt (with confirm);
    /// pass `--password <pw>` for non-interactive / agent use.
    ResetPassword {
        /// Username whose password to reset (must exist in the local DB).
        username: String,
        /// New password (>= 6 chars). Omit to be prompted interactively
        /// with hidden input + confirmation.
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum CommunityCommands {
    /// Upload a local skill directory to your PRIVATE pool (draft).
    ///
    /// Reads `<path>/SKILL.md` to confirm it's a skill, tar.gz it, POST
    /// to `<server>/api/users/me/skills/upload` with the Bearer key from
    /// `~/.runai-identity`. `--name` defaults to the directory's basename.
    ///
    /// PLANNING §1.4 rewrite: this does NOT land the skill directly in the
    /// shared community pool anymore — it lands in your own private pool
    /// with `publish_status='draft'` and kicks off enrichment. Once the
    /// summary is ready, run `runai community publish <name>` to submit it
    /// for admin review; only after admin approval does it appear in
    /// `community list` for other users. The old direct-to-pool
    /// `/api/community/upload` endpoint is now admin-only (issue #29).
    Upload {
        /// Path to the skill directory (must contain SKILL.md)
        #[arg(long)]
        path: std::path::PathBuf,
        /// Skill name in the community pool (defaults to dirname)
        #[arg(long)]
        name: Option<String>,
        /// Server base URL; defaults to RUNAI_SERVER env or http://127.0.0.1:17888
        #[arg(long)]
        server: Option<String>,
    },
    /// Submit a draft private skill for admin review (draft → pending).
    ///
    /// POSTs to `<server>/api/users/me/skills/<name>/publish-request`.
    /// Fails with a 400 if enrichment hasn't produced a summary yet — wait
    /// a bit and retry, or run `runai recommend enrich --name <name>`.
    Publish {
        /// Skill name (must already be uploaded via `community upload`)
        name: String,
        /// Server base URL; defaults to RUNAI_SERVER env or http://127.0.0.1:17888
        #[arg(long)]
        server: Option<String>,
    },
    /// List skills in the community pool. Output: tab-separated rows
    /// `<uploader_uid> <name> <version> <installs>`, no header (agent-friendly).
    List {
        /// Sort by: installs / created / name
        #[arg(long, default_value = "installs")]
        sort: String,
        /// Server base URL; defaults to RUNAI_SERVER env or http://127.0.0.1:17888
        #[arg(long)]
        server: Option<String>,
    },
    /// Install a community skill into this account's private pool.
    Install {
        /// uploader user id (from `community list`)
        uploader: String,
        /// skill name
        name: String,
        /// Server base URL
        #[arg(long)]
        server: Option<String>,
    },
    /// Delete a community skill (only uploader or admin can).
    Delete {
        /// uploader user id
        uploader: String,
        /// skill name
        name: String,
        /// Server base URL
        #[arg(long)]
        server: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum RecommendCommands {
    /// Interactive setup: pick provider, paste API key, write ~/.runai/config.toml
    Setup,
    /// Show current router config (api_key redacted)
    Status,
    /// Print the hook JSON snippet to drop into ~/.claude/settings.json
    HookSnippet,
    /// Install the UserPromptSubmit hook into ~/.claude/settings.json (idempotent; backs up the old file)
    InstallHook,
    /// Remove the runai-installed UserPromptSubmit hook from ~/.claude/settings.json
    UninstallHook,
    /// Show router LLM usage telemetry: tokens per model, latency, recent calls
    Stats {
        /// Only count events in the last N hours (omit for all-time)
        #[arg(long)]
        hours: Option<i64>,
        /// Also print the N most recent calls
        #[arg(long, default_value = "0")]
        recent: usize,
    },
    /// Record user feedback on a recently-used skill and re-evaluate its
    /// llm_score + summary in light of it. Designed to be called by the
    /// main Claude agent at the end of a turn when it notices a skill was
    /// helpful or unhelpful — keeps the routing signal living, not frozen.
    Feedback {
        /// Skill name (must exist and have a current summary)
        skill: String,
        /// Short free-form note about how the skill performed
        /// (e.g. "user said the slides were too plain" or "perfect match for figma sync")
        #[arg(long)]
        note: String,
    },
    /// Fetch a skill's SKILL.md content AND record adoption atomically.
    /// Stdout = SKILL.md body. Side effects: usage_count +1, session
    /// adoption row written (if CLAUDE_SESSION_ID is set). The hook output
    /// no longer exposes any skill path, so the main agent must run this
    /// command to obtain a recommended skill's contents — making this the
    /// single source of truth for "skill adopted" signal.
    Get {
        /// Skill name (must exist under <data_dir>/skills/<name>/SKILL.md)
        skill: String,
    },
    /// Wipe all LLM summaries (resource_ai_summary) — next enrich rebuilds.
    ResetScoring {
        /// Skip the "are you sure" prompt (for scripts / hooks)
        #[arg(long)]
        yes: bool,
    },
    /// Generate bilingual AI summaries for skills (improves BM25 prefilter
    /// recall, especially for cross-language queries). Default mode picks up
    /// missing summaries AND re-enriches skills whose SKILL.md mtime is
    /// newer than the stored summary's timestamp.
    Enrich {
        /// Process at most N skills this run (omit for all that need it)
        #[arg(long)]
        limit: Option<usize>,
        /// Regenerate every skill's summary, ignoring mtime/exists checks
        #[arg(long, conflicts_with = "missing_only")]
        force: bool,
        /// Only enrich skills that have NO summary yet — skip stale-mtime
        /// refresh (cheapest mode, for "first launch / new install" use)
        #[arg(long, conflicts_with = "force")]
        missing_only: bool,
        /// Targeted repair: re-enrich ONLY skills whose existing summary is in
        /// the wrong language (prose fields not in `summary_lang`). Cheapest
        /// way to fix a leaked index without a full `--force` pass.
        #[arg(long, conflicts_with_all = ["force", "missing_only"])]
        fix_lang: bool,
        /// Print per-skill progress
        #[arg(long)]
        verbose: bool,
        /// How many skills to enrich concurrently (default 32). Each worker
        /// makes one LLM call at a time. DeepSeek v4-flash实测 500 并发都没
        /// rate limit；32 在速度和系统资源之间取平衡（337 个 skill ~29s）。
        /// 想更快可设 --concurrency 128 (10s) 或 337 (5s)。
        #[arg(long, default_value_t = 32)]
        concurrency: usize,
        /// Only enrich the named skill(s). Pass `--name X --name Y` to limit
        /// to a specific subset (e.g. after `runai install` to refresh just
        /// the freshly downloaded skills). When set, mtime/exists checks are
        /// bypassed for the listed names — they are always re-enriched.
        #[arg(long = "name")]
        names: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum GroupCommands {
    /// Create a new group
    Create {
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, default_value = "custom")]
        kind: String,
    },
    /// Add a resource to a group
    Add {
        group: String,
        resource: String,
        #[arg(long, default_value = "skill")]
        resource_type: String,
    },
    /// Remove a resource from a group
    Remove { group: String, resource: String },
    /// List all groups
    List,
    /// Delete a group (does not delete its members)
    Delete { id: String },
    /// Update group metadata (display name and/or description)
    Update {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Show one group's full details (description + members)
    Show { id: String },
}

#[derive(Subcommand)]
pub enum TrashCommands {
    /// List trash entries
    List,
    /// Restore a trashed resource by trash ID or resource name
    Restore { query: String },
    /// Permanently delete a trashed resource by trash ID or resource name
    Purge { query: String },
    /// Permanently delete everything in trash
    Empty,
}

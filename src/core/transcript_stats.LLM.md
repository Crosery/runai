---
module: core::transcript_stats
file: src/core/transcript_stats.rs
role: usage-stats
---

# transcript_stats

## Purpose
Derive per-skill / per-MCP usage counts from Claude Code session transcripts.
Claude Code writes each session as a JSONL file under `~/.claude/projects/<slug>/<session>.jsonl`;
every assistant turn includes `tool_use` blocks we mine for (a) `Skill` invocations
keyed by `input.skill` and (b) `mcp__<server>__<tool>` invocations aggregated per server.

## Public API
- `scan_default() -> Result<TranscriptStats>` — primary entry. Reads/writes the
  on-disk cache at `~/.runai/transcript-scan-cache.json`, so repeated calls in a
  TUI session are effectively free for unchanged files.
- `scan(root) -> Result<TranscriptStats>` — **no cache**, always full re-parse.
  Kept for tests and callers that explicitly want no persistence.
- `scan_with_cache(root, cache_path) -> Result<TranscriptStats>` — underlying
  function if the caller wants a custom cache location.
- `TranscriptStats::lookup(kind, name) -> (u64, Option<i64>)` — (count, last_used_unix_ts).
- `default_transcript_root()` / `default_cache_path()` — path resolvers; honor
  `RUNAI_TRANSCRIPTS_DIR` for tests.

## Key invariants
- **Cache fingerprint is `(mtime_secs, size)`** per jsonl file. Either changes → re-parse
  that file. Same → reuse the cached per-file counts. Deleted files are pruned on the next scan.
- **Per-file counts are stored, not just the aggregate.** That's what lets a single
  file's recount propagate correctly (the old contribution is replaced, not added).
- **Cache version bump discards old caches** instead of mis-parsing them. Bump
  `CACHE_VERSION` when `FileCacheEntry` / `CachedCount` shape changes.
- **Atomic cache write**: temp file + rename. A crash mid-write can't leave
  half-valid JSON that poisons the next startup.
- **MCP granularity is server-level**, not tool-level. 50× `mcp__runai__sm_list`
  + 20× `mcp__runai__sm_status` shows as `runai: 70`. Intentional — usage stats
  are for "which server is valuable", not "which sub-tool".
- Only `type == "assistant"` lines with `message.role == "assistant"` contribute.
  Guards against user messages whose prompts literally contain the text "tool_use".

## Touch points
- **Upstream**: `tui::app::App::reload` (overlays counts on the Skills/MCPs tabs);
  `manager::usage_stats` (CLI `runai usage`).
- **Downstream**: `paths::data_dir` (cache location); filesystem only — no network,
  no DB.

## Gotchas
- The first scan after install / after a `CACHE_VERSION` bump is a full re-parse
  of every jsonl. 231MB / 400 files ≈ 165ms on release build — one-time cost, then
  subsequent scans only re-read files whose mtime changed.
- macOS APFS mtime granularity is 1 second. Within-1s appends that keep size
  identical wouldn't trigger a rescan; in practice jsonl files only grow, so the
  size check covers this. Still: don't rely on sub-second change detection.
- `RUNAI_TRANSCRIPTS_DIR` env var overrides the default root (used by tests).
  If you add a second override, keep this one working — CI depends on it.
- If a jsonl is opened, partially appended, and still being written by Claude
  Code while we scan, we count only committed lines. Next scan after the file
  is fully flushed picks up the rest — no special handling needed.

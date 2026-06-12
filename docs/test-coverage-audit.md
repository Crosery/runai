# Test Coverage Audit

Date: 2026-06-12

This audit records current test evidence for runai feature surfaces. It is not a release checklist and does not replace the root `AGENTS.md` safety contract.

## Priority Rules

- P0: data loss, cross-user auth or ownership isolation failure, real HOME / CLI config mutation outside the isolated test environment, or a core command outage.
- P1: primary workflow broken or misleading, especially CLI/MCP/server entrypoints that agents or users run directly.
- P2: edge-path bug, documentation or command-contract mismatch with a clear workaround, or polish issue.

## Focused Gates Run

- `cargo fmt --check`
- `cargo test --test cli_surface_e2e`
- `cargo test --test mcp_stdio_test`
- `cargo test --all-targets -- --test-threads=1`
- `./tests/browser/run.sh`
- `./tests/vm/run.sh`
- `git diff --check`

The full cargo gate passed on 2026-06-12. Ignored tests in that run:

- `tests/install_test.rs::test_real_install_minimax` — manual real-network install test.
- `tests/mcp_stdio_test.rs::mcp_stdio_sm_install_returns_runai_install_command` — known P1 bug tracked in issue #13.
- `tests/search_market_cli_e2e.rs::e2e_documented_market_install_command_reaches_install_path` — known P1 bug tracked in issue #14.

The browser dashboard harness passed on 2026-06-12. The VM cross-machine harness passed on 2026-06-12 with `VM e2e: 30 passed, 0 failed`.

## Feature Coverage Map

| Surface | Coverage Evidence | Notes |
|---|---|---|
| CLI safety and destructive paths | `tests/safety_e2e.rs`, `tests/mcp_canonical_e2e.rs`, `tests/cli_target_symmetry.rs`, unit tests in `core::{backup,scanner,paths,manager,linker}` | Real binary + isolated HOME/RUNE_DATA_DIR for scan, backup/restore, register/unregister, enable/disable, trash/restore. |
| Small CLI inventory and utility surfaces | `tests/cli_surface_e2e.rs` | Covers `discover`, `list`, `status`, malformed `install`, `usage --top`, `backups`, `backup`, default-latest `restore`, `trash list/empty/purge`, `recommend hook-snippet`, and `recommend stats` with the real binary. |
| CLI groups | `tests/group_cli_e2e.rs` | Covers create/add/remove/show/update/delete, symlink rejection, same-name skill/MCP resource type. |
| CLI users | `tests/users_cli_e2e.rs` | Covers `users add`, hidden compatibility alias, password sources, duplicate rejection, first-user admin, api_key hash auth. |
| CLI market and search | `tests/search_market_cli_e2e.rs` | Covers search/market installed-state semantics with seeded cache and same-name MCP/public skill cases. Also covers `market-install` missing-skill failure without writing a managed payload; successful network install remains covered below the core/server install layers rather than by a live external download gate. |
| CLI install | `src/core/installer.rs` unit tests, `tests/cli_surface_e2e.rs`, ignored `tests/install_test.rs` | Covers GitHub source parsing, malformed input failing before network, archive entry safety, existing-destination no-overwrite. Successful real GitHub install remains a manual ignored test because it depends on external network/API state. |
| CLI recommend setup/enrich/get | `tests/recommend_setup_cli_e2e.rs`, `tests/recommend_enrich_cli_e2e.rs`, `tests/router_skill_lifecycle.rs`, `tests/prompt_leak_e2e.rs`, `tests/prompts_multiuser_e2e.rs` | Covers config persistence/redaction, enrich failure surfacing, owner-aware summaries, adoption counting, prompt leakage, prefs isolation. |
| CLI update | `tests/update_cli_e2e.rs` | Covers isolated binary replacement, checksum mismatch, symlinked cache refusal. |
| CLI server hook and ensure | `tests/server_hook_cli_e2e.rs`, `tests/server_ensure_cli_e2e.rs` | Covers SessionStart hook install/uninstall and ensure-running collision behavior. |
| CLI autostart | `tests/autostart_cli_e2e.rs`, `core::autostart` unit tests | Uses fake `launchctl` / `systemctl`; no real OS service mutation. |
| Community CLI | `tests/community_cli_e2e.rs` | Covers upload/list/install/delete over a real team-mode server. |
| MCP stdio | `tests/mcp_stdio_test.rs`, `src/mcp/tools/server.rs` unit tests | Covers JSON-RPC stdout framing, `RUNE_DATA_DIR`, `sm_list`, `sm_status`, `sm_backup`, `sm_backups`, `sm_recommend_stats`, tool registry, groups/search/status helpers. Issue #13 is represented as an ignored regression test. |
| HTTP MCP | `tests/mcp_http_e2e.rs` | Covers auth gate, tools/list, public/community/private install/upload/get/bundle/list flows. |
| Server auth and anti-abuse | `tests/auth_uniform_error_e2e.rs`, `tests/auth_telemetry_leak_e2e.rs`, `tests/rate_limit_e2e.rs`, `tests/anti_explore_e2e.rs` | Covers uniform login failure, telemetry privacy gates, path probes, and route-level rate limits. |
| Server owner/team mode | `tests/server_mode_e2e.rs`, `tests/multiuser_owner_e2e.rs` | Covers owner-mode register rejection, team registration, owner pools, private/public isolation, promotion, trash restore. |
| Server dashboard APIs | `tests/server_high_gap_audit_e2e.rs`, `tests/community_market_e2e.rs`, `tests/prefs_public_recommend_e2e.rs`, `tests/feedback_scope_e2e.rs` | Covers high-risk path traversal, event isolation, admin user operations, library import, community market, prefs scope, feedback owner checks. |
| Browser dashboard | `tests/browser/run.sh` and specs under `tests/browser/*.spec.cjs` | Manual Chrome harness passed on 2026-06-12. |
| Cross-machine client scripts | `tests/vm/run.sh`, `tests/vm/README.md`, plus cargo `tests/install_script_e2e.rs`, `tests/install_script_windows_e2e.rs`, and `tests/remote_client_script_e2e.rs` | Orbstack VM harness passed on 2026-06-12. Cargo now covers the documented bash `curl -fsSL <server>/install \| bash` failure path without curl `(23)`, non-interactive first install, second-run identity reuse, and uninstall cleanup in isolated HOME. A Windows-only PowerShell lifecycle gate exists for `windows-latest`/Windows machines; this macOS machine has no `pwsh`/`powershell`, so the Windows gate was compiled with `--no-run` here but not executed locally. |
| TUI | Unit-level tests in `tui::app` and `tests/tui_hook_panel_test.rs` | No full terminal E2E; current coverage is state-machine level. |

## Bugs Recorded

| Priority | Issue | Evidence |
|---|---|---|
| P1 | https://github.com/Crosery/runai/issues/11 | `runai list --target claude` hides disabled managed resources while `status` reports `1/2 enabled`; users can read this as missing resources. |
| P2 | https://github.com/Crosery/runai/issues/12 | README daily command says `runai trash` browses trash, but actual CLI requires `runai trash list`; `runai trash` exits 2 with help. |
| P1 | https://github.com/Crosery/runai/issues/13 | Real MCP stdio `sm_install(repo="owner/repo")` returns `rune install owner/repo` instead of `runai install owner/repo`. |
| P1 | https://github.com/Crosery/runai/issues/14 | README documents `runai market install <name>`, but clap only accepts top-level `runai market-install <name>`. |
| P1 | https://github.com/Crosery/runai/issues/15 | Fixed in the bash template: prompts read from `/dev/tty` only when a controlling TTY can be opened; otherwise the installer prints non-interactive env-var instructions and drains stdin before exit. Regression: `tests/install_script_e2e.rs::curl_pipe_new_device_without_credentials_fails_without_broken_pipe_noise`. |
| P1 | https://github.com/Crosery/runai/issues/16 | PowerShell template now resolves one profile root for identity/hook/settings/client/pin paths, verifies existing identity through `/api/me` before prompt skip, avoids top-level `exit` on install failures, and keeps generated hook/client `exit 1` semantics. Covered by `src/server/install.rs` template tests and a Windows-only physical lifecycle gate in `tests/install_script_windows_e2e.rs`; local macOS run compiled it with `--no-run` only. |
| P2 | https://github.com/Crosery/runai/issues/17 | PowerShell template no longer uses PS7-only raw `` `e`` ANSI literals; styling is routed through `Runai-Style` and `[char]27` only when ANSI is enabled. Covered by `server::install::tests::powershell_template_reuses_verified_identity_and_disables_raw_ansi`. |
| P1 | https://github.com/Crosery/runai/issues/18 | Bash lifecycle gate implemented in `tests/install_script_e2e.rs`: served `/install`, isolated client HOME, first install, second install, and `/uninstall` preserving unrelated hooks/MCPs while removing runai-owned hook/client/MCP/pin. Windows lifecycle gate added in `tests/install_script_windows_e2e.rs`; it awaits execution on Windows. |

## Process Incidents Recorded

- `vault-wisp-wisp-we3` — postmortem for a command-contract probe that ran `runai recommend uninstall-hook` against the real HOME. The hook was restored immediately and future command probes must default to isolated HOME/RUNE_DATA_DIR unless they are pure `--help` invocations.

## Remaining Manual Gap

- `tests/vm/README.md` still records one explicit gap: remote MCP `market_install` real network download is not performed in the VM harness; cargo E2E covers the bounded failure/no-hang path.
- Remote client installer coverage still lacks a local physical Windows install/uninstall result. This macOS environment has no `pwsh` or `powershell`; `tests/install_script_windows_e2e.rs` is the Windows runner gate and was compiled here with `cargo test --test install_script_windows_e2e --no-run`.

#!/usr/bin/env bash
# Local CI gate — mirrors .github/workflows/ci.yml EXACTLY, in the same order.
# Run this after every code change, before you commit / push. If this is green,
# GitHub CI is green (the runner just adds the linux/windows matrix).
#
#   ./scripts/ci-local.sh
#
# CI runs the steps in this order and gates each on the previous, so a single
# unformatted line fails BEFORE any test runs — which is exactly how the
# 2026-06-03 push broke (fmt not run locally). fmt is therefore checked first.
set -euo pipefail
cd "$(dirname "$0")/.."

bold() { printf '\033[1m%s\033[0m\n' "$1"; }

bold "==> [1/3] cargo fmt --check"
if ! cargo fmt --check; then
  echo "    formatting failed — run 'cargo fmt' to fix, then re-run this." >&2
  exit 1
fi

bold "==> [2/3] cargo clippy --all-targets -- -W clippy::all"
cargo clippy --all-targets -- -W clippy::all

bold "==> [3/3] cargo test -- --test-threads=1"
# SQLite dislikes parallel I/O; single-threaded matches CI. This includes the
# physical e2e suites (safety_e2e, multiuser_owner_e2e, cli_target_symmetry,
# mcp_canonical_e2e, mcp_stdio) on unix — they are the real regression gate for
# the destructive-path / owner-isolation invariants.
cargo test -- --test-threads=1

bold "ALL GREEN — matches the CI gate (fmt + clippy + test)."

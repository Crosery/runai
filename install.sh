#!/usr/bin/env sh
# runai installer (macOS / Linux)
#
# Detects your platform, downloads the matching release binary from GitHub,
# verifies its checksum, installs it on PATH, and optionally wires up the
# edition you pick.
#
#   curl -fsSL https://raw.githubusercontent.com/Crosery/runai/main/install.sh | sh
#
# Non-interactive (agents / CI): pass flags after `-s --`
#   curl -fsSL .../install.sh | sh -s -- --edition personal --yes
#   curl -fsSL .../install.sh | sh -s -- --edition team --yes --no-setup
#
# Flags:
#   --edition personal|team   personal = local router + hook; team = run the server
#   --version <tag|latest>    release to install (default: latest stable)
#   --bin-dir <path>          where to put the runai binary (default: auto)
#   --yes                     non-interactive; take defaults, never prompt
#   --no-hook                 skip installing the Claude Code hook
#   --no-setup                skip the post-install edition setup entirely
#   --dry-run                 print what would happen, change nothing
#   --uninstall               remove the runai binary from its bin dir
#   --help                    this help
set -eu

REPO="Crosery/runai"
RELEASE_BASE="https://github.com/${REPO}/releases"

EDITION=""
VERSION="latest"
BIN_DIR=""
ASSUME_YES=0
DO_HOOK=1
DO_SETUP=1
DRY_RUN=0
UNINSTALL=0

# ---------- pretty output (colors only on a TTY; no emoji) ----------
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_RESET=$(printf '\033[0m'); C_BOLD=$(printf '\033[1m')
  C_DIM=$(printf '\033[2m');   C_BLUE=$(printf '\033[34m')
  C_GREEN=$(printf '\033[32m'); C_YELLOW=$(printf '\033[33m'); C_RED=$(printf '\033[31m')
else
  C_RESET=''; C_BOLD=''; C_DIM=''; C_BLUE=''; C_GREEN=''; C_YELLOW=''; C_RED=''
fi
step() { printf '%s==>%s %s\n' "$C_BLUE$C_BOLD" "$C_RESET" "$*"; }
ok()   { printf '%s  [ok]%s %s\n' "$C_GREEN" "$C_RESET" "$*"; }
warn() { printf '%s  [warn]%s %s\n' "$C_YELLOW" "$C_RESET" "$*"; }
err()  { printf '%s  [error]%s %s\n' "$C_RED$C_BOLD" "$C_RESET" "$*" >&2; }
die()  { err "$*"; exit 1; }
banner() {
  printf '%s\n' "${C_BOLD}${C_BLUE}"
  printf '  +--------------------------------------------+\n'
  printf '  |   runai installer  -  AI skill/MCP manager |\n'
  printf '  +--------------------------------------------+\n'
  printf '%s\n' "${C_RESET}"
}

usage() { sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0; }

# ---------- arg parsing ----------
while [ $# -gt 0 ]; do
  case "$1" in
    --edition) EDITION="${2:-}"; shift 2 ;;
    --edition=*) EDITION="${1#*=}"; shift ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --version=*) VERSION="${1#*=}"; shift ;;
    --bin-dir) BIN_DIR="${2:-}"; shift 2 ;;
    --bin-dir=*) BIN_DIR="${1#*=}"; shift ;;
    --yes|-y) ASSUME_YES=1; shift ;;
    --no-hook) DO_HOOK=0; shift ;;
    --no-setup) DO_SETUP=0; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    --uninstall) UNINSTALL=1; shift ;;
    --help|-h) usage ;;
    *) die "unknown flag: $1 (try --help)" ;;
  esac
done

run() { # echo + execute, or just echo under --dry-run
  printf '%s    $ %s%s\n' "$C_DIM" "$*" "$C_RESET"
  [ "$DRY_RUN" -eq 1 ] || "$@"
}

have() { command -v "$1" >/dev/null 2>&1; }

# ---------- platform detection ----------
detect_platform() {
  os=$(uname -s); arch=$(uname -m)
  case "$os" in
    Darwin) OS=darwin ;;
    Linux)  OS=linux ;;
    *) die "unsupported OS: $os (Windows: use install.ps1 via 'irm .../install.ps1 | iex')" ;;
  esac
  case "$arch" in
    x86_64|amd64) ARCH=amd64 ;;
    arm64|aarch64) ARCH=arm64 ;;
    *) die "unsupported arch: $arch" ;;
  esac
  ASSET="runai-${OS}-${ARCH}.tar.gz"
}

# ---------- bin dir selection ----------
pick_bin_dir() {
  [ -n "$BIN_DIR" ] && return 0
  for d in "$HOME/.local/bin" "$HOME/.cargo/bin" "/usr/local/bin"; do
    if [ -d "$d" ] && [ -w "$d" ]; then BIN_DIR="$d"; return 0; fi
  done
  BIN_DIR="$HOME/.local/bin"   # will be created
}

# ---------- download helpers ----------
fetch() { # fetch <url> <out>
  if have curl; then run curl -fsSL "$1" -o "$2"
  elif have wget; then run wget -qO "$2" "$1"
  else die "need curl or wget to download"; fi
}

sha256_of() { # print sha256 of a file
  if have sha256sum; then sha256sum "$1" | awk '{print $1}'
  elif have shasum; then shasum -a 256 "$1" | awk '{print $1}'
  else echo ""; fi
}

# ---------- uninstall path ----------
do_uninstall() {
  pick_bin_dir
  target="$BIN_DIR/runai"
  if [ -e "$target" ]; then
    step "Removing $target"
    run rm -f "$target"
    ok "runai binary removed. To unwire hooks, run 'runai recommend uninstall-hook' / 'runai server --uninstall-hook' BEFORE removing, or edit ~/.claude/settings.json."
  else
    warn "no runai binary at $target — nothing to remove"
  fi
  exit 0
}

# ---------- interactive edition pick ----------
prompt_edition() {
  [ -n "$EDITION" ] && return 0
  if [ "$ASSUME_YES" -eq 1 ] || [ ! -t 0 ]; then EDITION="personal"; return 0; fi
  printf '\n%sPick an edition:%s\n' "$C_BOLD" "$C_RESET"
  printf '  %s1%s) personal  - local skill router + Claude Code hook, no account, no server\n' "$C_BOLD" "$C_RESET"
  printf '  %s2%s) team      - run the multi-user dashboard server; others connect as clients\n' "$C_BOLD" "$C_RESET"
  printf 'choice [1]: '
  read -r reply || reply=1
  case "$reply" in 2|team) EDITION=team ;; *) EDITION=personal ;; esac
}

confirm() { # confirm <question> ; default yes
  [ "$ASSUME_YES" -eq 1 ] && return 0
  [ ! -t 0 ] && return 0
  printf '%s [Y/n]: ' "$1"
  read -r r || r=y
  case "$r" in n|N|no|NO) return 1 ;; *) return 0 ;; esac
}

# ---------- main ----------
banner
[ "$UNINSTALL" -eq 1 ] && do_uninstall

detect_platform
ok "platform: ${OS}/${ARCH}  asset: ${ASSET}"

pick_bin_dir
ok "install dir: ${BIN_DIR}"

# resolve download URLs
if [ "$VERSION" = "latest" ]; then
  url="${RELEASE_BASE}/latest/download/${ASSET}"
  sums="${RELEASE_BASE}/latest/download/checksums.txt"
else
  url="${RELEASE_BASE}/download/${VERSION}/${ASSET}"
  sums="${RELEASE_BASE}/download/${VERSION}/checksums.txt"
fi

tmp=$(mktemp -d "${TMPDIR:-/tmp}/runai-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

step "Downloading ${ASSET} (${VERSION})"
fetch "$url" "$tmp/$ASSET"

step "Verifying checksum"
if [ "$DRY_RUN" -eq 0 ]; then
  if fetch "$sums" "$tmp/checksums.txt" 2>/dev/null; then
    want=$(grep " ${ASSET}\$\|  ${ASSET}\$\|\*${ASSET}\$" "$tmp/checksums.txt" 2>/dev/null | awk '{print $1}' | head -1)
    got=$(sha256_of "$tmp/$ASSET")
    if [ -n "$want" ] && [ -n "$got" ]; then
      [ "$want" = "$got" ] || die "checksum mismatch: expected $want got $got"
      ok "sha256 verified"
    else
      warn "could not verify checksum (missing tool or entry) — continuing"
    fi
  else
    warn "checksums.txt not found for this release — skipping verification"
  fi
fi

step "Installing binary to ${BIN_DIR}"
[ -d "$BIN_DIR" ] || run mkdir -p "$BIN_DIR"
if [ "$DRY_RUN" -eq 0 ]; then
  tar xzf "$tmp/$ASSET" -C "$tmp"
fi
run install -m 0755 "$tmp/runai" "$BIN_DIR/runai" 2>/dev/null || run cp "$tmp/runai" "$BIN_DIR/runai"
[ "$DRY_RUN" -eq 1 ] || chmod +x "$BIN_DIR/runai" 2>/dev/null || true
RUNAI="$BIN_DIR/runai"
if [ "$DRY_RUN" -eq 0 ]; then
  ver=$("$RUNAI" --version 2>/dev/null || echo "?")
  ok "installed: $ver"
fi

# PATH hint
case ":$PATH:" in
  *":$BIN_DIR:"*) : ;;
  *) warn "$BIN_DIR is not on PATH. Add to your shell rc:"
     printf '      %sexport PATH="%s:$PATH"%s\n' "$C_BOLD" "$BIN_DIR" "$C_RESET" ;;
esac

# ---------- edition setup ----------
if [ "$DO_SETUP" -eq 0 ]; then
  ok "skipping edition setup (--no-setup). Binary is installed."
  exit 0
fi

prompt_edition
step "Edition: ${EDITION}"

case "$EDITION" in
  personal)
    if [ "$ASSUME_YES" -eq 0 ] && [ -t 0 ]; then
      if confirm "Configure the LLM skill router now (provider + API key)?"; then
        run "$RUNAI" recommend setup
      else
        warn "skipped — run 'runai recommend setup' later to enable the router"
      fi
    else
      warn "non-interactive: run 'runai recommend setup' to configure the router (needs an API key)"
    fi
    if [ "$DO_HOOK" -eq 1 ]; then
      step "Wiring Claude Code hooks (idempotent)"
      run "$RUNAI" recommend install-hook
      run "$RUNAI" server --install-hook
    fi
    ok "personal edition ready. Launch the TUI with: runai"
    ;;
  team)
    step "Team edition: starting the dashboard server on login"
    run "$RUNAI" server --install-autostart
    ok "server set to auto-start. Open http://127.0.0.1:17888 and register — the first account becomes admin."
    printf '  %sRemote clients connect with:%s curl -fsSL http://<this-host>:17888/install | bash\n' "$C_BOLD" "$C_RESET"
    ;;
  *)
    die "unknown edition: $EDITION (use personal or team)"
    ;;
esac

printf '\n%sDone.%s  runai %s\n' "$C_GREEN$C_BOLD" "$C_RESET" "$([ "$DRY_RUN" -eq 1 ] && echo '(dry-run)')"

#!/usr/bin/env bash
# runai client install — registers (or logs into) a runai account on the
# remote server, persists the resulting api_key to ~/.runai-identity, and
# wires a Claude Code UserPromptSubmit hook that POSTs every prompt to
# the server with Authorization: Bearer <api_key>.
#
# This file is a TEMPLATE — the server hydrates it per request:
#   - `{SERVER_URL}` is substituted with the URL the script was curl'd from.
#   - Sections wrapped in `# === RUNAI_SECTION:owner-only ... ===` are
#     stripped when the server is in `team` mode.
#   - Sections wrapped in `# === RUNAI_SECTION:team-only ... ===` are
#     stripped when the server is in `owner` mode.
#   - The server returns the assembled script body; in `owner` mode the
#     /install endpoint returns 404 instead, so this file never reaches a
#     remote client when the server is owner-mode.
# See src/server/install.rs::render_install_script for the renderer.
#
# Usage:
#   curl -fsSL http://<SERVER>:<PORT>/install | bash
#
# Non-interactive (agent / CI):
#   RUNAI_USERNAME=alice RUNAI_PASSWORD=hunter2 \
#     bash <(curl -fsSL http://<SERVER>:<PORT>/install)
#
# Subcommand flags (run a single phase):
#   --register-only   only set up identity (register or login + write
#                     ~/.runai-identity); skip hook and settings patch.
#   --login-only      sign in only; do NOT auto-register on 401.
#   --hook-only       skip auth; require an existing ~/.runai-identity;
#                     write ~/.runai-hook.sh and patch settings.json.
#   --help, -h        print this help and exit.
#
# What this does (default, all phases):
#   1) Resolve credentials (env > prompt). Try POST /auth/login, fall back
#      to POST /users/register on 401. Write returned api_key into
#      ~/.runai-identity (mode 600).
#   2) Write ~/.runai-hook.sh — bash wrapper that curls /recommend with
#      Authorization: Bearer <key from ~/.runai-identity>.
#   3) Patch ~/.claude/settings.json so UserPromptSubmit calls the hook.
#      Original settings.json is backed up to .runai-bak first.
#
# What this does NOT do:
#   - Install any binary.
#   - Modify anything outside ~/.runai-identity, ~/.runai-hook.sh,
#     and ~/.claude/settings.json.
#   - Touch your existing hooks — runai's entry is appended, others kept.
#
# Uninstall: curl -fsSL http://<SERVER>:<PORT>/uninstall | bash

set -euo pipefail

# {SERVER_URL} is substituted by the server at request time.
SERVER_URL="{SERVER_URL}"
IDENTITY_PATH="$HOME/.runai-identity"
SERVER_PIN_PATH="$HOME/.runai-server.json"
HOOK_PATH="$HOME/.runai-hook.sh"
SETTINGS_PATH="$HOME/.claude/settings.json"

# Phase flags. Default = run all three. `--*-only` selects a single phase.
DO_AUTH=1
DO_HOOK=1
DO_SETTINGS=1
# When set to 1, `--login-only` semantics apply: do NOT fall back to
# /users/register if login fails.
LOGIN_ONLY=0

print_help() {
  cat <<'HELP'
runai client install — TTY + non-interactive (agent-friendly) installer.

Usage:
  curl -fsSL http://<SERVER>:<PORT>/install | bash
  RUNAI_USERNAME=alice RUNAI_PASSWORD=hunter2 \
    bash <(curl -fsSL http://<SERVER>:<PORT>/install)

Phase flags (mutually exclusive; default runs all three):
  --register-only   Identity phase only: register-or-login + write
                    ~/.runai-identity. Skip hook + settings patch.
  --login-only      Identity phase only, no auto-register: fail if the
                    account does not already exist.
  --hook-only       Skip identity phase entirely; install hook script
                    and patch ~/.claude/settings.json. Requires an
                    existing ~/.runai-identity.
  --help, -h        Print this and exit.

Environment variables (substitute for TTY prompts):
  RUNAI_USERNAME    Username for register / login.
  RUNAI_PASSWORD    Password for register / login.
  HOME              Used to derive ~/.runai-identity, ~/.runai-hook.sh,
                    ~/.claude/settings.json paths. Override to drive the
                    installer against an isolated tree (CI, e2e tests).

Exit codes:
  0  success (or no-op when phase was already done)
  1  missing required input, auth failed, or settings.json patch failed

See https://<server>/ for the dashboard once installed.
HELP
}

# Parse argv. Keep the parser tiny — every option is order-independent.
while [[ $# -gt 0 ]]; do
  case "$1" in
    --help|-h)
      print_help
      exit 0
      ;;
    --register-only)
      DO_AUTH=1; DO_HOOK=0; DO_SETTINGS=0
      LOGIN_ONLY=0
      shift
      ;;
    --login-only)
      DO_AUTH=1; DO_HOOK=0; DO_SETTINGS=0
      LOGIN_ONLY=1
      shift
      ;;
    --hook-only)
      DO_AUTH=0; DO_HOOK=1; DO_SETTINGS=1
      shift
      ;;
    *)
      echo "runai-install: unknown argument: $1" >&2
      echo "run with --help for usage." >&2
      exit 1
      ;;
  esac
done

if [[ "$SERVER_URL" == "{""SERVER_URL""}" || -z "$SERVER_URL" ]]; then
  echo "runai-install: SERVER_URL placeholder was not substituted." >&2
  echo "did you curl this script directly from the runai server's /install endpoint?" >&2
  exit 1
fi

# ANSI styling. Only emit codes when stdout is a real TTY; piped output
# (e.g. CI logs) gets plain text. Color picks:
#   C1 cyan-ish brand / dividers
#   C2 green ok marks
#   C3 yellow warnings
#   C4 red errors
#   D  dim secondary
#   B  bold
if [ -t 1 ]; then
  C1=$'\033[38;5;81m'   # cyan
  C2=$'\033[38;5;114m'  # green
  C3=$'\033[38;5;221m'  # yellow
  C4=$'\033[38;5;203m'  # red
  D=$'\033[2m'          # dim
  B=$'\033[1m'          # bold
  R=$'\033[0m'          # reset
else
  C1='' ; C2='' ; C3='' ; C4='' ; D='' ; B='' ; R=''
fi

step() {
  # step N/M  description
  printf "${C1}┃${R} ${B}[%s]${R} %s\n" "$1" "$2"
}
ok() {
  printf "  ${C2}[OK]${R} %s\n" "$1"
}
warn() {
  printf "  ${C3}[..]${R} %s\n" "$1"
}
fail() {
  printf "  ${C4}[!!]${R} %s\n" "$1" >&2
}
hr() {
  printf "${C1}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${R}\n"
}

printf "\n"
hr
printf "${C1}┃${R}  ${B}runai${R} ${D}skill router${R}    ${D}client install${R}\n"
hr
printf "${D}  server   ${R}%s\n" "$SERVER_URL"
printf "${D}  identity ${R}%s\n" "$IDENTITY_PATH"
printf "${D}  hook     ${R}%s\n" "$HOOK_PATH"
printf "${D}  config   ${R}%s\n" "$SETTINGS_PATH"
hr
printf "\n"

# === RUNAI_SECTION:owner-only START ===
# Reserved scaffold: this block is delivered ONLY when the server is in
# owner mode. The owner mode currently returns 404 for /install entirely
# (single-user self-serve has no remote client surface), so this section
# stays empty by design — kept here so the template grammar is symmetric
# and future owner-mode-specific commands have a home.
# === RUNAI_SECTION:owner-only END ===

# === RUNAI_SECTION:team-only START ===
# Everything below is the team-mode client surface: register / login,
# write identity, install hook wrapper, patch Claude Code settings.json.
# The server strips this whole block before serving the script if it is
# ever run in owner mode (currently unreachable: owner mode returns 404
# at the route level — see PLANNING.md §1.2).

# 1) Account setup. Prompt for credentials (or read from env), try login,
#    fall back to register unless --login-only.
#
#    If ~/.runai-identity already exists with a valid key, skip the prompt
#    entirely — re-running the installer should not force a re-login.
if [[ "$DO_AUTH" -eq 1 ]]; then
  step "1/3" "account setup"
  if [[ -f "$IDENTITY_PATH" ]] && grep -q '"api_key"' "$IDENTITY_PATH" 2>/dev/null; then
    ok "found existing identity, reusing stored api_key"
    printf "  ${D}(rm %s to switch user)${R}\n\n" "$IDENTITY_PATH"
  else
    printf "  ${D}new device — register or sign in to %s${R}\n" "$SERVER_URL"

    # Early TTY guard: if both username + password env vars are unset AND
    # stdin is a pipe (the classic `curl ... | bash` case), `read` will
    # immediately return an empty string and the run will fail with the
    # cryptic "username cannot be empty" message. Surface the real cause
    # instead so the user knows how to fix it.
    if [[ -z "${RUNAI_USERNAME:-}" || -z "${RUNAI_PASSWORD:-}" ]] && [[ ! -t 0 ]]; then
      fail "stdin is a pipe — can't prompt for username / password"
      printf "  ${D}use one of these forms instead:${R}\n\n"
      printf "    ${B}bash <(curl -fsSL %s/install)${R}        # interactive prompt\n" "$SERVER_URL"
      printf "    ${B}RUNAI_USERNAME=x RUNAI_PASSWORD=y curl -fsSL %s/install | bash${R}\n\n" "$SERVER_URL"
      exit 1
    fi

    # Username: env first, then TTY prompt. Env wins so RUNAI_USERNAME=x
    # works in CI / agent contexts where there is no terminal.
    if [[ -z "${RUNAI_USERNAME:-}" ]]; then
      printf "  ${B}username${R}  "
      read -r RUNAI_USERNAME
    else
      ok "using RUNAI_USERNAME from env"
    fi
    if [[ -z "${RUNAI_USERNAME// /}" ]]; then
      fail "username cannot be empty (set RUNAI_USERNAME or answer the prompt)"
      exit 1
    fi

    # Password: same dual-mode resolution. We read char-by-char so the
    # user gets a `*` echo per keystroke (`read -rs` is fully silent and
    # makes it impossible to tell whether a keypress registered, which
    # was the most common cause of "password too short" failures).
    if [[ -z "${RUNAI_PASSWORD:-}" ]]; then
      printf "  ${B}password${R}  "
      RUNAI_PASSWORD=''
      while IFS= read -rsn1 _ch; do
        # Enter / EOF terminates input.
        if [[ -z "$_ch" || "$_ch" == $'\n' ]]; then
          break
        fi
        # Backspace (DEL 0x7f) — pop last char + visually erase the star.
        if [[ "$_ch" == $'\177' ]]; then
          if [[ -n "$RUNAI_PASSWORD" ]]; then
            RUNAI_PASSWORD="${RUNAI_PASSWORD%?}"
            printf '\b \b'
          fi
          continue
        fi
        # Ignore other ctrl chars (Ctrl+C interrupts the script naturally).
        if [[ "$_ch" < $' ' ]]; then
          continue
        fi
        RUNAI_PASSWORD+="$_ch"
        printf '*'
      done
      unset _ch
      printf "\n"
    else
      ok "using RUNAI_PASSWORD from env"
    fi
    if [[ -z "$RUNAI_PASSWORD" ]]; then
      fail "password cannot be empty (set RUNAI_PASSWORD or answer the prompt)"
      exit 1
    fi

    AUTH_BODY=$(printf '{"username":"%s","password":"%s"}' \
      "$(printf '%s' "$RUNAI_USERNAME" | python3 -c 'import json,sys;print(json.dumps(sys.stdin.read())[1:-1])')" \
      "$(printf '%s' "$RUNAI_PASSWORD" | python3 -c 'import json,sys;print(json.dumps(sys.stdin.read())[1:-1])')")

    # Try login first. Any HTTP 2xx → logged in. Otherwise fall through to register.
    warn "trying sign-in as ${B}${RUNAI_USERNAME}${R}"
    LOGIN_HTTP=$(curl -s -o /tmp/runai-login-resp.$$ -w '%{http_code}' \
      -X POST "$SERVER_URL/auth/login" \
      -H 'Content-Type: application/json' \
      -d "$AUTH_BODY" || echo 000)
    if [[ "$LOGIN_HTTP" == "200" ]]; then
      ok "signed in as ${B}${RUNAI_USERNAME}${R}"
      RESP_FILE=/tmp/runai-login-resp.$$
    elif [[ "$LOGIN_ONLY" -eq 1 ]]; then
      fail "sign-in failed (HTTP $LOGIN_HTTP) and --login-only is set"
      rm -f /tmp/runai-login-resp.$$
      exit 1
    else
      warn "user does not exist, registering"
      REG_HTTP=$(curl -s -o /tmp/runai-reg-resp.$$ -w '%{http_code}' \
        -X POST "$SERVER_URL/users/register" \
        -H 'Content-Type: application/json' \
        -d "$AUTH_BODY" || echo 000)
      if [[ "$REG_HTTP" == "201" || "$REG_HTTP" == "200" ]]; then
        ok "registered ${B}${RUNAI_USERNAME}${R}"
        RESP_FILE=/tmp/runai-reg-resp.$$
      else
        fail "auth failed (login=$LOGIN_HTTP register=$REG_HTTP)"
        printf "  ${D}server says:${R} " >&2
        cat /tmp/runai-reg-resp.$$ >&2 || true
        printf "\n" >&2
        rm -f /tmp/runai-login-resp.$$ /tmp/runai-reg-resp.$$
        exit 1
      fi
    fi

    # Extract user_id + api_key with python3 (stdlib, ships on macOS / most Linux).
    IDENTITY_JSON=$(python3 - "$RESP_FILE" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
out = {
    "version": 1,
    "server": None,
    "user_id": d["user_id"],
    "username": d["username"],
    "api_key": d["api_key"],
    "is_admin": d.get("is_admin", False),
}
print(json.dumps(out, indent=2))
PY
    )
    # Stamp the server URL so users can tell which server this identity points at.
    printf '%s\n' "$IDENTITY_JSON" | python3 -c "
import json, sys
d = json.loads(sys.stdin.read())
d['server'] = '$SERVER_URL'
print(json.dumps(d, indent=2))" > "$IDENTITY_PATH"
    chmod 600 "$IDENTITY_PATH"
    rm -f /tmp/runai-login-resp.$$ /tmp/runai-reg-resp.$$
    ok "wrote ${D}${IDENTITY_PATH}${R} ${D}(mode 600)${R}"
  fi
  printf "\n"

  # PLANNING §2.3 item 3 — pin the server's leaf-cert SHA-256 if the
  # server speaks TLS. The fingerprint endpoint is always reachable; over
  # an HTTPS scheme it forces us through the TLS handshake first, which
  # is exactly the MITM-detection moment we care about.
  #
  # Behavior:
  #   - HTTPS server: fetch /api/tls/fingerprint, persist
  #     {"server","fingerprint","scheme":"https"} into ~/.runai-server.json,
  #     and the hook will refuse to talk to a server whose cert doesn't
  #     match this fingerprint.
  #   - HTTP server: write {"scheme":"http"} (no fingerprint to pin), and
  #     the hook talks to plain HTTP. Documented compromise so loopback /
  #     owner-mode setups don't need TLS to function.
  if [[ "$SERVER_URL" == https://* ]]; then
    warn "fetching server cert fingerprint for pinning"
    FP_HTTP=$(curl -sS -o /tmp/runai-fp-resp.$$ -w '%{http_code}' \
      "$SERVER_URL/api/tls/fingerprint" 2>/dev/null || echo 000)
    if [[ "$FP_HTTP" == "200" ]]; then
      FINGERPRINT=$(python3 -c "
import json, sys
try:
  print(json.load(open('/tmp/runai-fp-resp.$$')).get('fingerprint', ''))
except Exception:
  print('')" 2>/dev/null || true)
      if [[ -z "$FINGERPRINT" ]]; then
        fail "server returned 200 but no fingerprint field — refusing to pin a blank"
        rm -f /tmp/runai-fp-resp.$$
        exit 1
      fi
      python3 -c "
import json
out = {'version': 1, 'server': '$SERVER_URL', 'scheme': 'https', 'fingerprint': '$FINGERPRINT'}
print(json.dumps(out, indent=2))" > "$SERVER_PIN_PATH"
      chmod 600 "$SERVER_PIN_PATH"
      ok "pinned server fingerprint ${D}(${FINGERPRINT:0:16}…)${R}"
    else
      fail "could not fetch /api/tls/fingerprint (HTTP $FP_HTTP); aborting before any client config goes out"
      rm -f /tmp/runai-fp-resp.$$
      exit 1
    fi
    rm -f /tmp/runai-fp-resp.$$
  else
    # HTTP server — no fingerprint to pin. Record the scheme so the hook
    # knows not to enforce pinning. PLANNING §2.3 item 2 only mandates
    # TLS for team + non-loopback; owner / loopback can still legitimately
    # speak HTTP.
    python3 -c "
import json
out = {'version': 1, 'server': '$SERVER_URL', 'scheme': 'http', 'fingerprint': None}
print(json.dumps(out, indent=2))" > "$SERVER_PIN_PATH"
    chmod 600 "$SERVER_PIN_PATH"
    warn "server is HTTP — no fingerprint pinning possible"
  fi
  printf "\n"
fi

if [[ "$DO_HOOK" -eq 1 ]]; then
  # --hook-only safety: refuse to write a hook if there's no identity to
  # back it. Without an api_key the hook would silently drop requests
  # into the legacy unauth compat lane, which is not what --hook-only
  # callers want.
  if [[ "$DO_AUTH" -eq 0 && ! -f "$IDENTITY_PATH" ]]; then
    fail "--hook-only requires an existing $IDENTITY_PATH; run without --hook-only first"
    exit 1
  fi
  step "2/3" "install hook wrapper"
  # Reads api_key from ~/.runai-identity at
  #    runtime so the hook keeps working even if the identity gets rotated.
  #    Falls back gracefully when the file is missing or unreadable —
  #    server still gets X-Runai-User for legacy session-prefix display.
  cat > "$HOOK_PATH" <<'HOOK'
#!/usr/bin/env bash
# Auto-generated by runai-client-install. Edit at your own risk; running
# the installer again overwrites this file.
#
# PLANNING.md §2.3 item 3 — server fingerprint pinning.
#
# This wrapper reads ~/.runai-server.json on every invocation and:
#   - if scheme=https + fingerprint set → curl with `--cacert /dev/null` is
#     no good (the cert is self-signed), so we curl normally but ALSO
#     fetch /api/tls/fingerprint over the wire and refuse to send the
#     prompt body if it doesn't match the pinned value. The fingerprint
#     check is the actual MITM gate; the TLS handshake is just what makes
#     an attacker have to forge a matching cert in the first place.
#   - if scheme=http → no pinning is possible; behave like the legacy hook.
RUNAI_SERVER="__SERVER_URL__"
RUNAI_IDENTITY="$HOME/.runai-identity"
RUNAI_SERVER_PIN="$HOME/.runai-server.json"

# Best-effort: extract api_key from ~/.runai-identity. We don't fail the
# hook on a missing file — anonymous calls still work in compat mode.
RUNAI_API_KEY=""
if [[ -r "$RUNAI_IDENTITY" ]]; then
  RUNAI_API_KEY=$(python3 -c "
import json, sys
try:
    with open('$RUNAI_IDENTITY') as f:
        print(json.load(f).get('api_key', ''))
except Exception:
    print('')
" 2>/dev/null || true)
fi

AUTH_HEADER=()
if [[ -n "$RUNAI_API_KEY" ]]; then
  AUTH_HEADER=(-H "Authorization: Bearer $RUNAI_API_KEY")
fi

# Read the pinned fingerprint (if any). Empty FP_PIN → HTTP scheme or no
# pin file → skip pinning. Non-empty FP_PIN → verify server cert matches.
FP_PIN=""
PIN_SCHEME=""
if [[ -r "$RUNAI_SERVER_PIN" ]]; then
  read -r FP_PIN PIN_SCHEME < <(python3 -c "
import json, sys
try:
    d = json.load(open('$RUNAI_SERVER_PIN'))
    print(d.get('fingerprint') or '', d.get('scheme') or '')
except Exception:
    print('', '')
" 2>/dev/null || true)
fi

# Pinning gate: only enforce when both scheme=https AND a fingerprint
# was written at install time. Failure here is fatal — exit non-zero so
# Claude Code surfaces the failure instead of silently routing the user's
# prompt through whatever cert a MITM presented.
if [[ "$PIN_SCHEME" == "https" && -n "$FP_PIN" ]]; then
  FP_LIVE=$(curl -fsS --max-time 5 "$RUNAI_SERVER/api/tls/fingerprint" 2>/dev/null \
    | python3 -c "
import json, sys
try:
    print(json.load(sys.stdin).get('fingerprint',''))
except Exception:
    print('')" 2>/dev/null || echo "")
  if [[ -z "$FP_LIVE" ]]; then
    echo "runai-hook: could not retrieve live server fingerprint from $RUNAI_SERVER — refusing to forward prompt" >&2
    exit 1
  fi
  if [[ "$FP_LIVE" != "$FP_PIN" ]]; then
    echo "runai-hook: server fingerprint mismatch — refusing to forward prompt" >&2
    echo "  pinned: $FP_PIN" >&2
    echo "  live  : $FP_LIVE" >&2
    echo "  if you rotated the server cert, re-run the install script to refresh the pin." >&2
    exit 1
  fi
fi

curl -s --max-time 30 \
  -X POST "$RUNAI_SERVER/recommend" \
  -H "Content-Type: application/json" \
  -H "X-Runai-User: $USER@$(hostname -s)" \
  "${AUTH_HEADER[@]}" \
  --data-binary @- \
  2>/dev/null || true
HOOK
  # Substitute the server URL into the hook (separate sed step so the heredoc
  # above can stay literal and not require manual escaping of $RUNAI_SERVER).
  sed -i.bak "s|__SERVER_URL__|$SERVER_URL|" "$HOOK_PATH" && rm -f "$HOOK_PATH.bak"
  chmod +x "$HOOK_PATH"
  ok "wrote ${D}${HOOK_PATH}${R}"
  printf "\n"
fi

if [[ "$DO_SETTINGS" -eq 1 ]]; then
  step "3/3" "patch Claude Code settings"
  mkdir -p "$(dirname "$SETTINGS_PATH")"
  if [[ -f "$SETTINGS_PATH" ]]; then
    cp "$SETTINGS_PATH" "${SETTINGS_PATH}.runai-bak"
    ok "backed up to ${D}${SETTINGS_PATH}.runai-bak${R}"
  else
    echo '{}' > "$SETTINGS_PATH"
    warn "no settings.json found — created empty one"
  fi

  PATCH_RESULT=$(python3 - "$SETTINGS_PATH" "$HOOK_PATH" <<'PY'
import json
import sys

settings_path = sys.argv[1]
hook_path = sys.argv[2]
hook_cmd = hook_path

with open(settings_path) as f:
    try:
        data = json.load(f)
    except json.JSONDecodeError:
        data = {}

hooks = data.setdefault('hooks', {})
ups = hooks.setdefault('UserPromptSubmit', [])

already = False
for group in ups:
    for h in group.get('hooks', []):
        if h.get('command') == hook_cmd:
            already = True
            break
    if already:
        break

if not already:
    ups.append({'hooks': [{'type': 'command', 'command': hook_cmd}]})

with open(settings_path, 'w') as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
    f.write('\n')

print('__RUNAI_PATCHED__' if not already else '__RUNAI_NOOP__')
PY
  )
  if echo "$PATCH_RESULT" | grep -q '__RUNAI_PATCHED__'; then
    ok "patched UserPromptSubmit hook"
  elif echo "$PATCH_RESULT" | grep -q '__RUNAI_NOOP__'; then
    ok "hook already present (no-op)"
  else
    echo "$PATCH_RESULT"
  fi
  printf "\n"
fi

# §1.4 — install the `runai-client` companion command (bash) at
# ~/.local/bin/runai-client. Subcommands: upload / list / install / --help.
# Idempotent: overwrites any previous copy at the same path. Designed to
# work with or without fzf — if fzf is missing the upload subcommand
# falls back to non-interactive `--path / --name` mode and prints an
# install hint for the user's package manager.
RUNAI_CLIENT_PATH="${RUNAI_CLIENT_PATH:-$HOME/.local/bin/runai-client}"
mkdir -p "$(dirname "$RUNAI_CLIENT_PATH")"
cat > "$RUNAI_CLIENT_PATH" <<'CLIENT'
#!/usr/bin/env bash
# runai-client — companion command for remote runai users (no binary
# installed locally; everything is curl + tar). Installed by the
# runai-client install script (PLANNING.md §1.4).
#
# Subcommands:
#   upload    Pack a skill directory into tar.gz and POST to
#             /api/community/upload. TUI mode (fzf detected) scans
#             ~/.claude/skills/ + $(pwd)/.claude/skills/ for candidates;
#             CLI mode (--path <dir> --name <name>) is non-interactive.
#   list      GET /api/community/list, render as a table.
#   install   POST /api/community/install/<uid>/<name>, copies the skill
#             to the caller's private pool on the server.
#   --help    Print this help and exit.
#
# All subcommands read SERVER_URL + api_key from ~/.runai-identity (or
# the RUNAI_SERVER / RUNAI_API_KEY env vars). Exit non-zero on failure.

set -euo pipefail

IDENTITY_PATH="${RUNAI_IDENTITY:-$HOME/.runai-identity}"

resolve_server() {
  if [[ -n "${RUNAI_SERVER:-}" ]]; then
    printf '%s' "$RUNAI_SERVER"
    return
  fi
  if [[ -r "$IDENTITY_PATH" ]]; then
    python3 -c "
import json, sys
try:
    print(json.load(open('$IDENTITY_PATH')).get('server') or '')
except Exception:
    print('')
" 2>/dev/null || true
    return
  fi
  printf ''
}

resolve_api_key() {
  if [[ -n "${RUNAI_API_KEY:-}" ]]; then
    printf '%s' "$RUNAI_API_KEY"
    return
  fi
  if [[ -r "$IDENTITY_PATH" ]]; then
    python3 -c "
import json, sys
try:
    print(json.load(open('$IDENTITY_PATH')).get('api_key') or '')
except Exception:
    print('')
" 2>/dev/null || true
    return
  fi
  printf ''
}

usage_root() {
  cat <<'HELP'
runai-client — remote companion command (bash) for the runai server.

Usage:
  runai-client <subcommand> [options]

Subcommands:
  upload      Pack a skill dir to tar.gz, POST to /api/community/upload.
              Defaults to TUI selection (fzf required); use --path / --name
              for non-interactive mode.
  list        List skills currently on the server's community pool.
  install     Install a community skill into the server's private copy
              of your account: runai-client install <uploader_uid> <name>
  --help, -h  Print this help and exit.

Environment:
  RUNAI_SERVER     Server base URL (overrides ~/.runai-identity).
  RUNAI_API_KEY    Bearer key (overrides ~/.runai-identity).
  RUNAI_IDENTITY   Path to identity file (default ~/.runai-identity).

Examples:
  runai-client upload
  runai-client upload --path ./my-skill --name my-skill
  runai-client list
  runai-client install u_abc123 my-skill

Run "runai-client <subcommand> --help" for subcommand-specific options.
HELP
}

usage_upload() {
  cat <<'HELP'
runai-client upload — pack & upload a skill directory.

Usage:
  runai-client upload                          # TUI (fzf) selection
  runai-client upload --path <dir> --name <n>  # non-interactive

TUI mode scans these roots for candidate skill directories:
  ~/.claude/skills/
  $(pwd)/.claude/skills/

A "candidate" is any direct subdirectory that contains a SKILL.md file.
The selected directory is packed via `tar -czf` and POSTed as multipart
`bundle` to /api/community/upload with the form field `name=<n>`.

Options:
  --path <dir>   Skill directory to upload (must contain SKILL.md).
  --name <name>  Override the skill name (default = basename of --path).
  --help, -h     Print this help and exit.
HELP
}

ensure_creds() {
  SERVER="$(resolve_server)"
  API_KEY="$(resolve_api_key)"
  if [[ -z "$SERVER" ]]; then
    echo "runai-client: no server URL — set RUNAI_SERVER or run install script first" >&2
    exit 1
  fi
}

cmd_list() {
  local SORT="installs"
  local OFFSET=0
  local LIMIT=50
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --sort) SORT="$2"; shift 2 ;;
      --offset) OFFSET="$2"; shift 2 ;;
      --limit) LIMIT="$2"; shift 2 ;;
      --help|-h)
        echo "runai-client list [--sort installs|created|name] [--offset N] [--limit N]"
        return 0
        ;;
      *)
        echo "runai-client list: unknown arg: $1" >&2; return 1
        ;;
    esac
  done
  ensure_creds
  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  local URL="$SERVER/api/community/list?sort=$SORT&offset=$OFFSET&limit=$LIMIT"
  local RESP
  RESP=$(curl -sS -w '\n__HTTP_CODE__%{http_code}' "${AUTH[@]}" "$URL")
  local HTTP="${RESP##*__HTTP_CODE__}"
  local BODY="${RESP%__HTTP_CODE__*}"
  if [[ "$HTTP" != "200" ]]; then
    echo "runai-client list: server returned HTTP $HTTP" >&2
    echo "$BODY" >&2
    return 1
  fi
  python3 - "$BODY" <<'PY'
import json, sys
data = json.loads(sys.argv[1])
items = data.get('items', [])
total = data.get('total', 0)
print(f"{'NAME':<28} {'UPLOADER':<14} {'VERSION':<14} {'INSTALLS':>8}")
print('-' * 70)
for s in items:
    name = (s.get('name') or '')[:26]
    uid  = (s.get('uploader_uid') or '')[:12]
    ver  = (s.get('version') or '')[:12]
    ins  = s.get('installs_total', 0)
    print(f"{name:<28} {uid:<14} {ver:<14} {ins:>8}")
print('-' * 70)
print(f"total: {total}  offset: {data.get('offset', 0)}  limit: {data.get('limit', 0)}")
PY
}

cmd_install() {
  local UID_ARG=""
  local NAME_ARG=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --help|-h)
        echo "runai-client install <uploader_uid> <name>"
        return 0
        ;;
      *)
        if [[ -z "$UID_ARG" ]]; then UID_ARG="$1"
        elif [[ -z "$NAME_ARG" ]]; then NAME_ARG="$1"
        else echo "runai-client install: extra arg: $1" >&2; return 1
        fi
        shift
        ;;
    esac
  done
  if [[ -z "$UID_ARG" || -z "$NAME_ARG" ]]; then
    echo "runai-client install: missing uploader_uid or name" >&2
    echo "usage: runai-client install <uploader_uid> <name>" >&2
    return 1
  fi
  ensure_creds
  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  local URL="$SERVER/api/community/install/$UID_ARG/$NAME_ARG"
  curl -sS -X POST "${AUTH[@]}" "$URL"
  echo
}

# Scan candidate dirs (TUI mode). Prints one absolute path per line.
scan_candidates() {
  local roots=()
  [[ -d "$HOME/.claude/skills" ]] && roots+=("$HOME/.claude/skills")
  if [[ -d "$(pwd)/.claude/skills" ]]; then roots+=("$(pwd)/.claude/skills"); fi
  for root in "${roots[@]}"; do
    while IFS= read -r d; do
      if [[ -f "$d/SKILL.md" ]]; then
        printf '%s\n' "$d"
      fi
    done < <(find "$root" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
  done
}

cmd_upload() {
  local PATH_ARG=""
  local NAME_ARG=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --path) PATH_ARG="$2"; shift 2 ;;
      --name) NAME_ARG="$2"; shift 2 ;;
      --help|-h) usage_upload; return 0 ;;
      *)
        echo "runai-client upload: unknown arg: $1" >&2
        echo "run 'runai-client upload --help' for usage" >&2
        return 1
        ;;
    esac
  done

  ensure_creds

  if [[ -z "$PATH_ARG" ]]; then
    # TUI path. Require fzf or print install hint + fallback message.
    if ! command -v fzf >/dev/null 2>&1; then
      cat >&2 <<'EOF'
runai-client upload: fzf not installed — needed for interactive selection.

Install fzf:
  macOS:   brew install fzf
  Debian:  sudo apt install fzf
  Fedora:  sudo dnf install fzf
  Arch:    sudo pacman -S fzf

Or skip fzf entirely and use non-interactive mode:
  runai-client upload --path <skill-dir> --name <skill-name>
EOF
      return 1
    fi
    local CANDIDATES
    CANDIDATES=$(scan_candidates)
    if [[ -z "$CANDIDATES" ]]; then
      echo "runai-client upload: no candidate skills found under ~/.claude/skills/ or $(pwd)/.claude/skills/" >&2
      echo "  pass --path <dir> to upload a specific directory instead" >&2
      return 1
    fi
    PATH_ARG=$(printf '%s' "$CANDIDATES" | fzf --prompt='select skill dir > ' --height=40%)
    if [[ -z "$PATH_ARG" ]]; then
      echo "runai-client upload: nothing selected" >&2
      return 1
    fi
  fi

  if [[ ! -d "$PATH_ARG" ]]; then
    echo "runai-client upload: --path is not a directory: $PATH_ARG" >&2
    return 1
  fi
  if [[ ! -f "$PATH_ARG/SKILL.md" ]]; then
    echo "runai-client upload: $PATH_ARG does not contain SKILL.md" >&2
    return 1
  fi

  if [[ -z "$NAME_ARG" ]]; then
    NAME_ARG=$(basename "$PATH_ARG")
  fi

  local TMPDIR
  TMPDIR=$(mktemp -d)
  local TARFILE="$TMPDIR/$NAME_ARG.tar.gz"
  tar -czf "$TARFILE" -C "$(dirname "$PATH_ARG")" "$(basename "$PATH_ARG")"

  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  local URL="$SERVER/api/community/upload"
  echo "uploading $PATH_ARG ($(wc -c <"$TARFILE" | tr -d ' ') bytes) as name=$NAME_ARG"
  local RESP
  RESP=$(curl -sS -w '\n__HTTP_CODE__%{http_code}' \
    -X POST "${AUTH[@]}" \
    -F "name=$NAME_ARG" \
    -F "bundle=@$TARFILE" \
    "$URL")
  rm -rf "$TMPDIR"
  local HTTP="${RESP##*__HTTP_CODE__}"
  local BODY="${RESP%__HTTP_CODE__*}"
  if [[ "$HTTP" != "200" ]]; then
    echo "runai-client upload: server returned HTTP $HTTP" >&2
    echo "$BODY" >&2
    return 1
  fi
  echo "$BODY"
  echo
  echo "OK uploaded."
}

case "${1:-}" in
  ""|--help|-h|help)
    usage_root
    exit 0
    ;;
  upload)
    shift
    cmd_upload "$@"
    ;;
  list)
    shift
    cmd_list "$@"
    ;;
  install)
    shift
    cmd_install "$@"
    ;;
  *)
    echo "runai-client: unknown subcommand: $1" >&2
    usage_root >&2
    exit 1
    ;;
esac
CLIENT
chmod +x "$RUNAI_CLIENT_PATH"
ok "installed ${D}${RUNAI_CLIENT_PATH}${R}"
printf "${D}  add it to PATH if needed:${R} export PATH=\"\$HOME/.local/bin:\$PATH\"\n\n"

# === RUNAI_SECTION:team-only END ===

hr
printf "  ${C2}${B}all set.${R}  open a ${B}new${R} Claude Code session and your prompts\n"
printf "  will route through ${C1}%s${R}\n" "$SERVER_URL"
hr
printf "${D}  dashboard${R}  %s\n" "$SERVER_URL"
printf "${D}  uninstall${R}  curl -fsSL %s/uninstall | bash\n" "$SERVER_URL"
printf "${D}  switch user${R}  rm %s && rerun this script\n\n" "$IDENTITY_PATH"

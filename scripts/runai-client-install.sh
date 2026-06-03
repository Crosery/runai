#!/usr/bin/env bash
# runai client install — registers (or logs into) a runai account on the
# remote server, persists the resulting api_key to ~/.runai-identity, and
# wires a Claude Code UserPromptSubmit hook that POSTs every prompt to
# the server with Authorization: Bearer <api_key>.
#
# Usage:
#   curl -fsSL http://<SERVER>:<PORT>/install | bash
#
# What this does:
#   1) Interactively asks for a username + password.
#   2) Tries POST /auth/login first; on 401, falls back to POST /users/register.
#      Writes the returned api_key to ~/.runai-identity (mode 600).
#   3) Writes ~/.runai-hook.sh — a bash wrapper that curls /recommend with
#      Authorization: Bearer <key from ~/.runai-identity>.
#   4) Patches ~/.claude/settings.json to call that script on UserPromptSubmit.
#      Original settings.json is backed up to .runai-bak first.
#   5) Prints how to uninstall.
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
HOOK_PATH="$HOME/.runai-hook.sh"
SETTINGS_PATH="$HOME/.claude/settings.json"

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

# 1) Account setup. Prompt for credentials, try login, fall back to register.
#    Curl-driven so this works on minimal systems without python3 at this stage.
#    (python3 is only needed below for the JSON patch step.)
#
#    If ~/.runai-identity already exists with a valid key, skip the prompt
#    entirely — re-running the installer should not force a re-login.
step "1/4" "account setup"
if [[ -f "$IDENTITY_PATH" ]] && grep -q '"api_key"' "$IDENTITY_PATH" 2>/dev/null; then
  ok "found existing identity, reusing stored api_key"
  printf "  ${D}(rm %s to switch user)${R}\n\n" "$IDENTITY_PATH"
else
  printf "  ${D}new device — register or sign in to %s${R}\n" "$SERVER_URL"
  printf "  ${B}username${R}  "
  read -r RUNAI_USERNAME
  if [[ -z "${RUNAI_USERNAME// /}" ]]; then
    fail "username cannot be empty"
    exit 1
  fi
  printf "  ${B}password${R}  "
  read -rs RUNAI_PASSWORD
  printf "\n"

  if [[ -z "$RUNAI_PASSWORD" ]]; then
    fail "password cannot be empty"
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

step "2/4" "install hook wrapper"
# Reads api_key from ~/.runai-identity at
#    runtime so the hook keeps working even if the identity gets rotated.
#    Falls back gracefully when the file is missing or unreadable —
#    server still gets X-Runai-User for legacy session-prefix display.
cat > "$HOOK_PATH" <<'HOOK'
#!/usr/bin/env bash
# Auto-generated by runai-client-install. Edit at your own risk; running
# the installer again overwrites this file.
RUNAI_SERVER="__SERVER_URL__"
RUNAI_IDENTITY="$HOME/.runai-identity"

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

step "3/4" "patch Claude Code settings"
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

step "4/4" "done"
hr
printf "  ${C2}${B}all set.${R}  open a ${B}new${R} Claude Code session and your prompts\n"
printf "  will route through ${C1}%s${R}\n" "$SERVER_URL"
hr
printf "${D}  dashboard${R}  %s\n" "$SERVER_URL"
printf "${D}  uninstall${R}  curl -fsSL %s/uninstall | bash\n" "$SERVER_URL"
printf "${D}  switch user${R}  rm %s && rerun this script\n\n" "$IDENTITY_PATH"

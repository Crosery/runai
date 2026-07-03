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
printf "${C1}┃${R}  ${B}runai${R} ${D}skill router${R}    ${D}客户端安装${R}\n"
hr
printf "${D}  server   ${R}%s\n" "$SERVER_URL"
printf "${D}  identity ${R}%s\n" "$IDENTITY_PATH"
printf "${D}  hook     ${R}%s\n" "$HOOK_PATH"
printf "${D}  config   ${R}%s\n" "$SETTINGS_PATH"
hr
printf "\n"

# Disclosure — what this script will actually do, before it asks for a
# password. Users have a right to know what their machine is signing up
# for. Keep concise; long warnings get skipped over.
printf "${C3}┃${R} ${B}本次安装会:${R}\n"
printf "  1. 把用户名 / 密码发到 ${B}%s${R} 注册或登录 — 服务器只存 argon2 哈希\n" "$SERVER_URL"
printf "  2. 把 api_key 写到 ${B}%s${R} (mode 600,${C4}内含明文密钥不要分享${R})\n" "$IDENTITY_PATH"
printf "  3. 装 hook 脚本 ${B}%s${R} + 修改 Claude Code 的 ${B}%s${R}\n" "$HOOK_PATH" "$SETTINGS_PATH"
printf "  4. 已有 ${B}%s${R} 直接复用,不会重复注册\n" "$IDENTITY_PATH"
printf "\n"
printf "${D}  Ctrl-C 随时退出。卸载:curl -fsSL %s/uninstall | bash${R}\n" "$SERVER_URL"
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
  step "1/3" "账号注册 / 登录"
  if [[ -f "$IDENTITY_PATH" ]] && grep -q '"api_key"' "$IDENTITY_PATH" 2>/dev/null; then
    EXISTING_KEY=$(python3 - "$IDENTITY_PATH" <<'PY'
import json, sys
try:
    print(json.load(open(sys.argv[1])).get("api_key") or "")
except Exception:
    print("")
PY
    )
    if [[ -n "$EXISTING_KEY" ]]; then
      VERIFY_HTTP=$(curl -s -o /tmp/runai-me-resp.$$ -w '%{http_code}' \
        -H "Authorization: Bearer $EXISTING_KEY" \
        "$SERVER_URL/api/me" || echo 000)
      rm -f /tmp/runai-me-resp.$$
      if [[ "$VERIFY_HTTP" == "200" ]]; then
        ok "复用已有 ${B}$IDENTITY_PATH${R} 里的 api_key"
        printf "  ${D}(想换账号 → rm %s 后重跑)${R}\n\n" "$IDENTITY_PATH"
      else
        warn "已有 identity 已失效 (HTTP $VERIFY_HTTP),重新登录刷新 api_key"
        rm -f "$IDENTITY_PATH"
      fi
    else
      warn "已有 identity 缺少 api_key,重新登录刷新"
      rm -f "$IDENTITY_PATH"
    fi
  fi
  if [[ ! -f "$IDENTITY_PATH" ]]; then
    printf "  ${D}新机器 — 注册或登录到 %s${R}\n" "$SERVER_URL"

    # Early TTY guard: if both username + password env vars are unset AND
    # stdin is a pipe (the classic `curl ... | bash` case), `read` will
    # immediately return an empty string and the run will fail with the
    # cryptic "username cannot be empty" message. Surface the real cause
    # instead so the user knows how to fix it.
    if [[ -z "${RUNAI_USERNAME:-}" || -z "${RUNAI_PASSWORD:-}" ]] && [[ ! -t 0 ]]; then
      fail "stdin 是 pipe,拿不到键盘 — 没法 prompt 用户名 / 密码"
      printf "  ${D}换成下面任一种形式重跑:${R}\n\n"
      printf "    ${B}bash <(curl -fsSL %s/install)${R}        # 交互模式,会 prompt\n" "$SERVER_URL"
      printf "    ${B}RUNAI_USERNAME=x RUNAI_PASSWORD=y curl -fsSL %s/install | bash${R}  # 非交互\n\n" "$SERVER_URL"
      exit 1
    fi

    # Username: env first, then TTY prompt. Env wins so RUNAI_USERNAME=x
    # works in CI / agent contexts where there is no terminal.
    if [[ -z "${RUNAI_USERNAME:-}" ]]; then
      printf "  ${B}用户名${R}  "
      read -r RUNAI_USERNAME
    else
      ok "用 RUNAI_USERNAME 环境变量"
    fi
    if [[ -z "${RUNAI_USERNAME// /}" ]]; then
      fail "用户名不能为空(设 RUNAI_USERNAME 或在 prompt 里输入)"
      exit 1
    fi

    # Password: same dual-mode resolution. We read char-by-char so the
    # user gets a `*` echo per keystroke (`read -rs` is fully silent and
    # makes it impossible to tell whether a keypress registered, which
    # was the most common cause of "password too short" failures).
    if [[ -z "${RUNAI_PASSWORD:-}" ]]; then
      printf "  ${B}密码${R}    "
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
      ok "用 RUNAI_PASSWORD 环境变量"
    fi
    if [[ -z "$RUNAI_PASSWORD" ]]; then
      fail "密码不能为空(设 RUNAI_PASSWORD 或在 prompt 里输入)"
      exit 1
    fi

    AUTH_BODY=$(printf '{"username":"%s","password":"%s"}' \
      "$(printf '%s' "$RUNAI_USERNAME" | python3 -c 'import json,sys;print(json.dumps(sys.stdin.read())[1:-1])')" \
      "$(printf '%s' "$RUNAI_PASSWORD" | python3 -c 'import json,sys;print(json.dumps(sys.stdin.read())[1:-1])')")
    # rotate_api_key: this installer persists the key to ~/.runai-identity, so
    # it opts into rotation. A plain (dashboard) login never rotates and gets
    # no api_key back — without this flag the extract step below would fail.
    LOGIN_BODY="${AUTH_BODY%\}},\"rotate_api_key\":true}"

    # Try login first. Any HTTP 2xx → logged in. Otherwise fall through to register.
    warn "用 ${B}${RUNAI_USERNAME}${R} 登录中"
    LOGIN_HTTP=$(curl -s -o /tmp/runai-login-resp.$$ -w '%{http_code}' \
      -X POST "$SERVER_URL/auth/login" \
      -H 'Content-Type: application/json' \
      -d "$LOGIN_BODY" || echo 000)
    if [[ "$LOGIN_HTTP" == "200" ]]; then
      ok "登录成功 — ${B}${RUNAI_USERNAME}${R}"
      RESP_FILE=/tmp/runai-login-resp.$$
    elif [[ "$LOGIN_ONLY" -eq 1 ]]; then
      fail "登录失败 (HTTP $LOGIN_HTTP),且 --login-only 已开,不自动注册"
      if [[ "$LOGIN_HTTP" == "401" ]]; then
        warn "忘记密码请联系 server 管理员用 runai admin reset-password 重置"
      fi
      rm -f /tmp/runai-login-resp.$$
      exit 1
    else
      warn "账号不存在,改用注册流程"
      REG_HTTP=$(curl -s -o /tmp/runai-reg-resp.$$ -w '%{http_code}' \
        -X POST "$SERVER_URL/users/register" \
        -H 'Content-Type: application/json' \
        -d "$AUTH_BODY" || echo 000)
      if [[ "$REG_HTTP" == "201" || "$REG_HTTP" == "200" ]]; then
        ok "注册成功 — ${B}${RUNAI_USERNAME}${R}"
        RESP_FILE=/tmp/runai-reg-resp.$$
      else
        fail "登录 + 注册都失败 (login=$LOGIN_HTTP register=$REG_HTTP)"
        if [[ "$LOGIN_HTTP" == "401" ]]; then
          warn "忘记密码请联系 server 管理员用 runai admin reset-password 重置"
        fi
        printf "  ${D}server 返回:${R} " >&2
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
    ok "已写入 ${D}${IDENTITY_PATH}${R} ${D}(mode 600)${R}"
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
    warn "拉 server TLS 指纹做 pinning"
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
        fail "server 返回 200 但没 fingerprint 字段 — 不能 pin 空值"
        rm -f /tmp/runai-fp-resp.$$
        exit 1
      fi
      python3 -c "
import json
out = {'version': 1, 'server': '$SERVER_URL', 'scheme': 'https', 'fingerprint': '$FINGERPRINT'}
print(json.dumps(out, indent=2))" > "$SERVER_PIN_PATH"
      chmod 600 "$SERVER_PIN_PATH"
      ok "已 pin server 指纹 ${D}(${FINGERPRINT:0:16}…)${R}"
    else
      fail "拉 /api/tls/fingerprint 失败 (HTTP $FP_HTTP);中断,客户端不写任何配置"
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
    warn "server 走 HTTP — 没法做 TLS 指纹 pinning"
  fi
  printf "\n"
fi

if [[ "$DO_HOOK" -eq 1 ]]; then
  # --hook-only safety: refuse to write a hook if there's no identity to
  # back it. Without an api_key the hook would silently drop requests
  # into the legacy unauth compat lane, which is not what --hook-only
  # callers want.
  if [[ "$DO_AUTH" -eq 0 && ! -f "$IDENTITY_PATH" ]]; then
    fail "--hook-only 需要先有 $IDENTITY_PATH;请先不带 --hook-only 跑一次"
    exit 1
  fi
  step "2/3" "装 hook 脚本"
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
  ok "已写入 ${D}${HOOK_PATH}${R}"
  printf "\n"
fi

if [[ "$DO_SETTINGS" -eq 1 ]]; then
  step "3/3" "改 Claude Code settings.json"
  mkdir -p "$(dirname "$SETTINGS_PATH")"
  if [[ -f "$SETTINGS_PATH" ]]; then
    cp "$SETTINGS_PATH" "${SETTINGS_PATH}.runai-bak"
    ok "已备份到 ${D}${SETTINGS_PATH}.runai-bak${R}"
  else
    echo '{}' > "$SETTINGS_PATH"
    warn "没找到 settings.json — 新建一个空的"
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
    ok "已挂上 UserPromptSubmit hook"
  elif echo "$PATCH_RESULT" | grep -q '__RUNAI_NOOP__'; then
    ok "hook 已经在了 — 跳过"
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

# Lightweight stderr printers (the install script's ok/warn/die live
# outside this heredoc; the companion needs its own).
warn() { echo "runai-client: $*" >&2; }
die() { echo "runai-client: $*" >&2; exit 1; }

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
  upload         Pack a skill dir to tar.gz, POST to the server's private
                 upload endpoint. Defaults to TUI multi-select (fzf
                 required); use --path / --name for non-interactive mode.
                 PLANNING §1.4 rewrite: lands in YOUR private pool with
                 publish_status='draft'; admin must approve before
                 community visibility.
  list-mine      List skills you've uploaded + their workflow state:
                   uploaded_at / enrich_status / publish_status.
  publish        Request publication of your draft skill to community:
                   runai-client publish <skill-name>
                 Pre-condition: enrich must be 'done'.
  list           List skills currently on the server's community pool.
  install        Install a community skill into the server's private copy
                 of your account: runai-client install <uploader_uid> <name>
  activate       Fetch a skill's SKILL.md + record a usage event
                 (idempotent / outbox-backed). Prints SKILL.md to stdout.
                   runai-client activate <skill> [--session-id <id>]
                 Cache lands at ~/.runai/client-cache/servers/<server-key>/
                 skills/<skill-key>/ (NEVER ~/.runai/skills/). Network failures queue the usage
                 event in a durable local outbox; `flush` replays it.
  feedback       Send a passive feedback note for a skill.
                   runai-client feedback <skill> --note "<text>"
                 Idempotent + outbox-backed (same protocol as activate).
  sync           Prewarm the cache for one or more skills without
                 printing. --all fetches full bundles.
  flush          Replay every queued outbox event in mtime order.
  --help, -h     Print this help and exit.

Environment:
  RUNAI_SERVER     Server base URL (overrides ~/.runai-identity).
  RUNAI_API_KEY    Bearer key (overrides ~/.runai-identity).
  RUNAI_IDENTITY   Path to identity file (default ~/.runai-identity).

Examples:
  runai-client upload
  runai-client upload --path ./my-skill --name my-skill
  runai-client list
  runai-client install u_abc123 my-skill
  runai-client activate my-skill --session-id "$CLAUDE_SESSION_ID"
  runai-client feedback my-skill --note "works great for X"
  runai-client sync my-skill other-skill
  runai-client flush

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

cmd_list_mine() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --help|-h)
        echo "runai-client list-mine — list your uploaded skills + workflow state."
        echo
        echo "Usage:"
        echo "  runai-client list-mine"
        return 0
        ;;
      *)
        echo "runai-client list-mine: 未知参数: $1" >&2
        return 1
        ;;
    esac
  done
  ensure_creds
  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  local RESP
  RESP=$(curl -sS -w '\n__HTTP_CODE__%{http_code}' "${AUTH[@]}" "$SERVER/api/users/me/skills")
  local HTTP="${RESP##*__HTTP_CODE__}"
  local BODY="${RESP%__HTTP_CODE__*}"
  if [[ "$HTTP" != "200" ]]; then
    echo "runai-client list-mine: server 返回 HTTP $HTTP" >&2
    echo "$BODY" >&2
    return 1
  fi
  python3 - "$BODY" <<'PY'
import json, sys, datetime
data = json.loads(sys.argv[1])
items = data.get('items', [])
total = data.get('total', 0)
print(f"{'NAME':<28} {'UPLOADED':<19} {'ENRICH':<8} {'PUBLISH':<10} {'REASON'}")
print('-' * 90)
for s in items:
    name = (s.get('name') or '')[:26]
    ts = s.get('uploaded_at') or 0
    when = datetime.datetime.fromtimestamp(ts).strftime('%Y-%m-%d %H:%M') if ts else '-'
    enrich = s.get('enrich_status') or ''
    pub = s.get('publish_status') or ''
    reason = (s.get('publish_reason') or '')[:30]
    print(f"{name:<28} {when:<19} {enrich:<8} {pub:<10} {reason}")
print('-' * 90)
print(f"total: {total}")
PY
}

cmd_publish() {
  local NAME_ARG=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --help|-h)
        echo "runai-client publish <skill-name>"
        echo
        echo "Request review by the server admin to publish your draft skill"
        echo "to the community pool. Pre-condition: enrich_status='done'."
        return 0
        ;;
      *)
        if [[ -z "$NAME_ARG" ]]; then NAME_ARG="$1"; shift; continue; fi
        echo "runai-client publish: 未知参数: $1" >&2
        return 1
        ;;
    esac
  done
  if [[ -z "$NAME_ARG" ]]; then
    echo "runai-client publish: 需要指定 skill name" >&2
    echo "用法: runai-client publish <skill-name>" >&2
    return 1
  fi
  ensure_creds
  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  local URL="$SERVER/api/users/me/skills/$NAME_ARG/publish-request"
  local RESP
  RESP=$(curl -sS -w '\n__HTTP_CODE__%{http_code}' -X POST "${AUTH[@]}" "$URL")
  local HTTP="${RESP##*__HTTP_CODE__}"
  local BODY="${RESP%__HTTP_CODE__*}"
  if [[ "$HTTP" == "200" ]]; then
    echo "已申请发布 — admin 将审核"
    echo "$BODY"
  elif [[ "$HTTP" == "400" ]]; then
    echo "申请失败:$BODY" >&2
    return 1
  elif [[ "$HTTP" == "404" ]]; then
    echo "找不到该 skill — 用 runai-client list-mine 看自己上传过哪些" >&2
    return 1
  else
    echo "runai-client publish: HTTP $HTTP" >&2
    echo "$BODY" >&2
    return 1
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

# Scan candidate dirs (TUI mode). Prints tab-separated rows so fzf can
# show skill name + description as the user-visible columns, while the
# absolute path stays in column 2 for the upload step. Columns:
#   <name>\t<absolute-path>\t<description-from-SKILL.md-frontmatter>
scan_candidates() {
  local roots=()
  [[ -d "$HOME/.claude/skills" ]] && roots+=("$HOME/.claude/skills")
  if [[ -d "$(pwd)/.claude/skills" ]]; then roots+=("$(pwd)/.claude/skills"); fi
  for root in "${roots[@]}"; do
    while IFS= read -r d; do
      if [[ -f "$d/SKILL.md" ]]; then
        local name desc
        name=$(basename "$d")
        # Pull the `description:` line out of the YAML frontmatter (stops
        # at the closing `---`). Strip surrounding quotes; collapse to
        # one line so it fits the fzf row.
        desc=$(awk '
          BEGIN { delim = 0 }
          /^---[[:space:]]*$/ { delim++; if (delim == 2) exit; next }
          delim == 1 && /^description:/ {
            sub(/^description:[[:space:]]*/, "")
            gsub(/^["'\''](.*)["'\'']$/, "\\1", $0)
            print
            exit
          }
        ' "$d/SKILL.md" 2>/dev/null | tr '\n' ' ' | sed 's/[[:space:]]*$//')
        printf '%s\t%s\t%s\n' "$name" "$d" "${desc:-(无描述)}"
      fi
    done < <(find "$root" -mindepth 1 -maxdepth 1 -type d 2>/dev/null)
  done
}

# Upload ONE skill dir. Used both by --path single-upload and by the
# TUI multi-select loop. Returns 0 on success, 1 on any failure.
do_single_upload() {
  local PATH_ARG="$1"
  local NAME_ARG="$2"
  if [[ ! -d "$PATH_ARG" ]]; then
    echo "  [!!] 路径不是目录:$PATH_ARG" >&2
    return 1
  fi
  if [[ ! -f "$PATH_ARG/SKILL.md" ]]; then
    echo "  [!!] $PATH_ARG 内不含 SKILL.md" >&2
    return 1
  fi
  local TMPDIR
  TMPDIR=$(mktemp -d)
  local TARFILE="$TMPDIR/$NAME_ARG.tar.gz"
  tar -czf "$TARFILE" -C "$(dirname "$PATH_ARG")" "$(basename "$PATH_ARG")"
  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  # PLANNING §1.4 rewrite: default upload target is the caller's private
  # pool (publish_status='draft'); admin can later approve to community.
  local URL="$SERVER/api/users/me/skills/upload"
  local SIZE
  SIZE=$(wc -c <"$TARFILE" | tr -d ' ')
  echo "  打包 $NAME_ARG ($SIZE bytes),上传到 $URL …"
  local RESP HTTP BODY
  RESP=$(curl -sS -w '\n__HTTP_CODE__%{http_code}' \
    -X POST "${AUTH[@]}" \
    -F "name=$NAME_ARG" \
    -F "bundle=@$TARFILE" \
    "$URL")
  rm -rf "$TMPDIR"
  HTTP="${RESP##*__HTTP_CODE__}"
  BODY="${RESP%__HTTP_CODE__*}"
  if [[ "$HTTP" != "200" ]]; then
    echo "  [!!] 服务器返回 HTTP $HTTP" >&2
    echo "  $BODY" >&2
    return 1
  fi
  echo "  $BODY"
  echo "  [OK] 上传成功"
  return 0
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
        echo "runai-client upload: 未知参数: $1" >&2
        echo "运行 'runai-client upload --help' 查看用法" >&2
        return 1
        ;;
    esac
  done

  ensure_creds

  if [[ -n "$PATH_ARG" ]]; then
    # Non-interactive single-upload path.
    [[ -z "$NAME_ARG" ]] && NAME_ARG=$(basename "$PATH_ARG")
    echo "[1/1] $(basename "$PATH_ARG")"
    do_single_upload "$PATH_ARG" "$NAME_ARG"
    return $?
  fi

  # TUI multi-select. fzf required.
  if ! command -v fzf >/dev/null 2>&1; then
    cat >&2 <<'EOF'
runai-client upload: 选 skill 需要 fzf,但未安装。

装 fzf:
  macOS:   brew install fzf
  Debian:  sudo apt install fzf
  Fedora:  sudo dnf install fzf
  Arch:    sudo pacman -S fzf

或者跳过 fzf 直接走非交互模式:
  runai-client upload --path <skill-dir> --name <skill-name>
EOF
    return 1
  fi
  local CANDIDATES
  CANDIDATES=$(scan_candidates)
  if [[ -z "$CANDIDATES" ]]; then
    echo "runai-client upload: 在 ~/.claude/skills/ 和 $(pwd)/.claude/skills/ 都没找到 skill" >&2
    echo "  传 --path <dir> 直接指定 skill 目录" >&2
    return 1
  fi

  # fzf 多选:展示 skill 名 + 描述(列 1 + 3),路径(列 2)藏起来。
  # Tab 切换选中,Enter 确认。--bind 让 ctrl-a 全选 / ctrl-d 全清。
  local SELECTED
  SELECTED=$(printf '%s' "$CANDIDATES" | fzf \
    --multi \
    --delimiter=$'\t' \
    --with-nth='1,3' \
    --prompt='选 skill 上传 > ' \
    --header='Space / Tab 切换选中  ·  Ctrl-A 全选  ·  Ctrl-D 全清  ·  Enter 确认' \
    --pointer='▶' \
    --marker='◉' \
    --bind='space:toggle' \
    --bind='ctrl-a:select-all' \
    --bind='ctrl-d:deselect-all' \
    --height=60% \
    --reverse)
  if [[ -z "$SELECTED" ]]; then
    echo "runai-client upload: 没有选中任何 skill,放弃" >&2
    return 1
  fi

  # 拆出所有选中的路径(列 2)。fzf --multi 用换行分隔多行结果。
  local PATHS=()
  while IFS=$'\t' read -r _name _path _desc; do
    [[ -n "$_path" ]] && PATHS+=("$_path")
  done <<< "$SELECTED"

  local total=${#PATHS[@]}
  echo
  echo "准备上传 $total 个 skill 到 $SERVER:"
  local p
  for p in "${PATHS[@]}"; do
    echo "  - $(basename "$p")"
  done
  echo

  local ok=0 fail=0 i=0
  for p in "${PATHS[@]}"; do
    ((i++))
    local nm
    nm=$(basename "$p")
    echo "[$i/$total] $nm"
    if do_single_upload "$p" "$nm"; then
      ((ok++))
    else
      ((fail++))
    fi
    echo
  done
  echo "完成 — 成功 $ok / 失败 $fail / 共 $total"
  [[ $fail -eq 0 ]]
}

# ─── activation / feedback protocol (PLANNING §1.3) ───────────────────────────
# runai-client activate / feedback / sync / flush + the client-side cache
# and durable outbox. The cache lives at
# ~/.runai/client-cache/servers/<server-key>/skills/<skill-key>/
# (NEVER ~/.runai/skills/ — that path is reserved for the runai binary's
# managed pool and writing here would collide with the owner-pool
# invariant on machines that also run `runai`). The outbox lives inside
# that server-scoped skill cache as `.outbox/<ts>-<event_id>.json` — one
# file per event, replayed in mtime order by `flush`.
#
# Activate contract (铁律): SKILL.md is printed ONLY after a usage event
# has been either ACKed by the server (200 from /skills/use) or durably
# queued in the local outbox. If neither is true (401/403/409/404),
# activate fails without printing. A cold cache + network failure still
# queues the event but cannot print (no content), so it fails too — the
# outbox entry survives for the next `flush`.

client_cache_root() {
  printf '%s' "${HOME}/.runai/client-cache"
}

cache_key() {
  python3 -c 'import hashlib, sys; print(hashlib.sha256(sys.argv[1].encode("utf-8")).hexdigest())' "$1"
}

server_cache_root() {
  local server_key; server_key=$(cache_key "$SERVER")
  printf '%s/servers/%s' "$(client_cache_root)" "$server_key"
}

skill_cache_dir() {
  local skill_key; skill_key=$(cache_key "$1")
  printf '%s/skills/%s' "$(server_cache_root)" "$skill_key"
}

gen_uuid() {
  if command -v uuidgen >/dev/null 2>&1; then
    uuidgen
  elif [[ -r /proc/sys/kernel/random/uuid ]]; then
    cat /proc/sys/kernel/random/uuid
  else
    python3 -c 'import uuid; print(uuid.uuid4())'
  fi
}

# Atomic write: stage to <dest>.tmp.$$ then mv -f. Caller passes the
# final destination path; we derive the tmp sibling.
atomic_write() {
  local dest="$1"
  local tmp="${dest}.tmp.$$"
  cat > "$tmp" || { rm -f "$tmp"; return 1; }
  mv -f "$tmp" "$dest"
}

# GET a single file from /skills/file/{name}/{rel} into <dest> (atomic).
# Rejects traversal in <rel> so `--include ../../etc/passwd` cannot escape.
fetch_file() {
  local name="$1" rel="$2" dest="$3"
  case "$rel" in
    *..*|/*|*\$'\t'*)
      echo "runai-client: refusing traversal include path: $rel" >&2
      return 1
      ;;
  esac
  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  mkdir -p "$(dirname "$dest")"
  local tmp="${dest}.tmp.$$"
  local code
  code=$(curl -sS -m 30 -w '%{http_code}' -o "$tmp" \
    "${AUTH[@]}" "$SERVER/skills/file/$name/$rel" 2>/dev/null) || { rm -f "$tmp"; return 1; }
  if [[ "$code" != "200" ]]; then
    rm -f "$tmp"
    return 1
  fi
  mv -f "$tmp" "$dest"
  chmod 600 "$dest" 2>/dev/null
  return 0
}

# GET /skills/bundle/{name} and extract into <dest_dir>. Extraction is
# staged into <dest_dir>.new.$$ then atomically swapped in so a half-
# extracted tree never replaces a good cache.
fetch_bundle() {
  local name="$1" dest_dir="$2"
  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  mkdir -p "$dest_dir"
  local stage; stage=$(mktemp -d "${dest_dir}.new.XXXXXX")
  local tgz="$stage/bundle.tar.gz"
  local code
  code=$(curl -sS -m 60 -w '%{http_code}' -o "$tgz" \
    "${AUTH[@]}" "$SERVER/skills/bundle/$name" 2>/dev/null) || { rm -rf "$stage"; return 1; }
  if [[ "$code" != "200" ]]; then
    rm -rf "$stage"
    return 1
  fi
  if ! tar -xzf "$tgz" -C "$stage" 2>/dev/null; then
    rm -rf "$stage"
    return 1
  fi
  rm -f "$tgz"
  # tar layout is <name>/...; flatten into stage root so files land at
  # $stage/SKILL.md, $stage/references/... — then swap.
  local inner="$stage/$name"
  if [[ -d "$inner" ]]; then
    mv "$inner"/* "$inner"/.* "$stage/" 2>/dev/null || true
    rmdir "$inner" 2>/dev/null || true
  fi
  rm -rf "${dest_dir:?}"
  mv "$stage" "$dest_dir"
  chmod -R go-rwx "$dest_dir" 2>/dev/null || true
  return 0
}

# Write a durable outbox entry. Atomic + crash-conscious: write a tmp
# sibling, flush+fsync it, rename into place, then fsync the directory when
# the platform allows it. If this function returns 0, activate may print
# cached SKILL.md even when the server is offline.
outbox_write() {
  local kind="$1" event_id="$2" skill="$3" body="$4" session_id="$5" note="${6:-}"
  local outbox; outbox="$(skill_cache_dir "$skill")/.outbox"
  mkdir -p "$outbox"
  local ts; ts=$(date +%s)
  python3 - "$outbox" "$event_id" "$kind" "$skill" "$body" "$session_id" "$note" "$ts" <<'PY'
import json, os, sys
outbox, eid, kind, skill, body, sid, note, ts = sys.argv[1:9]
os.makedirs(outbox, mode=0o700, exist_ok=True)
entry = {
    "event_id": eid,
    "kind": kind,
    "skill": skill,
    "body": body,
    "session_id": sid,
    "note": note,
    "ts": int(ts),
    "attempts": 0,
}
tmp = os.path.join(outbox, f".tmp.{os.getpid()}.{eid}")
final = os.path.join(outbox, f"{ts}-{eid}.json")
try:
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(entry, fh, separators=(",", ":"))
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, final)
    os.chmod(final, 0o600)
    try:
        dfd = os.open(outbox, os.O_RDONLY)
        try:
            os.fsync(dfd)
        finally:
            os.close(dfd)
    except OSError:
        pass
except Exception:
    try:
        os.unlink(tmp)
    except OSError:
        pass
    raise
PY
}

# Replay one outbox entry. Returns 0 if the entry is resolved (deleted),
# 1 if retained for a later retry. Auth failures (401/403) are dropped
# (deleted) — they will never succeed on retry. 5xx / network failures
# increment attempts and keep the file.
outbox_replay() {
  local f="$1"
  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  local event_id kind skill body session_id note attempts
  event_id=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('event_id',''))" "$f")
  kind=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('kind',''))" "$f")
  skill=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('skill',''))" "$f")
  body=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('body',''))" "$f")
  note=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('note',''))" "$f")
  attempts=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1])).get('attempts',0))" "$f")
  local url
  if [[ "$kind" == "usage" ]]; then
    url="$SERVER/skills/use/$skill"
  else
    url="$SERVER/feedback"
  fi
  local code
  code=$(curl -sS -m 15 -o /dev/null -w '%{http_code}' -X POST \
    "${AUTH[@]}" \
    -H "X-Runai-Event-Id: $event_id" \
    -H 'Content-Type: application/json' \
    -d "$body" \
    "$url" 2>/dev/null) || code="000"
  case "$code" in
    200|409)
      # 200 = applied now; 409 = already applied with a different payload
      # (treat as resolved — the event is durably recorded either way).
      rm -f "$f"
      return 0
      ;;
    401|403)
      echo "runai-client flush: dropping $kind event $event_id (auth failure $code)" >&2
      rm -f "$f"
      return 0
      ;;
    422)
      # Malformed — missing event_id we should have set. Drop; never retryable.
      echo "runai-client flush: dropping malformed $kind event $event_id (422)" >&2
      rm -f "$f"
      return 0
      ;;
    000|5*)
      # network / 5xx — increment attempts, keep for next flush.
      python3 - "$f" "$((attempts+1))" <<'PY'
import json, sys
p, n = sys.argv[1], sys.argv[2]
d = json.load(open(p))
d['attempts'] = int(n)
tmp = p + '.tmp'
with open(tmp, 'w') as fh:
    json.dump(d, fh)
import os
os.replace(tmp, p)
PY
      return 1
      ;;
    *)
      echo "runai-client flush: unexpected HTTP $code for $kind event $event_id — retaining" >&2
      return 1
      ;;
  esac
}

usage_activate() {
  cat <<'HELP'
runai-client activate — fetch a skill's SKILL.md and record a usage event.

Usage:
  runai-client activate <skill> [options]

Options:
  --session-id <id>   Claude session id (threaded into the usage event).
  --refresh           Re-fetch SKILL.md even if a warm cache exists.
  --include <relpath> Also fetch this sibling file into the cache (repeatable).
  --all               Fetch the whole skill bundle into the cache.
  --event-id <id>     Override the auto-generated event id (for replay).
  --help, -h          Print this help and exit.

Cache layout (never written to ~/.runai/skills/):
  ~/.runai/client-cache/servers/<server-key>/skills/<skill-key>/SKILL.md
  ~/.runai/client-cache/servers/<server-key>/skills/<skill-key>/files/<relpath>
  ~/.runai/client-cache/servers/<server-key>/skills/<skill-key>/.outbox/<ts>-<event_id>.json

Contract: SKILL.md is printed to stdout ONLY after the usage event is
ACKed by the server OR durably queued in the local outbox. A cold cache
+ network failure queues the event but does NOT print (no content).
HELP
}

usage_feedback() {
  cat <<'HELP'
runai-client feedback — send a passive feedback note for a skill.

Usage:
  runai-client feedback <skill> --note "<text>" [options]

Options:
  --note <text>       The feedback note (required).
  --event-id <id>     Override the auto-generated event id (for replay).
  --help, -h          Print this help and exit.

Network failures queue the event in the local outbox (replayed by
`runai-client flush`). 401/403 do NOT queue — auth failures are never
retryable.
HELP
}

usage_sync() {
  cat <<'HELP'
runai-client sync — prewarm the local cache for one or more skills.

Usage:
  runai-client sync <skill> [<skill> ...]
  runai-client sync --all   (prewarm bundles for every listed skill)

Options:
  --all   Fetch full bundles (not just SKILL.md) into each cache dir.
  --help, -h   Print this help and exit.

Unknown skills are skipped with a warning (exit 0) so a stale list does
not abort a prewarm loop.
HELP
}

cmd_activate() {
  local SKILL="" SESSION_ID="" REFRESH=0 EVENT_ID="" ALL=0
  local -a INCLUDES=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --session-id) SESSION_ID="$2"; shift 2 ;;
      --refresh) REFRESH=1; shift ;;
      --include) INCLUDES+=("$2"); shift 2 ;;
      --all) ALL=1; shift ;;
      --event-id) EVENT_ID="$2"; shift 2 ;;
      --help|-h) usage_activate; return 0 ;;
      *)
        if [[ -z "$SKILL" ]]; then SKILL="$1"; shift; continue; fi
        echo "runai-client activate: 未知参数: $1" >&2
        return 1
        ;;
    esac
  done
  if [[ -z "$SKILL" ]]; then
    echo "runai-client activate: 需要 skill name" >&2
    usage_activate >&2
    return 2
  fi
  ensure_creds
  [[ -z "$EVENT_ID" ]] && EVENT_ID="$(gen_uuid)"

  local CACHE_BASE; CACHE_BASE=$(client_cache_root)
  local CACHE_ROOT; CACHE_ROOT=$(server_cache_root)
  local CACHE_DIR; CACHE_DIR=$(skill_cache_dir "$SKILL")
  local SKILL_MD="$CACHE_DIR/SKILL.md"
  mkdir -p "$CACHE_DIR/files" "$CACHE_DIR/.outbox"
  chmod 700 "$HOME/.runai" "$CACHE_BASE" "$CACHE_BASE/servers" "$CACHE_ROOT" "$CACHE_ROOT/skills" "$CACHE_DIR" 2>/dev/null || true

  # Canonical payload (server re-sorts keys for hashing, so our field
  # order only needs to be self-consistent).
  local includes_str; includes_str=$(printf '%s\n' ${INCLUDES[@]+"${INCLUDES[@]}"})
  local BODY; BODY=$(python3 -c \
    "import json,sys; print(json.dumps({'session_id': sys.argv[1], 'include': [x for x in sys.argv[2].splitlines() if x]}))" \
    "$SESSION_ID" "$includes_str")

  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  local RESP HTTP BODY_RESP
  RESP=$(curl -sS -m 15 -w '\n__HTTP_CODE__%{http_code}' -X POST \
    "${AUTH[@]}" \
    -H "X-Runai-Event-Id: $EVENT_ID" \
    -H 'Content-Type: application/json' \
    -d "$BODY" \
    "$SERVER/skills/use/$SKILL" 2>/dev/null) || RESP=""
  HTTP="${RESP##*__HTTP_CODE__}"
  BODY_RESP="${RESP%__HTTP_CODE__*}"

  case "$HTTP" in
    200) : ;;  # ACKed
    409) echo "runai-client activate: event_id 冲突 (HTTP 409)" >&2; return 1 ;;
    401|403) echo "runai-client activate: 鉴权失败 (HTTP $HTTP)" >&2; return 1 ;;
    404) echo "runai-client activate: server 找不到该 skill" >&2; return 1 ;;
    ""|000|5*)
      # network / 5xx → durable outbox
      outbox_write "usage" "$EVENT_ID" "$SKILL" "$BODY" "$SESSION_ID"
      ;;
    *) echo "runai-client activate: 意外 HTTP $HTTP" >&2; return 1 ;;
  esac

  # Ensure content. Cache hit unless --refresh.
  if [[ $REFRESH -eq 0 && -f "$SKILL_MD" ]]; then
    : # use cache
  else
    if ! fetch_file "$SKILL" "SKILL.md" "$SKILL_MD"; then
      if [[ -f "$SKILL_MD" ]]; then
        warn "拉取失败,使用旧缓存 $SKILL"
      else
        # cold cache + fetch failure. Outbox already has the event if
        # we queued it. Contract: fail WITHOUT printing.
        echo "runai-client activate: 无法拉取 SKILL.md 且无缓存" >&2
        return 1
      fi
    fi
  fi

  # Fetch --include / --all
  if [[ $ALL -eq 1 ]]; then
    fetch_bundle "$SKILL" "$CACHE_DIR/files" || warn "bundle 拉取失败: $SKILL"
  else
    for rel in ${INCLUDES[@]+"${INCLUDES[@]}"}; do
      fetch_file "$SKILL" "$rel" "$CACHE_DIR/files/$rel" || warn "include 拉取失败: $rel"
    done
  fi

  # Contract: ACK or outbox succeeded → safe to print.
  cat "$SKILL_MD"
}

cmd_feedback() {
  local SKILL="" NOTE="" EVENT_ID=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --note) NOTE="$2"; shift 2 ;;
      --event-id) EVENT_ID="$2"; shift 2 ;;
      --help|-h) usage_feedback; return 0 ;;
      *)
        if [[ -z "$SKILL" ]]; then SKILL="$1"; shift; continue; fi
        echo "runai-client feedback: 未知参数: $1" >&2
        return 1
        ;;
    esac
  done
  if [[ -z "$SKILL" ]]; then
    echo "runai-client feedback: 需要 skill name" >&2
    usage_feedback >&2
    return 2
  fi
  if [[ -z "$NOTE" ]]; then
    echo "runai-client feedback: --note required" >&2
    return 2
  fi
  ensure_creds
  [[ -z "$EVENT_ID" ]] && EVENT_ID="$(gen_uuid)"

  local BODY; BODY=$(python3 -c \
    "import json,sys; print(json.dumps({'skill': sys.argv[1], 'note': sys.argv[2]}))" \
    "$SKILL" "$NOTE")

  local AUTH=()
  [[ -n "$API_KEY" ]] && AUTH=(-H "Authorization: Bearer $API_KEY")
  local RESP HTTP
  RESP=$(curl -sS -m 15 -w '\n__HTTP_CODE__%{http_code}' -X POST \
    "${AUTH[@]}" \
    -H "X-Runai-Event-Id: $EVENT_ID" \
    -H 'Content-Type: application/json' \
    -d "$BODY" \
    "$SERVER/feedback" 2>/dev/null) || RESP=""
  HTTP="${RESP##*__HTTP_CODE__}"
  case "$HTTP" in
    200) return 0 ;;
    409) return 0 ;;  # idempotent — already applied with this payload
    401|403) echo "runai-client feedback: 鉴权失败 (HTTP $HTTP)" >&2; return 1 ;;
    ""|000|5*)
      outbox_write "feedback" "$EVENT_ID" "$SKILL" "$BODY" "" "$NOTE"
      return 0
      ;;
    *) echo "runai-client feedback: 意外 HTTP $HTTP" >&2; return 1 ;;
  esac
}

cmd_sync() {
  local ALL=0
  local -a SKILLS=()
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --all) ALL=1; shift ;;
      --help|-h) usage_sync; return 0 ;;
      *) SKILLS+=("$1"); shift ;;
    esac
  done
  if [[ ${#SKILLS[@]} -eq 0 ]]; then
    echo "runai-client sync: 至少指定一个 skill (或用 --all 预暖所有已知 skill)" >&2
    usage_sync >&2
    return 2
  fi
  ensure_creds
  local CACHE_BASE; CACHE_BASE=$(client_cache_root)
  local CACHE_ROOT; CACHE_ROOT=$(server_cache_root)
  chmod 700 "$HOME/.runai" "$CACHE_BASE" "$CACHE_BASE/servers" "$CACHE_ROOT" "$CACHE_ROOT/skills" 2>/dev/null || true
  local ok=0 skip=0
  for s in "${SKILLS[@]}"; do
    local dir; dir=$(skill_cache_dir "$s")
    mkdir -p "$dir/files" "$dir/.outbox"
    chmod 700 "$dir" 2>/dev/null || true
    chmod 700 "$CACHE_ROOT" "$dir" 2>/dev/null || true
    if ! fetch_file "$s" "SKILL.md" "$dir/SKILL.md"; then
      echo "runai-client sync: 跳过 $s (server 返回非 200 或不可达)" >&2
      skip=$((skip+1))
      continue
    fi
    if [[ $ALL -eq 1 ]]; then
      fetch_bundle "$s" "$dir/files" || warn "sync: bundle 拉取失败: $s"
    fi
    ok=$((ok+1))
  done
  echo "sync: 预暖 $ok / 跳过 $skip"
  return 0
}

cmd_flush() {
  ensure_creds
  local CACHE_ROOT; CACHE_ROOT=$(client_cache_root)
  local outbox_files=()
  if [[ -d "$CACHE_ROOT" ]]; then
    # mtime-ordered list of every outbox entry across all skills.
    while IFS= read -r f; do
      [[ -n "$f" ]] && outbox_files+=("$f")
    done < <(find "$CACHE_ROOT" -type f -path '*/.outbox/*.json' 2>/dev/null \
      | xargs -r ls -t 2>/dev/null)
  fi
  if [[ ${#outbox_files[@]} -eq 0 ]]; then
    echo "flush: outbox 为空"
    return 0
  fi
  local ok=0 retain=0
  for f in "${outbox_files[@]}"; do
    if outbox_replay "$f"; then ok=$((ok+1)); else retain=$((retain+1)); fi
  done
  echo "flush: 已重放 $ok / 保留 $retain"
  return 0
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
  list-mine)
    shift
    cmd_list_mine "$@"
    ;;
  publish)
    shift
    cmd_publish "$@"
    ;;
  list)
    shift
    cmd_list "$@"
    ;;
  install)
    shift
    cmd_install "$@"
    ;;
  activate)
    shift
    cmd_activate "$@"
    ;;
  feedback)
    shift
    cmd_feedback "$@"
    ;;
  sync)
    shift
    cmd_sync "$@"
    ;;
  flush)
    shift
    cmd_flush "$@"
    ;;
  *)
    echo "runai-client: unknown subcommand: $1" >&2
    usage_root >&2
    exit 1
    ;;
esac
CLIENT
chmod +x "$RUNAI_CLIENT_PATH"
ok "已装 ${D}${RUNAI_CLIENT_PATH}${R}"
printf "${D}  add it to PATH if needed:${R} export PATH=\"\$HOME/.local/bin:\$PATH\"\n\n"

# === RUNAI_SECTION:team-only END ===

hr
printf "  ${C2}${B}装好了。${R}  打开一个${B}新的${R} Claude Code session,你的 prompt 就会自动\n"
printf "  路由到 ${C1}%s${R}\n" "$SERVER_URL"
hr
printf "${D}  仪表板${R}  %s\n" "$SERVER_URL"
printf "${D}  卸载  ${R}  curl -fsSL %s/uninstall | bash\n" "$SERVER_URL"
printf "${D}  换账号${R}  rm %s 后重跑本脚本\n\n" "$IDENTITY_PATH"

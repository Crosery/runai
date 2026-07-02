# runai client install — Windows / PowerShell.
#
# This file is a TEMPLATE — the server hydrates it per request:
#   - `{SERVER_URL}` is substituted with the URL the request came in on.
#   - Sections wrapped in `# === RUNAI_SECTION:owner-only ... ===` are
#     stripped when the server is in `team` mode.
#   - Sections wrapped in `# === RUNAI_SECTION:team-only ... ===` are
#     stripped when the server is in `owner` mode.
#   - The server returns the assembled script body; in `owner` mode the
#     /install.ps1 endpoint returns 404 instead.
# See src/server/install.rs::render_install_script for the renderer.
#
# Usage:
#   irm http://<SERVER>:<PORT>/install.ps1 | iex
#
# Non-interactive (agent / CI):
#   $env:RUNAI_USERNAME = "alice"
#   $env:RUNAI_PASSWORD = "hunter2"
#   irm http://<SERVER>:<PORT>/install.ps1 | iex
#
# Subcommand flags (set as environment variables since `iex` swallows argv):
#   $env:RUNAI_PHASE = "register-only" | "login-only" | "hook-only"
#   $env:RUNAI_HELP  = "1"   # print help and exit

# {SERVER_URL} is substituted by the server at request time.
$ServerUrl = "{SERVER_URL}"
$IdentityPath = "$env:USERPROFILE\.runai-identity"
$HookPath = "$env:USERPROFILE\.runai-hook.ps1"
$SettingsPath = "$env:USERPROFILE\.claude\settings.json"

# Phase selection. Defaults: run everything.
$DoAuth = $true
$DoHook = $true
$DoSettings = $true
$LoginOnly = $false

function Print-RunaiHelp {
    Write-Host @"
runai client install (Windows / PowerShell) — TTY + non-interactive installer.

Usage:
  irm http://<SERVER>:<PORT>/install.ps1 | iex

Non-interactive:
  `$env:RUNAI_USERNAME = "alice"
  `$env:RUNAI_PASSWORD = "hunter2"
  irm http://<SERVER>:<PORT>/install.ps1 | iex

Environment variables:
  RUNAI_USERNAME   Username for register / login.
  RUNAI_PASSWORD   Password for register / login.
  RUNAI_PHASE      One of:
                     register-only  identity phase only (auto-register on 401)
                     login-only     identity phase only, no auto-register
                     hook-only      skip identity, install hook + settings only
  RUNAI_HELP       Set to 1 to print this message and exit.

Exit codes:
  0  success
  1  missing required input, auth failed, or settings.json patch failed
"@
}

if ($env:RUNAI_HELP -eq "1") {
    Print-RunaiHelp
    return
}

switch ($env:RUNAI_PHASE) {
    "register-only" { $DoAuth = $true;  $DoHook = $false; $DoSettings = $false; $LoginOnly = $false }
    "login-only"    { $DoAuth = $true;  $DoHook = $false; $DoSettings = $false; $LoginOnly = $true  }
    "hook-only"     { $DoAuth = $false; $DoHook = $true;  $DoSettings = $true  }
    $null           { }  # default = all phases
    ""              { }
    default {
        Write-Error "runai-install: unknown RUNAI_PHASE '$($env:RUNAI_PHASE)' — see RUNAI_HELP=1"
        exit 1
    }
}

if ($ServerUrl -eq ("{" + "SERVER_URL" + "}") -or [string]::IsNullOrEmpty($ServerUrl)) {
    Write-Error "SERVER_URL placeholder not substituted. Pipe this through the runai server's /install.ps1 endpoint."
    exit 1
}

# Pretty headers (Windows 10+ console / Terminal both understand ANSI).
function Write-Hr     { Write-Host ("`e[38;5;81m" + ("=" * 60) + "`e[0m") }
function Write-Brand  { Write-Host ("`e[38;5;81m| `e[1mrunai`e[0m `e[2mskill router`e[0m    `e[2mclient install (Windows)`e[0m") }
function Write-Step   { param([string]$num, [string]$desc); Write-Host ("`e[38;5;81m|`e[0m `e[1m[$num]`e[0m $desc") }
function Write-Ok     { param([string]$msg);  Write-Host ("  `e[38;5;114m[OK]`e[0m $msg") }
function Write-Warn2  { param([string]$msg);  Write-Host ("  `e[38;5;221m[..]`e[0m $msg") }
function Write-Fail2  { param([string]$msg);  Write-Host ("  `e[38;5;203m[!!]`e[0m $msg") }
function Write-Dim    { param([string]$msg);  Write-Host ("`e[2m$msg`e[0m") }

Write-Host ""
Write-Hr
Write-Brand
Write-Hr
Write-Dim ("  server   $ServerUrl")
Write-Dim ("  identity $IdentityPath")
Write-Dim ("  hook     $HookPath")
Write-Dim ("  config   $SettingsPath")
Write-Hr
Write-Host ""

# === RUNAI_SECTION:owner-only START ===
# Reserved scaffold: this block is delivered ONLY when the server is in
# owner mode. Owner mode currently returns 404 for /install.ps1 entirely
# (single-user self-serve has no remote client surface), so this section
# stays empty by design — kept here so the template grammar is symmetric.
# === RUNAI_SECTION:owner-only END ===

# === RUNAI_SECTION:team-only START ===
# Everything below is the team-mode client surface: register / login,
# write identity, install hook wrapper, patch Claude Code settings.json.
# The server strips this whole block before serving the script if it is
# ever run in owner mode (currently unreachable: owner mode returns 404
# at the route level — see PLANNING.md §1.2).

$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

# 1) Account setup. Prompt for username + password unless a valid
# ~/.runai-identity already exists or env vars are supplied. Tries login
# first, falls back to register on 401 (unless --login-only). Persists
# api_key at %USERPROFILE%\.runai-identity.
$haveIdentity = $false
if (Test-Path $IdentityPath) {
    try {
        $existing = Get-Content $IdentityPath -Raw | ConvertFrom-Json
        if ($existing.api_key) { $haveIdentity = $true }
    } catch { }
}

if ($DoAuth) {
    Write-Step "1/3" "account setup"
    if ($haveIdentity) {
        try {
            $headers = @{ Authorization = "Bearer $($existing.api_key)" }
            Invoke-RestMethod -Method Get -Uri "$ServerUrl/api/me" -Headers $headers -TimeoutSec 10 | Out-Null
            Write-Ok "found existing identity, reusing stored api_key"
            Write-Dim ("  (remove $IdentityPath to switch user)")
            Write-Host ""
        } catch {
            Write-Warn2 "existing identity is stale, signing in again"
            Remove-Item -Force $IdentityPath -ErrorAction SilentlyContinue
            $haveIdentity = $false
        }
    }
    if (-not $haveIdentity) {
        Write-Dim ("  new device - register or sign in to $ServerUrl")

        # Username: env first, then TTY prompt. Env wins so RUNAI_USERNAME=x
        # works in non-interactive (agent / CI) contexts.
        if ([string]::IsNullOrWhiteSpace($env:RUNAI_USERNAME)) {
            $RunaiUsername = Read-Host "  username"
        } else {
            $RunaiUsername = $env:RUNAI_USERNAME
            Write-Ok "using RUNAI_USERNAME from env"
        }
        if ([string]::IsNullOrWhiteSpace($RunaiUsername)) {
            Write-Fail2 "username cannot be empty (set `$env:RUNAI_USERNAME or answer the prompt)"
            exit 1
        }

        if ([string]::IsNullOrEmpty($env:RUNAI_PASSWORD)) {
            $RunaiPasswordSecure = Read-Host "  password" -AsSecureString
            $bstr = [System.Runtime.InteropServices.Marshal]::SecureStringToBSTR($RunaiPasswordSecure)
            $RunaiPassword = [System.Runtime.InteropServices.Marshal]::PtrToStringAuto($bstr)
            [System.Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) | Out-Null
        } else {
            $RunaiPassword = $env:RUNAI_PASSWORD
            Write-Ok "using RUNAI_PASSWORD from env"
        }
        if ([string]::IsNullOrEmpty($RunaiPassword)) {
            Write-Fail2 "password cannot be empty (set `$env:RUNAI_PASSWORD or answer the prompt)"
            exit 1
        }

        $authBody = @{ username = $RunaiUsername; password = $RunaiPassword } | ConvertTo-Json -Compress
        $resp = $null
        Write-Warn2 "trying sign-in as $RunaiUsername"
        $loginFailed = $false
        $loginHttpStatus = $null
        try {
            $resp = Invoke-RestMethod -Method Post -Uri "$ServerUrl/auth/login" `
                -ContentType "application/json; charset=utf-8" -Body $authBody
            Write-Ok "signed in as $RunaiUsername"
        } catch {
            $loginFailed = $true
            if ($_.Exception.Response) {
                try { $loginHttpStatus = [int]$_.Exception.Response.StatusCode } catch {}
            }
        }
        if ($loginFailed) {
            if ($LoginOnly) {
                Write-Fail2 "sign-in failed and RUNAI_PHASE=login-only is set"
                if ($loginHttpStatus -eq 401) {
                    Write-Warn2 "forgot your password? ask the server admin to reset it with runai admin reset-password"
                }
                exit 1
            }
            Write-Warn2 "user does not exist, registering"
            try {
                $resp = Invoke-RestMethod -Method Post -Uri "$ServerUrl/users/register" `
                    -ContentType "application/json; charset=utf-8" -Body $authBody
                Write-Ok "registered $RunaiUsername"
            } catch {
                Write-Fail2 "auth failed: $($_.Exception.Message)"
                if ($loginHttpStatus -eq 401) {
                    Write-Warn2 "forgot your password? ask the server admin to reset it with runai admin reset-password"
                }
                exit 1
            }
        }

        $identity = [PSCustomObject]@{
            version  = 1
            server   = $ServerUrl
            user_id  = $resp.user_id
            username = $resp.username
            api_key  = $resp.api_key
            is_admin = [bool]$resp.is_admin
        }
        [System.IO.File]::WriteAllText($IdentityPath, ($identity | ConvertTo-Json), $utf8NoBom)
        Write-Ok ("wrote " + $IdentityPath)
        $haveIdentity = $true
    }
    Write-Host ""
}

# 2) Write the hook wrapper. Reads Claude Code's hook JSON from stdin,
#    forwards to the server with X-Runai-User header, prints server
#    response on stdout. 30s timeout + silent failure so a slow /
#    unreachable server never blocks the user's Claude Code prompt.
if ($DoHook) {
    # --hook-only safety: refuse to write a hook if there's no identity
    # to back it.
    if (-not $DoAuth -and -not $haveIdentity) {
        Write-Fail2 "RUNAI_PHASE=hook-only requires an existing $IdentityPath; run without RUNAI_PHASE first"
        exit 1
    }
    $hookBody = @"
# Auto-generated by runai-client-install.ps1. Overwritten on reinstall.
# Force UTF-8 on stdin / stdout / Invoke-RestMethod body — the Claude Code
# hook protocol uses UTF-8 JSON, but PowerShell defaults to the Windows
# console codepage (CP936 in zh-CN, CP1252 in en-US). Without these the
# Chinese characters in the user prompt arrive on the server as '?',
# routing breaks and the router LLM receives '???'. The output side has
# the same issue: hook output goes back to Claude Code, so reasoning /
# skill descriptions must also stay UTF-8.
`$ErrorActionPreference = "Continue"
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new(`$false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(`$false)
`$OutputEncoding = [System.Text.UTF8Encoding]::new(`$false)

``$RunaiServer = "$ServerUrl"
`$RunaiIdentity = "`$env:USERPROFILE\.runai-identity"

# Best-effort: pull api_key from ~/.runai-identity so the hook sends
# Authorization: Bearer <key>. Missing file or parse error → fall back
# to legacy X-Runai-User-only mode so existing setups keep working.
`$RunaiApiKey = ""
if (Test-Path `$RunaiIdentity) {
    try {
        `$identity = Get-Content `$RunaiIdentity -Raw | ConvertFrom-Json
        if (`$identity.api_key) { `$RunaiApiKey = `$identity.api_key }
    } catch { `$RunaiApiKey = "" }
}

try {
    # Read stdin as raw UTF-8 bytes (don't trust [Console]::In default
    # codepage even after setting InputEncoding above — re-open the
    # underlying stream to be safe).
    `$stdin = [Console]::OpenStandardInput()
    `$reader = New-Object System.IO.StreamReader(`$stdin, [System.Text.UTF8Encoding]::new(`$false))
    `$payload = `$reader.ReadToEnd()

    # Encode body as UTF-8 bytes for Invoke-RestMethod so the HTTP body
    # is exactly the bytes Claude Code wrote, not a re-encoded version.
    `$bodyBytes = [System.Text.Encoding]::UTF8.GetBytes(`$payload)

    `$headers = @{ 'X-Runai-User' = "`$env:USERNAME@`$env:COMPUTERNAME" }
    if (`$RunaiApiKey) { `$headers['Authorization'] = "Bearer `$RunaiApiKey" }

    `$resp = Invoke-RestMethod ``
        -Method Post ``
        -Uri "`$RunaiServer/recommend" ``
        -ContentType "application/json; charset=utf-8" ``
        -Headers `$headers ``
        -Body `$bodyBytes ``
        -TimeoutSec 30
    # Emit response as UTF-8 bytes via stdout (bypass PS string encoding).
    `$respBytes = [System.Text.Encoding]::UTF8.GetBytes(`$resp)
    [Console]::OpenStandardOutput().Write(`$respBytes, 0, `$respBytes.Length)
} catch {
    # Silent fail: do not block Claude Code prompt on server hiccups.
}
"@
    # Write hook script as UTF-8 NO BOM. PowerShell 5.1's `-Encoding utf8`
    # writes a BOM, which breaks the `#requires` / shebang convention and
    # can confuse some shells; PS Core's `utf8NoBOM` isn't available on 5.1.
    # Use raw .NET writer for portability across PS 5.1 and 7+.
    Write-Step "2/3" "install hook wrapper"
    [System.IO.File]::WriteAllText($HookPath, $hookBody, $utf8NoBom)
    Write-Ok ("wrote " + $HookPath)
    Write-Host ""
}

if ($DoSettings) {
    Write-Step "3/3" "patch Claude Code settings"
    $claudeDir = Split-Path $SettingsPath
    if (-not (Test-Path $claudeDir)) {
        New-Item -ItemType Directory -Path $claudeDir -Force | Out-Null
    }
    if (Test-Path $SettingsPath) {
        Copy-Item $SettingsPath "$SettingsPath.runai-bak" -Force
        Write-Ok ("backed up to " + $SettingsPath + ".runai-bak")
        $raw = Get-Content $SettingsPath -Raw
        if ([string]::IsNullOrWhiteSpace($raw)) { $raw = "{}" }
        try {
            $parsed = $raw | ConvertFrom-Json
        } catch {
            Write-Warning "settings.json was not valid JSON, replacing with empty object"
            $parsed = New-Object PSObject
        }
    } else {
        $parsed = New-Object PSObject
    }

    # Recursively convert PSCustomObject -> nested hashtable so we can mutate
    # arrays / add keys without PSCustomObject quirks.
    function ConvertTo-RunaiHashtable($obj) {
        if ($null -eq $obj) { return $null }
        if ($obj -is [PSCustomObject]) {
            $h = @{}
            foreach ($p in $obj.PSObject.Properties) {
                $h[$p.Name] = ConvertTo-RunaiHashtable $p.Value
            }
            return $h
        }
        if ($obj -is [System.Collections.IEnumerable] -and -not ($obj -is [string])) {
            $arr = @()
            foreach ($item in $obj) { $arr += ,(ConvertTo-RunaiHashtable $item) }
            return ,$arr
        }
        return $obj
    }

    $data = ConvertTo-RunaiHashtable $parsed
    if ($null -eq $data -or -not ($data -is [hashtable])) { $data = @{} }
    if (-not $data.ContainsKey('hooks')) { $data.hooks = @{} }
    if (-not $data.hooks.ContainsKey('UserPromptSubmit')) { $data.hooks.UserPromptSubmit = @() }

    # Claude Code on Windows invokes hook commands via cmd.exe by default.
    # Wrap with `chcp 65001 >NUL & powershell ...` to switch the console
    # codepage to UTF-8 BEFORE PowerShell takes stdin — without it the JSON
    # from Claude Code is already munged through CP936/CP1252 by the time
    # our hook script runs, no in-script encoding override can recover the
    # original bytes. `chcp 65001` is the well-known Windows-UTF-8 idiom.
    $hookCmd = "chcp 65001 >NUL & powershell -NoProfile -ExecutionPolicy Bypass -File `"$HookPath`""

    # Idempotent: skip if our exact command is already present.
    $already = $false
    foreach ($g in $data.hooks.UserPromptSubmit) {
        if ($null -ne $g -and $g.ContainsKey('hooks')) {
            foreach ($h in @($g.hooks)) {
                if ($null -ne $h -and $h.command -eq $hookCmd) { $already = $true; break }
            }
        }
        if ($already) { break }
    }

    if (-not $already) {
        $newGroup = @{ hooks = @(@{ type = "command"; command = $hookCmd }) }
        $data.hooks.UserPromptSubmit = @($data.hooks.UserPromptSubmit + $newGroup)
        Write-Ok "patched UserPromptSubmit hook"
    } else {
        Write-Ok "hook already present (no-op)"
    }

    # settings.json also UTF-8 no BOM — Claude Code parses it with UTF-8 by
    # default and a BOM trips some JSON readers.
    [System.IO.File]::WriteAllText($SettingsPath, ($data | ConvertTo-Json -Depth 20), $utf8NoBom)
    Write-Host ""
}
# === RUNAI_SECTION:team-only END ===

Write-Hr
Write-Host ("  `e[38;5;114m`e[1mall set.`e[0m  open a `e[1mnew`e[0m Claude Code session and your prompts")
Write-Host ("  will route through `e[38;5;81m$ServerUrl`e[0m")
Write-Hr
Write-Dim ("  dashboard   $ServerUrl")
Write-Dim ("  uninstall   irm $ServerUrl/uninstall.ps1 | iex")
Write-Dim ("  switch user del `"$IdentityPath`" && re-run installer")
Write-Host ""

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
        # rotate_api_key: this installer persists the key to ~/.runai-identity,
        # so it opts into rotation. A plain (dashboard) login never rotates.
        $loginBody = @{ username = $RunaiUsername; password = $RunaiPassword; rotate_api_key = $true } | ConvertTo-Json -Compress
        $resp = $null
        Write-Warn2 "trying sign-in as $RunaiUsername"
        $loginFailed = $false
        $loginHttpStatus = $null
        try {
            $resp = Invoke-RestMethod -Method Post -Uri "$ServerUrl/auth/login" `
                -ContentType "application/json; charset=utf-8" -Body $loginBody
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
    `$json = `$payload | ConvertFrom-Json
    `$json | Add-Member -NotePropertyName client_kind -NotePropertyValue 'claude' -Force
    `$payload = `$json | ConvertTo-Json -Depth 20 -Compress

    # Encode body as UTF-8 bytes for Invoke-RestMethod so the HTTP body
    # carries the original hook fields plus runai's host marker.
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

# 2b) Write the runai-client companion (PowerShell). Mirrors the bash
#     companion's activate / file / feedback / sync / flush subcommands + the
#     client-cache + durable outbox (PLANNING §1.3). The agent-facing
#     hook output now says `runai-client activate <name>`, so Windows
#     agents need this companion on PATH. Installed at
#     ~\.local\bin\runai-client.ps1.
if ($DoHook) {
    $companionDir = "$env:USERPROFILE\.local\bin"
    $companionPath = "$companionDir\runai-client.ps1"
    if (-not (Test-Path $companionDir)) {
        New-Item -ItemType Directory -Path $companionDir -Force | Out-Null
    }
    $companionBody = @'
# Auto-generated by runai-client-install.ps1. Overwritten on reinstall.
# runai-client (PowerShell companion) — activate / file / feedback / sync / flush
# for the runai activation/feedback protocol (PLANNING §1.3).
$ErrorActionPreference = "Stop"
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)

function Get-RunaiServer {
    if ($env:RUNAI_SERVER) { return $env:RUNAI_SERVER }
    $id = if ($env:RUNAI_IDENTITY) { $env:RUNAI_IDENTITY } else { "$env:USERPROFILE\.runai-identity" }
    if (Test-Path $id) {
        try { return (Get-Content $id -Raw | ConvertFrom-Json).server } catch {}
    }
    return $null
}
function Get-RunaiApiKey {
    if ($env:RUNAI_API_KEY) { return $env:RUNAI_API_KEY }
    $id = if ($env:RUNAI_IDENTITY) { $env:RUNAI_IDENTITY } else { "$env:USERPROFILE\.runai-identity" }
    if (Test-Path $id) {
        try { return (Get-Content $id -Raw | ConvertFrom-Json).api_key } catch {}
    }
    return $null
}
function Get-CacheRoot { return "$env:USERPROFILE\.runai\client-cache" }
function Get-RunaiCacheKey {
    param([string]$Value)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
    $hash = $sha.ComputeHash($bytes)
    return (($hash | ForEach-Object { $_.ToString('x2') }) -join '')
}
function Get-ServerCacheRoot {
    $server = Get-RunaiServer
    if (-not $server) { return (Join-Path (Get-CacheRoot) 'servers\missing-server') }
    return (Join-Path (Get-CacheRoot) ("servers\" + (Get-RunaiCacheKey $server)))
}
function Get-SkillCacheDir {
    param([string]$Skill)
    return (Join-Path (Get-ServerCacheRoot) ("skills\" + (Get-RunaiCacheKey $Skill)))
}
function New-RunaiEventId { return ([string]([guid]::NewGuid())) }
function Write-RunaiWarn { param([string]$m) Write-Host "runai-client: $m" -ForegroundColor Yellow }
function Write-RunaiDie { param([string]$m) Write-Host "runai-client: $m" -ForegroundColor Red; exit 1 }

function Get-AuthHeaders {
    $h = @{}
    $k = Get-RunaiApiKey
    if ($k) { $h['Authorization'] = "Bearer $k" }
    return $h
}

function Invoke-FetchFile {
    param([string]$Skill, [string]$Rel, [string]$Dest)
    if ($Rel -match '\.\.|^/|\\') { Write-Host "runai-client: refusing traversal include path: $Rel"; return $false }
    $server = Get-RunaiServer
    $headers = Get-AuthHeaders
    $url = "$server/skills/file/$Skill/$Rel"
    $dir = Split-Path $Dest -Parent
    if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
    $tmp = "$Dest.tmp"
    try {
        Invoke-WebRequest -Uri $url -Headers $headers -OutFile $tmp -UseBasicParsing -TimeoutSec 30
        Move-Item -Force $tmp $Dest
        return $true
    } catch { Remove-Item -Force $tmp -ErrorAction SilentlyContinue; return $false }
}

function Invoke-FetchBundle {
    param([string]$Skill, [string]$DestDir)
    $server = Get-RunaiServer
    $headers = Get-AuthHeaders
    $url = "$server/skills/bundle/$Skill"
    $stage = Join-Path $env:TEMP "runai-bundle-$([guid]::NewGuid())"
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    $tgz = Join-Path $stage "bundle.tar.gz"
    try {
        Invoke-WebRequest -Uri $url -Headers $headers -OutFile $tgz -UseBasicParsing -TimeoutSec 60
        if (-not (Get-Command tar -ErrorAction SilentlyContinue)) {
            throw "tar command not found; cannot extract runai skill bundle"
        }
        & tar -xzf $tgz -C $stage 2>$null
        if ($LASTEXITCODE -ne 0) {
            throw "tar extraction failed"
        }
        Remove-Item -Force $tgz -ErrorAction SilentlyContinue
        if (Test-Path (Join-Path $stage $Skill)) {
            $inner = Join-Path $stage $Skill
            if (Test-Path $DestDir) { Remove-Item -Recurse -Force $DestDir }
            Move-Item $inner $DestDir
        } else {
            if (Test-Path $DestDir) { Remove-Item -Recurse -Force $DestDir }
            Move-Item $stage $DestDir
        }
        return $true
    } catch { Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue; return $false }
}

function Invoke-CacheBundle {
    param([string]$Skill, [string]$CacheDir, [string]$SkillMd)
    $filesDir = Join-Path $CacheDir 'files'
    $marker = Join-Path $CacheDir '.bundle-ok'
    if (-not (Invoke-FetchBundle -Skill $Skill -DestDir $filesDir)) { return $false }
    $fetchedSkillMd = Join-Path $filesDir 'SKILL.md'
    if (-not (Test-Path $fetchedSkillMd)) {
        Remove-Item -Force $marker -ErrorAction SilentlyContinue
        return $false
    }
    Copy-Item -Force $fetchedSkillMd $SkillMd
    Remove-Item -Force $fetchedSkillMd -ErrorAction SilentlyContinue
    Set-Content -Path $marker -Value '' -NoNewline
    return $true
}

function Test-RunaiRelPath {
    param([string]$Rel)
    if (-not $Rel) { return $false }
    if ($Rel -match '\.\.|^/|^[A-Za-z]:|\\') { return $false }
    return $true
}

function Write-Outbox {
    param([string]$Kind, [string]$EventId, [string]$Skill, [string]$Body, [string]$SessionId, [string]$Note)
    $outbox = Join-Path (Get-SkillCacheDir $Skill) '.outbox'
    if (-not (Test-Path $outbox)) { New-Item -ItemType Directory -Path $outbox -Force | Out-Null }
    $ts = [int][double]::Parse((Get-Date -UFormat %s))
    $entry = [ordered]@{ event_id=$EventId; kind=$Kind; skill=$Skill; body=$Body; session_id=$SessionId; note=$Note; ts=$ts; attempts=0 }
    $tmp = Join-Path $outbox ".tmp.$EventId.json"
    $final = Join-Path $outbox "$ts-$EventId.json"
    $json = ($entry | ConvertTo-Json -Compress) + "`n"
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $fs = [System.IO.FileStream]::new(
        $tmp,
        [System.IO.FileMode]::Create,
        [System.IO.FileAccess]::Write,
        [System.IO.FileShare]::None,
        4096,
        [System.IO.FileOptions]::WriteThrough
    )
    try {
        $fs.Write($bytes, 0, $bytes.Length)
        $fs.Flush($true)
    } finally {
        $fs.Dispose()
    }
    Move-Item -Force $tmp $final
}

function Invoke-OutboxReplay {
    param([string]$File)
    $entry = Get-Content $File -Raw | ConvertFrom-Json
    $server = Get-RunaiServer
    $headers = Get-AuthHeaders
    $headers['X-Runai-Event-Id'] = $entry.event_id
    if ($entry.kind -eq 'usage') { $url = "$server/skills/use/$($entry.skill)" } else { $url = "$server/feedback" }
    try {
        $resp = Invoke-WebRequest -Uri $url -Method Post -Headers $headers -ContentType 'application/json' -Body $entry.body -UseBasicParsing -TimeoutSec 15
        $code = [int]$resp.StatusCode
    } catch {
        $code = 0
        if ($_.Exception.Response) { try { $code = [int]$_.Exception.Response.StatusCode } catch {} }
    }
    if ($code -eq 200 -or $code -eq 409 -or $code -eq 422) {
        Remove-Item -Force $File -ErrorAction SilentlyContinue
        return $true
    }
    if ($code -eq 401 -or $code -eq 403) {
        Write-Host "runai-client flush: dropping $($entry.kind) event $($entry.event_id) (auth failure $code)"
        Remove-Item -Force $File -ErrorAction SilentlyContinue
        return $true
    }
    # 5xx / network — increment attempts, keep.
    $entry.attempts = [int]$entry.attempts + 1
    $entry | ConvertTo-Json -Compress | Set-Content -Path $File -Encoding UTF8
    return $false
}

function Invoke-Activate {
    param([string]$Skill, [string]$SessionId, [switch]$Refresh, [string[]]$Include, [switch]$All, [string]$EventId)
    if (-not $Skill) { Write-Host "runai-client activate: skill name required"; exit 2 }
    $server = Get-RunaiServer
    if (-not $server) { Write-RunaiDie "no server URL — set RUNAI_SERVER or run install first" }
    if (-not $EventId) { $EventId = New-RunaiEventId }
    $cacheRoot = Get-ServerCacheRoot
    $cacheDir = Get-SkillCacheDir $Skill
    $skillMd = Join-Path $cacheDir 'SKILL.md'
    $bundleMarker = Join-Path $cacheDir '.bundle-ok'
    New-Item -ItemType Directory -Path (Join-Path $cacheDir 'files') -Force | Out-Null
    New-Item -ItemType Directory -Path (Join-Path $cacheDir '.outbox') -Force | Out-Null
    $body = @{ session_id=$SessionId; include=@($Include) } | ConvertTo-Json -Compress
    $headers = Get-AuthHeaders
    $headers['X-Runai-Event-Id'] = $EventId
    $url = "$server/skills/use/$Skill"
    $code = 0
    try {
        $resp = Invoke-WebRequest -Uri $url -Method Post -Headers $headers -ContentType 'application/json' -Body $body -UseBasicParsing -TimeoutSec 15
        $code = [int]$resp.StatusCode
    } catch {
        if ($_.Exception.Response) { try { $code = [int]$_.Exception.Response.StatusCode } catch {} }
    }
    switch ($code) {
        200 { }
        409 { Write-RunaiDie "event_id conflict (HTTP 409)" }
        401 { Write-RunaiDie "auth failed (HTTP 401)" }
        403 { Write-RunaiDie "auth failed (HTTP 403)" }
        404 { Write-RunaiDie "skill not found on server" }
        0 { Write-Outbox -Kind 'usage' -EventId $EventId -Skill $Skill -Body $body -SessionId $SessionId -Note '' }
        default {
            if ($code -ge 500) { Write-Outbox -Kind 'usage' -EventId $EventId -Skill $Skill -Body $body -SessionId $SessionId -Note '' }
            else { Write-RunaiDie "unexpected HTTP $code" }
        }
    }
    if ((-not $Refresh) -and (Test-Path $skillMd) -and (Test-Path $bundleMarker)) {
        # use complete cache
    } else {
        $ok = Invoke-CacheBundle -Skill $Skill -CacheDir $cacheDir -SkillMd $skillMd
        if (-not $ok) {
            if (Test-Path $skillMd) { Write-RunaiWarn "bundle fetch failed, using stale cache for $Skill" }
            else { Write-RunaiDie "unable to fetch SKILL.md and no warm cache" }
        }
    }
    foreach ($rel in $Include) {
        if (-not (Test-RunaiRelPath $rel)) { Write-RunaiWarn "invalid include path: $rel"; continue }
        $dest = Join-Path (Join-Path $cacheDir 'files') $rel
        if (-not (Test-Path $dest)) { Invoke-FetchFile -Skill $Skill -Rel $rel -Dest $dest | Out-Null }
    }
    Get-Content $skillMd -Raw
}

function Invoke-File {
    param([string]$Skill, [string]$Rel)
    if (-not $Skill -or -not $Rel) { Write-Host "runai-client file: skill and relpath required"; exit 2 }
    if (-not (Test-RunaiRelPath $Rel)) { Write-Host "runai-client file: refusing traversal path: $Rel"; exit 2 }
    $server = Get-RunaiServer
    if (-not $server) { Write-RunaiDie "no server URL" }
    $cacheDir = Get-SkillCacheDir $Skill
    if ($Rel -eq 'SKILL.md') { $target = Join-Path $cacheDir 'SKILL.md' }
    else { $target = Join-Path (Join-Path $cacheDir 'files') $Rel }
    if (-not (Test-Path $target)) {
        $dir = Split-Path $target -Parent
        if ($dir -and -not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir -Force | Out-Null }
        if (-not (Invoke-FetchFile -Skill $Skill -Rel $Rel -Dest $target)) {
            Write-RunaiWarn "not found in skill bundle: $Skill/$Rel"
            Write-RunaiDie "this command only reads files inside the managed skill directory cached by activate/sync; runtime paths such as ~/.ppt-anything must be read from the local filesystem."
        }
    }
    Get-Content $target -Raw
}

function Invoke-Feedback {
    param([string]$Skill, [string]$Note, [string]$Verdict, [string]$EventId)
    if (-not $Skill) { Write-Host "runai-client feedback: skill name required"; exit 2 }
    if ($Verdict -and $Verdict -ne 'good' -and $Verdict -ne 'bad') {
        Write-Host "runai-client feedback: --verdict must be good or bad"; exit 2
    }
    if (-not $Note -and -not $Verdict) { Write-Host "runai-client feedback: --verdict or --note required"; exit 2 }
    $server = Get-RunaiServer
    if (-not $server) { Write-RunaiDie "no server URL" }
    if (-not $EventId) { $EventId = New-RunaiEventId }
    $payload = @{ skill = $Skill }
    if ($Note) { $payload['note'] = $Note }
    if ($Verdict) { $payload['verdict'] = $Verdict }
    $body = $payload | ConvertTo-Json -Compress
    $headers = Get-AuthHeaders
    $headers['X-Runai-Event-Id'] = $EventId
    $code = 0
    try { $resp = Invoke-WebRequest -Uri "$server/feedback" -Method Post -Headers $headers -ContentType 'application/json' -Body $body -UseBasicParsing -TimeoutSec 15; $code = [int]$resp.StatusCode }
    catch { if ($_.Exception.Response) { try { $code = [int]$_.Exception.Response.StatusCode } catch {} } }
    switch ($code) {
        200 { exit 0 }
        409 { exit 0 }
        401 { Write-RunaiDie "auth failed (HTTP 401)" }
        403 { Write-RunaiDie "auth failed (HTTP 403)" }
        0 { Write-Outbox -Kind 'feedback' -EventId $EventId -Skill $Skill -Body $body -SessionId '' -Note $Note; exit 0 }
        default { if ($code -ge 500) { Write-Outbox -Kind 'feedback' -EventId $EventId -Skill $Skill -Body $body -SessionId '' -Note $Note; exit 0 } else { Write-RunaiDie "unexpected HTTP $code" } }
    }
}

function Invoke-Sync {
    param([string[]]$Skills, [switch]$All)
    if (-not $Skills -or $Skills.Count -eq 0) { Write-Host "runai-client sync: at least one skill required"; exit 2 }
    $server = Get-RunaiServer
    $cacheRoot = Get-ServerCacheRoot
    $ok=0; $skip=0
    foreach ($s in $Skills) {
        $dir = Get-SkillCacheDir $s
        New-Item -ItemType Directory -Path (Join-Path $dir 'files') -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $dir '.outbox') -Force | Out-Null
        $fetched = Invoke-CacheBundle -Skill $s -CacheDir $dir -SkillMd (Join-Path $dir 'SKILL.md')
        if (-not $fetched) { Write-Host "runai-client sync: skipping $s (server non-200 or unreachable)"; $skip++; continue }
        $ok++
    }
    Write-Host "sync: prewarmed $ok / skipped $skip"
}

function Invoke-Flush {
    $cacheRoot = Get-CacheRoot
    if (-not (Test-Path $cacheRoot)) { Write-Host "flush: outbox empty"; exit 0 }
    $files = Get-ChildItem -Path $cacheRoot -Recurse -Filter '*.json' | Where-Object { $_.FullName -match '\.outbox' } | Sort-Object LastWriteTime
    if (-not $files) { Write-Host "flush: outbox empty"; exit 0 }
    $ok=0; $retain=0
    foreach ($f in $files) { if (Invoke-OutboxReplay -File $f.FullName) { $ok++ } else { $retain++ } }
    Write-Host "flush: replayed $ok / retained $retain"
}

# --- dispatch ---
$sub = if ($args.Count -ge 1) { $args[0] } else { '' }
switch ($sub) {
    '' { ; }
    'activate' {
        $skill=''; $session=''; $refresh=$false; $all=$false; $eventId=''; $include=@()
        for ($i=1; $i -lt $args.Count; $i++) {
            switch ($args[$i]) {
                '--session-id' { $session=$args[++$i] }
                '--refresh' { $refresh=$true }
                '--all' { $all=$true }
                '--event-id' { $eventId=$args[++$i] }
                '--include' { $include+=$args[++$i] }
                default { if (-not $skill) { $skill=$args[$i] } }
            }
        }
        Invoke-Activate -Skill $skill -SessionId $session -Refresh:$refresh -Include $include -All:$all -EventId $eventId
    }
    'file' {
        $skill=''; $rel=''
        for ($i=1; $i -lt $args.Count; $i++) {
            if (-not $skill) { $skill=$args[$i]; continue }
            if (-not $rel) { $rel=$args[$i]; continue }
        }
        Invoke-File -Skill $skill -Rel $rel
    }
    'feedback' {
        $skill=''; $note=''; $verdict=''; $eventId=''
        for ($i=1; $i -lt $args.Count; $i++) {
            switch ($args[$i]) {
                '--note' { $note=$args[++$i] }
                '--verdict' { $verdict=$args[++$i] }
                '--event-id' { $eventId=$args[++$i] }
                default { if (-not $skill) { $skill=$args[$i] } }
            }
        }
        Invoke-Feedback -Skill $skill -Note $note -Verdict $verdict -EventId $eventId
    }
    'sync' {
        $all=$false; $skills=@()
        for ($i=1; $i -lt $args.Count; $i++) {
            switch ($args[$i]) {
                '--all' { $all=$true }
                default { $skills+=$args[$i] }
            }
        }
        Invoke-Sync -Skills $skills -All:$all
    }
    'flush' { Invoke-Flush }
    default { Write-Host "runai-client: unknown subcommand: $sub"; exit 1 }
}
'@
    [System.IO.File]::WriteAllText($companionPath, $companionBody, $utf8NoBom)
    Write-Ok ("wrote " + $companionPath)
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

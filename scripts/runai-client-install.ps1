# runai client install - Windows / PowerShell.
#
# This file is a TEMPLATE - the server hydrates it per request:
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
function Resolve-RunaiProfileRoot {
    if (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) {
        return $env:USERPROFILE
    }
    if (-not [string]::IsNullOrWhiteSpace($env:HOME)) {
        return $env:HOME
    }
    if (-not [string]::IsNullOrWhiteSpace($env:HOMEDRIVE) -and -not [string]::IsNullOrWhiteSpace($env:HOMEPATH)) {
        return "$env:HOMEDRIVE$env:HOMEPATH"
    }
    throw "runai-install: cannot resolve user profile path; set USERPROFILE or HOME"
}
$RunaiProfileRoot = Resolve-RunaiProfileRoot
$IdentityPath = Join-Path $RunaiProfileRoot ".runai-identity"
$ServerPinPath = Join-Path $RunaiProfileRoot ".runai-server.json"
$HookPath = Join-Path $RunaiProfileRoot ".runai-hook.ps1"
$SettingsPath = Join-Path (Join-Path $RunaiProfileRoot ".claude") "settings.json"
$RunaiClientPath = Join-Path (Join-Path $RunaiProfileRoot ".local\bin") "runai-client.ps1"
$RunaiClientShimPath = Join-Path (Join-Path $RunaiProfileRoot ".local\bin") "runai-client.cmd"

# Phase selection. Defaults: run everything.
$DoAuth = $true
$DoHook = $true
$DoSettings = $true
$LoginOnly = $false

function Print-RunaiHelp {
    Write-Host @"
runai client install (Windows / PowerShell) - TTY + non-interactive installer.

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
                     hook-only      skip identity, refresh server pin,
                                    install hook/client/MCP + settings
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
        throw "runai-install: unknown RUNAI_PHASE '$($env:RUNAI_PHASE)' - see RUNAI_HELP=1"
    }
}

if ($ServerUrl -eq ("{" + "SERVER_URL" + "}") -or [string]::IsNullOrEmpty($ServerUrl)) {
    throw "SERVER_URL placeholder not substituted. Pipe this through the runai server's /install.ps1 endpoint."
}

# TLS: a runai team server uses a self-signed cert, so skip CA-chain
# validation for HTTPS (mirrors the bash client's curl --insecure). PS 7+
# uses -SkipCertificateCheck via default params; Windows PowerShell 5.1 has
# no such switch, so set the legacy ServicePointManager callback + TLS 1.2.
# Plain HTTP needs none of this.
if ($ServerUrl -like 'https://*') {
    if ($PSVersionTable.PSVersion.Major -ge 6) {
        $PSDefaultParameterValues['Invoke-RestMethod:SkipCertificateCheck'] = $true
        $PSDefaultParameterValues['Invoke-WebRequest:SkipCertificateCheck'] = $true
    } else {
        [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12
        [System.Net.ServicePointManager]::ServerCertificateValidationCallback = { $true }
    }
}

# Pretty headers. Windows PowerShell 5.1 does not recognize the PowerShell 7
# `` `e`` escape sequence, so use [char]27 and disable styling unless the host
# is likely to render ANSI. This keeps legacy consoles readable instead of
# printing literal e[38;5;81m text.
$RunaiAnsi = $false
if ($env:NO_COLOR -ne "1" -and $Host.Name -match "ConsoleHost|Visual Studio Code|Windows Terminal") {
    $RunaiAnsi = $true
}
$Esc = [char]27
function Runai-Style {
    param([string]$Code, [string]$Text)
    if ($script:RunaiAnsi) { return "$script:Esc[$Code$Text$script:Esc[0m" }
    return $Text
}
function Write-Hr     { Write-Host (Runai-Style "38;5;81m" ("=" * 60)) }
function Write-Brand  { Write-Host ((Runai-Style "38;5;81m" "| ") + (Runai-Style "1m" "runai") + " " + (Runai-Style "2m" "skill router") + "    " + (Runai-Style "2m" "client install (Windows)")) }
function Write-Step   { param([string]$num, [string]$desc); Write-Host ((Runai-Style "38;5;81m" "|") + " " + (Runai-Style "1m" "[$num]") + " $desc") }
function Write-Ok     { param([string]$msg);  Write-Host ("  " + (Runai-Style "38;5;114m" "[OK]") + " $msg") }
function Write-Warn2  { param([string]$msg);  Write-Host ("  " + (Runai-Style "38;5;221m" "[..]") + " $msg") }
function Write-Fail2  { param([string]$msg);  Write-Host ("  " + (Runai-Style "38;5;203m" "[!!]") + " $msg") }
function Write-Dim    { param([string]$msg);  Write-Host (Runai-Style "2m" $msg) }
function Stop-RunaiInstall {
    param([string]$Message)
    Write-Fail2 $Message
    throw $Message
}
function Test-RunaiIdentityWithServer {
    if (-not (Test-Path -LiteralPath $IdentityPath)) { return $false }
    try {
        $existing = Get-Content -LiteralPath $IdentityPath -Raw -Encoding UTF8 | ConvertFrom-Json
        if (-not $existing.api_key) { return $false }
        $headers = @{ Authorization = "Bearer $($existing.api_key)" }
        $me = Invoke-RestMethod -Method Get -Uri "$ServerUrl/api/me" -Headers $headers
        return ($me.username -eq $existing.username -and $me.user_id -eq $existing.user_id)
    } catch {
        return $false
    }
}
function Write-RunaiServerPin {
    # Pin the server's leaf-cert SHA-256 for every future HTTPS prompt /
    # companion-CLI request. The team server cert is self-signed, so the
    # client intentionally skips CA-chain validation; this pin is the MITM gate.
    if ($ServerUrl -like 'https://*') {
        Write-Warn2 "fetching server cert fingerprint for pinning"
        try {
            $fpResp = Invoke-RestMethod -Method Get -Uri "$ServerUrl/api/tls/fingerprint"
            $fingerprint = [string]$fpResp.fingerprint
        } catch {
            Stop-RunaiInstall "could not fetch /api/tls/fingerprint; aborting before client config is written"
        }
        if ([string]::IsNullOrWhiteSpace($fingerprint)) {
            Stop-RunaiInstall "server returned no fingerprint field; refusing to pin a blank"
        }
        $pin = [PSCustomObject]@{
            version     = 1
            server      = $ServerUrl
            scheme      = "https"
            fingerprint = $fingerprint
        }
        [System.IO.File]::WriteAllText($ServerPinPath, ($pin | ConvertTo-Json), $utf8NoBom)
        Write-Ok ("pinned server fingerprint " + $fingerprint.Substring(0, [Math]::Min(16, $fingerprint.Length)) + "..")
    } else {
        $pin = [PSCustomObject]@{
            version     = 1
            server      = $ServerUrl
            scheme      = "http"
            fingerprint = $null
        }
        [System.IO.File]::WriteAllText($ServerPinPath, ($pin | ConvertTo-Json), $utf8NoBom)
        Write-Warn2 "server is HTTP - no fingerprint pinning possible"
    }
}

Write-Host ""
Write-Hr
Write-Brand
Write-Hr
Write-Dim ("  server   $ServerUrl")
Write-Dim ("  identity $IdentityPath")
Write-Dim ("  hook     $HookPath")
Write-Dim ("  config   $SettingsPath")
Write-Dim ("  client   $RunaiClientPath")
Write-Hr
Write-Host ""

# === RUNAI_SECTION:owner-only START ===
# Reserved scaffold: this block is delivered ONLY when the server is in
# owner mode. Owner mode currently returns 404 for /install.ps1 entirely
# (single-user self-serve has no remote client surface), so this section
# stays empty by design - kept here so the template grammar is symmetric.
# === RUNAI_SECTION:owner-only END ===

# === RUNAI_SECTION:team-only START ===
# Everything below is the team-mode client surface: register / login,
# write identity, install hook wrapper, patch Claude Code settings.json,
# install runai-client, and register the remote HTTP MCP.
# The server strips this whole block before serving the script if it is
# ever run in owner mode (currently unreachable: owner mode returns 404
# at the route level - see PLANNING.md section 1.2).

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
    Write-Step "1/5" "account setup"
    if ($haveIdentity) {
        if (Test-RunaiIdentityWithServer) {
            Write-Ok "found existing identity, server accepted stored api_key"
            Write-Dim ("  (remove $IdentityPath to switch user)")
            Write-Host ""
        } else {
            Write-Dim ("  run again with RUNAI_PHASE=login-only, or remove $IdentityPath to register/sign in fresh")
            Stop-RunaiInstall "existing identity was rejected by $ServerUrl"
        }
    } else {
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
            Stop-RunaiInstall "username cannot be empty (set `$env:RUNAI_USERNAME or answer the prompt)"
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
            Stop-RunaiInstall "password cannot be empty (set `$env:RUNAI_PASSWORD or answer the prompt)"
        }

        $authBody = @{ username = $RunaiUsername; password = $RunaiPassword } | ConvertTo-Json -Compress
        $resp = $null
        Write-Warn2 "trying sign-in as $RunaiUsername"
        $loginFailed = $false
        try {
            $resp = Invoke-RestMethod -Method Post -Uri "$ServerUrl/auth/login" `
                -ContentType "application/json; charset=utf-8" -Body $authBody
            Write-Ok "signed in as $RunaiUsername"
        } catch {
            $loginFailed = $true
        }
        if ($loginFailed) {
            if ($LoginOnly) {
                Stop-RunaiInstall "sign-in failed and RUNAI_PHASE=login-only is set"
            }
            Write-Warn2 "user does not exist, registering"
            try {
                $resp = Invoke-RestMethod -Method Post -Uri "$ServerUrl/users/register" `
                    -ContentType "application/json; charset=utf-8" -Body $authBody
                Write-Ok "registered $RunaiUsername"
            } catch {
                Stop-RunaiInstall "auth failed: $($_.Exception.Message)"
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

if ($DoAuth -or $DoHook) {
    if (-not $DoAuth -and -not $haveIdentity) {
        Stop-RunaiInstall "RUNAI_PHASE=hook-only requires an existing $IdentityPath with api_key; run without RUNAI_PHASE first"
    }
    Write-RunaiServerPin
    Write-Host ""
}

# 2) Write the hook wrapper. Reads Claude Code's hook JSON from stdin,
#    forwards to the server with X-Runai-User header, prints server
#    response on stdout. 30s timeout + silent failure so a slow /
#    unreachable server never blocks the user's Claude Code prompt.
if ($DoHook) {
    $hookBody = @"
# Auto-generated by runai-client-install.ps1. Overwritten on reinstall.
# Force UTF-8 on stdin / stdout / Invoke-RestMethod body - the Claude Code
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

`$RunaiServer = "$ServerUrl"
function Resolve-RunaiProfileRoot {
    if (-not [string]::IsNullOrWhiteSpace(`$env:USERPROFILE)) { return `$env:USERPROFILE }
    if (-not [string]::IsNullOrWhiteSpace(`$env:HOME)) { return `$env:HOME }
    if (-not [string]::IsNullOrWhiteSpace(`$env:HOMEDRIVE) -and -not [string]::IsNullOrWhiteSpace(`$env:HOMEPATH)) { return "`$env:HOMEDRIVE`$env:HOMEPATH" }
    return ""
}
`$RunaiProfileRoot = Resolve-RunaiProfileRoot
`$RunaiIdentity = Join-Path `$RunaiProfileRoot ".runai-identity"
`$RunaiServerPin = Join-Path `$RunaiProfileRoot ".runai-server.json"

# Self-signed HTTPS: skip CA validation (mirrors the bash hook's
# curl --insecure). PS 7+ uses -SkipCertificateCheck; PS 5.1 uses the
# legacy callback + TLS 1.2. Plain HTTP needs none of this.
if (`$RunaiServer -like 'https://*') {
    if (`$PSVersionTable.PSVersion.Major -ge 6) {
        `$PSDefaultParameterValues['Invoke-RestMethod:SkipCertificateCheck'] = `$true
    } else {
        [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12
        [System.Net.ServicePointManager]::ServerCertificateValidationCallback = { `$true }
    }
}

# Best-effort: pull api_key from ~/.runai-identity so the hook sends
# Authorization: Bearer <key>. Missing file or parse error: fall back
# to legacy X-Runai-User-only mode so existing setups keep working.
`$RunaiApiKey = ""
if (Test-Path `$RunaiIdentity) {
    try {
        `$identity = Get-Content `$RunaiIdentity -Raw | ConvertFrom-Json
        if (`$identity.api_key) { `$RunaiApiKey = `$identity.api_key }
    } catch { `$RunaiApiKey = "" }
}

function Test-RunaiServerPin {
    if (-not (`$RunaiServer -like 'https://*')) { return `$true }
    if (-not (Test-Path `$RunaiServerPin)) {
        Write-Error "runai-hook: missing `$RunaiServerPin - refusing to forward prompt over HTTPS"
        return `$false
    }
    try {
        `$pin = Get-Content `$RunaiServerPin -Raw | ConvertFrom-Json
        `$expected = [string]`$pin.fingerprint
        `$scheme = [string]`$pin.scheme
    } catch {
        Write-Error "runai-hook: could not parse `$RunaiServerPin - refusing to forward prompt"
        return `$false
    }
    if (`$scheme -ne "https" -or [string]::IsNullOrWhiteSpace(`$expected)) {
        Write-Error "runai-hook: no HTTPS fingerprint pin found - refusing to forward prompt"
        return `$false
    }
    try {
        `$live = Invoke-RestMethod -Method Get -Uri "`$RunaiServer/api/tls/fingerprint" -TimeoutSec 5
        `$actual = [string]`$live.fingerprint
    } catch {
        Write-Error "runai-hook: could not retrieve live server fingerprint from `$RunaiServer - refusing to forward prompt"
        return `$false
    }
    if ([string]::IsNullOrWhiteSpace(`$actual)) {
        Write-Error "runai-hook: live server fingerprint was blank - refusing to forward prompt"
        return `$false
    }
    if (`$actual -ne `$expected) {
        Write-Error "runai-hook: server fingerprint mismatch - refusing to forward prompt"
        Write-Error "  pinned: `$expected"
        Write-Error "  live  : `$actual"
        Write-Error "  if you rotated the server cert, re-run the install script to refresh the pin."
        return `$false
    }
    return `$true
}

if (-not (Test-RunaiServerPin)) { exit 1 }

try {
    # Read stdin as raw UTF-8 bytes (don't trust [Console]::In default
    # codepage even after setting InputEncoding above - re-open the
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
    Write-Step "2/5" "install hook wrapper"
    [System.IO.File]::WriteAllText($HookPath, $hookBody, $utf8NoBom)
    Write-Ok ("wrote " + $HookPath)
    Write-Host ""
}

if ($DoSettings) {
    Write-Step "3/5" "patch Claude Code settings"
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
    # codepage to UTF-8 BEFORE PowerShell takes stdin - without it the JSON
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

    # settings.json also UTF-8 no BOM - Claude Code parses it with UTF-8 by
    # default and a BOM trips some JSON readers.
    [System.IO.File]::WriteAllText($SettingsPath, ($data | ConvertTo-Json -Depth 20), $utf8NoBom)
    Write-Host ""
}

# PLANNING section 1.6 - install the runai-client companion command (PowerShell mirror of
# the bash client). The .ps1 carries the implementation; the .cmd shim makes
# `runai-client ...` work from cmd.exe / PATH without asking users to type
# the .ps1 suffix.
if ($DoHook) {
    Write-Step "4/5" "install runai-client companion"
    $clientDir = Split-Path $RunaiClientPath
    if (-not (Test-Path -LiteralPath $clientDir)) {
        New-Item -ItemType Directory -Path $clientDir -Force | Out-Null
    }
    $clientBody = @'
# runai-client - companion command for remote runai users on Windows.
# Installed by runai-client-install.ps1. No local runai binary required.

$ErrorActionPreference = "Stop"
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$OutputEncoding = [System.Text.UTF8Encoding]::new($false)

function Resolve-RunaiProfileRoot {
    if (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) { return $env:USERPROFILE }
    if (-not [string]::IsNullOrWhiteSpace($env:HOME)) { return $env:HOME }
    if (-not [string]::IsNullOrWhiteSpace($env:HOMEDRIVE) -and -not [string]::IsNullOrWhiteSpace($env:HOMEPATH)) { return "$env:HOMEDRIVE$env:HOMEPATH" }
    throw "cannot resolve user profile path; set USERPROFILE or HOME"
}

$RunaiProfileRoot = Resolve-RunaiProfileRoot
$DefaultIdentityPath = Join-Path $RunaiProfileRoot ".runai-identity"
$RunaiIdentityPath = if ($env:RUNAI_IDENTITY) { $env:RUNAI_IDENTITY } else { $DefaultIdentityPath }
$RunaiServerPinPath = Join-Path $RunaiProfileRoot ".runai-server.json"
$RunaiLocalManifest = if ($env:RUNAI_LOCAL_SKILLS) { $env:RUNAI_LOCAL_SKILLS } else { Join-Path $RunaiProfileRoot ".runai-local-skills" }

function Enable-RunaiTlsSkip {
    param([string]$Server)
    if ($Server -like 'https://*') {
        if ($PSVersionTable.PSVersion.Major -ge 6) {
            $PSDefaultParameterValues['Invoke-RestMethod:SkipCertificateCheck'] = $true
            $PSDefaultParameterValues['Invoke-WebRequest:SkipCertificateCheck'] = $true
        } else {
            [System.Net.ServicePointManager]::SecurityProtocol = [System.Net.SecurityProtocolType]::Tls12
            [System.Net.ServicePointManager]::ServerCertificateValidationCallback = { $true }
        }
    }
}

function Read-RunaiIdentity {
    if (-not (Test-Path -LiteralPath $RunaiIdentityPath)) { return $null }
    try {
        return Get-Content -LiteralPath $RunaiIdentityPath -Raw -Encoding UTF8 | ConvertFrom-Json
    } catch {
        return $null
    }
}

function Resolve-RunaiServer {
    if (-not [string]::IsNullOrWhiteSpace($env:RUNAI_SERVER)) {
        return $env:RUNAI_SERVER.TrimEnd('/')
    }
    $identity = Read-RunaiIdentity
    if ($null -ne $identity -and $identity.server) {
        return ([string]$identity.server).TrimEnd('/')
    }
    return ""
}

function Resolve-RunaiApiKey {
    if (-not [string]::IsNullOrWhiteSpace($env:RUNAI_API_KEY)) {
        return $env:RUNAI_API_KEY
    }
    $identity = Read-RunaiIdentity
    if ($null -ne $identity -and $identity.api_key) {
        return [string]$identity.api_key
    }
    return ""
}

function Get-RunaiAuthHeaders {
    $key = Resolve-RunaiApiKey
    if ([string]::IsNullOrWhiteSpace($key)) { return @{} }
    return @{ Authorization = "Bearer $key" }
}

function Assert-RunaiServerPin {
    param([string]$Server)
    if (-not ($Server -like 'https://*')) { return }
    if (-not (Test-Path -LiteralPath $RunaiServerPinPath)) {
        throw "server fingerprint pin missing at $RunaiServerPinPath - re-run install.ps1"
    }
    try {
        $pin = Get-Content -LiteralPath $RunaiServerPinPath -Raw -Encoding UTF8 | ConvertFrom-Json
        $expected = [string]$pin.fingerprint
        $scheme = [string]$pin.scheme
    } catch {
        throw "could not parse server fingerprint pin at $RunaiServerPinPath"
    }
    if ($scheme -ne "https" -or [string]::IsNullOrWhiteSpace($expected)) {
        throw "HTTPS server has no usable fingerprint pin - re-run install.ps1"
    }
    try {
        $live = Invoke-RestMethod -Method Get -Uri "$Server/api/tls/fingerprint" -TimeoutSec 5
        $actual = [string]$live.fingerprint
    } catch {
        throw "could not retrieve live server fingerprint from $Server"
    }
    if ([string]::IsNullOrWhiteSpace($actual)) {
        throw "live server fingerprint was blank"
    }
    if ($actual -ne $expected) {
        throw "server fingerprint mismatch - refusing to contact $Server"
    }
}

function Require-RunaiServer {
    $server = Resolve-RunaiServer
    if ([string]::IsNullOrWhiteSpace($server)) {
        throw "no server URL - set RUNAI_SERVER or run install.ps1 first"
    }
    Enable-RunaiTlsSkip $server
    Assert-RunaiServerPin $server
    return $server
}

function Test-RunaiSafeSkillName {
    param([string]$Name)
    if ([string]::IsNullOrWhiteSpace($Name)) { return $false }
    return ($Name -match '^[A-Za-z0-9_-]+$' -and $Name -ne '.' -and $Name -ne '..')
}

function Test-RunaiLocalManifestContains {
    param([string]$Target, [string]$Name)
    if (-not (Test-Path -LiteralPath $RunaiLocalManifest)) { return $false }
    foreach ($line in Get-Content -LiteralPath $RunaiLocalManifest -Encoding UTF8) {
        if ($line -eq "$Target`t$Name") { return $true }
    }
    return $false
}

function Get-RunaiTargetDir {
    param([string]$Target)
    $root = if ($env:APPDATA) { $env:APPDATA } else { $RunaiProfileRoot }
    switch ($Target) {
        "claude"   { return (Join-Path $root "claude\skills") }
        "codex"    { return (Join-Path $root "codex\skills") }
        "gemini"   { return (Join-Path $root "gemini\skills") }
        "opencode" { return (Join-Path $root "opencode\skills") }
        default    { throw "unknown target '$Target' (use claude,codex,gemini,opencode,all)" }
    }
}

function Get-RunaiDefaultTargets {
    $targets = @()
    foreach ($t in @("claude", "codex", "gemini", "opencode")) {
        $dir = Get-RunaiTargetDir $t
        $home = Split-Path $dir
        if (Test-Path -LiteralPath $home) { $targets += $t }
    }
    if ($targets.Count -eq 0) { $targets += "claude" }
    return $targets
}

function ConvertTo-RunaiTargetList {
    param([string]$TargetArg)
    if ([string]::IsNullOrWhiteSpace($TargetArg) -or $TargetArg.ToLowerInvariant() -eq "all") {
        return Get-RunaiDefaultTargets
    }
    return $TargetArg.Split(",") | ForEach-Object { $_.Trim().ToLowerInvariant() } | Where-Object { $_ }
}

function Write-RunaiUsage {
    Write-Host @"
runai-client - remote companion command (PowerShell) for the runai server.

Usage:
  runai-client <subcommand> [options]

Subcommands:
  upload      Pack a skill dir to tar.gz and POST to /api/community/upload.
  list        List skills currently on the server's community pool.
  install     Install a community skill into your private server pool:
              runai-client install <uploader_uid> <name>
  get         Install a server skill into local CLI agent skills dirs:
              runai-client get <name> [--target claude,codex,gemini,opencode|all]
  --help, -h  Print this help and exit.

Environment:
  RUNAI_SERVER        Server base URL (overrides ~/.runai-identity).
  RUNAI_API_KEY       Bearer key (overrides ~/.runai-identity).
  RUNAI_IDENTITY      Identity file path (default ~/.runai-identity).
  RUNAI_LOCAL_SKILLS  Local get manifest (default ~/.runai-local-skills).
"@
}

function Write-RunaiUploadUsage {
    Write-Host @"
runai-client upload - pack and upload a skill directory.

Usage:
  runai-client upload
  runai-client upload --path <dir> --name <name>

Interactive mode scans:
  <profile>\.claude\skills\
  .\.claude\skills\

Options:
  --path <dir>   Skill directory to upload (must contain SKILL.md).
  --name <name>  Override skill name (default = directory name).
  --help, -h     Print this help and exit.
"@
}

function Find-RunaiSkillCandidates {
    $roots = @()
    $globalRoot = Join-Path $RunaiProfileRoot ".claude\skills"
    $projectRoot = Join-Path (Get-Location).Path ".claude\skills"
    if (Test-Path -LiteralPath $globalRoot) { $roots += $globalRoot }
    if (Test-Path -LiteralPath $projectRoot) { $roots += $projectRoot }
    foreach ($root in $roots) {
        Get-ChildItem -LiteralPath $root -Directory -ErrorAction SilentlyContinue | ForEach-Object {
            if (Test-Path -LiteralPath (Join-Path $_.FullName "SKILL.md")) {
                $_.FullName
            }
        }
    }
}

function Get-RunaiHttpErrorMessage {
    param(
        [string]$Prefix,
        [System.Management.Automation.ErrorRecord]$ErrorRecord
    )
    $ex = $ErrorRecord.Exception
    $status = ""
    $body = ""
    if ($null -ne $ex.Response) {
        try {
            $statusCode = $ex.Response.StatusCode
            if ($null -ne $statusCode) { $status = [int]$statusCode }
        } catch {}
        try {
            if ($null -ne $ex.Response.Content) {
                $body = $ex.Response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
            }
        } catch {}
        if ([string]::IsNullOrWhiteSpace($body)) {
            try {
                $stream = $ex.Response.GetResponseStream()
                if ($null -ne $stream) {
                    $reader = New-Object System.IO.StreamReader($stream)
                    $body = $reader.ReadToEnd()
                    $reader.Dispose()
                }
            } catch {}
        }
    }
    if ([string]::IsNullOrWhiteSpace($status)) {
        return "${Prefix}: $($ex.Message)"
    }
    if ([string]::IsNullOrWhiteSpace($body)) {
        return "${Prefix}: server returned HTTP $status"
    }
    return "${Prefix}: server returned HTTP $status`n$body"
}

function Invoke-RunaiJson {
    param(
        [string]$Method,
        [string]$Path,
        [object]$Body = $null,
        [string]$ErrorPrefix = "runai-client"
    )
    $server = Require-RunaiServer
    $headers = Get-RunaiAuthHeaders
    $params = @{
        Method = $Method
        Uri = "$server$Path"
        Headers = $headers
    }
    if ($null -ne $Body) {
        $params.ContentType = "application/json; charset=utf-8"
        $params.Body = ($Body | ConvertTo-Json -Depth 20 -Compress)
    }
    try {
        return Invoke-RestMethod @params
    } catch {
        throw (Get-RunaiHttpErrorMessage -Prefix $ErrorPrefix -ErrorRecord $_)
    }
}

function Invoke-RunaiMultipartUpload {
    param(
        [string]$TarPath,
        [string]$Name
    )
    $server = Require-RunaiServer
    $headers = Get-RunaiAuthHeaders
    $boundary = "----runai" + ([guid]::NewGuid().ToString("N"))
    $fileName = ($Name -replace '"', '')
    $encoder = [System.Text.Encoding]::UTF8
    $prefix = "--$boundary`r`nContent-Disposition: form-data; name=`"name`"`r`n`r`n$Name`r`n--$boundary`r`nContent-Disposition: form-data; name=`"bundle`"; filename=`"$fileName.tar.gz`"`r`nContent-Type: application/gzip`r`n`r`n"
    $suffix = "`r`n--$boundary--`r`n"
    $stream = New-Object System.IO.MemoryStream
    $prefixBytes = $encoder.GetBytes($prefix)
    $stream.Write($prefixBytes, 0, $prefixBytes.Length)
    $fileBytes = [System.IO.File]::ReadAllBytes($TarPath)
    $stream.Write($fileBytes, 0, $fileBytes.Length)
    $suffixBytes = $encoder.GetBytes($suffix)
    $stream.Write($suffixBytes, 0, $suffixBytes.Length)
    $bodyBytes = $stream.ToArray()
    $stream.Dispose()
    try {
        return Invoke-RestMethod -Method Post -Uri "$server/api/community/upload" -Headers $headers -ContentType "multipart/form-data; boundary=$boundary" -Body $bodyBytes
    } catch {
        throw (Get-RunaiHttpErrorMessage -Prefix "runai-client upload" -ErrorRecord $_)
    }
}

function Invoke-RunaiList {
    param([string[]]$Rest)
    $sort = "installs"
    $offset = 0
    $limit = 50
    for ($i = 0; $i -lt $Rest.Count; $i++) {
        switch ($Rest[$i]) {
            "--sort"   { $i++; $sort = $Rest[$i] }
            "--offset" { $i++; $offset = [int]$Rest[$i] }
            "--limit"  { $i++; $limit = [int]$Rest[$i] }
            "--help"   { Write-Host "runai-client list [--sort installs|created|name] [--offset N] [--limit N]"; return }
            "-h"       { Write-Host "runai-client list [--sort installs|created|name] [--offset N] [--limit N]"; return }
            default    { throw "runai-client list: unknown arg '$($Rest[$i])'" }
        }
    }
    $query = "?sort=$([uri]::EscapeDataString($sort))&offset=$offset&limit=$limit"
    $data = Invoke-RunaiJson -Method Get -Path "/api/community/list$query" -ErrorPrefix "runai-client list"
    "{0,-28} {1,-14} {2,-14} {3,8}" -f "NAME", "UPLOADER", "VERSION", "INSTALLS"
    "-" * 70
    foreach ($s in @($data.items)) {
        $name = ([string]$s.name)
        $uid = ([string]$s.uploader_uid)
        $version = ([string]$s.version)
        if ($name.Length -gt 26) { $name = $name.Substring(0, 26) }
        if ($uid.Length -gt 12) { $uid = $uid.Substring(0, 12) }
        if ($version.Length -gt 12) { $version = $version.Substring(0, 12) }
        "{0,-28} {1,-14} {2,-14} {3,8}" -f $name, $uid, $version, ([int]$s.installs_total)
    }
    "-" * 70
    "total: $($data.total)  offset: $($data.offset)  limit: $($data.limit)"
}

function Invoke-RunaiInstall {
    param([string[]]$Rest)
    if ($Rest.Count -eq 1 -and ($Rest[0] -eq "--help" -or $Rest[0] -eq "-h")) {
        Write-Host "runai-client install <uploader_uid> <name>"
        return
    }
    if ($Rest.Count -ne 2) { throw "usage: runai-client install <uploader_uid> <name>" }
    $uid = [uri]::EscapeDataString($Rest[0])
    $name = [uri]::EscapeDataString($Rest[1])
    Invoke-RunaiJson -Method Post -Path "/api/community/install/$uid/$name" -ErrorPrefix "runai-client install" | ConvertTo-Json -Depth 20
}

function Invoke-RunaiUpload {
    param([string[]]$Rest)
    $pathArg = ""
    $nameArg = ""
    for ($i = 0; $i -lt $Rest.Count; $i++) {
        switch ($Rest[$i]) {
            "--path" { $i++; $pathArg = $Rest[$i] }
            "--name" { $i++; $nameArg = $Rest[$i] }
            "--help" { Write-RunaiUploadUsage; return }
            "-h"     { Write-RunaiUploadUsage; return }
            default  { throw "runai-client upload: unknown arg '$($Rest[$i])'" }
        }
    }
    if ([string]::IsNullOrWhiteSpace($pathArg)) {
        if (-not (Get-Command fzf -ErrorAction SilentlyContinue)) {
            throw "runai-client upload: pass --path <skill-dir> --name <skill-name> (fzf not installed for interactive selection)"
        }
        $selected = Find-RunaiSkillCandidates | fzf --prompt "select skill dir > "
        if ([string]::IsNullOrWhiteSpace($selected)) { throw "runai-client upload: nothing selected" }
        $pathArg = $selected
    }
    $skillPath = (Resolve-Path -LiteralPath $pathArg).Path
    if (-not (Test-Path -LiteralPath $skillPath -PathType Container)) {
        throw "runai-client upload: --path is not a directory: $pathArg"
    }
    if (-not (Test-Path -LiteralPath (Join-Path $skillPath "SKILL.md"))) {
        throw "runai-client upload: $skillPath does not contain SKILL.md"
    }
    if ([string]::IsNullOrWhiteSpace($nameArg)) {
        $nameArg = Split-Path $skillPath -Leaf
    }
    if (-not (Test-RunaiSafeSkillName $nameArg)) {
        throw "runai-client upload: unsafe skill name '$nameArg'"
    }
    $tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) ("runai-upload-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null
    $tarPath = Join-Path $tmpDir "$nameArg.tar.gz"
    try {
        $parent = Split-Path $skillPath
        $leaf = Split-Path $skillPath -Leaf
        & tar -czf $tarPath -C $parent $leaf
        if ($LASTEXITCODE -ne 0) { throw "tar failed with exit code $LASTEXITCODE" }
        Write-Host "uploading $skillPath as name=$nameArg"
        Invoke-RunaiMultipartUpload -TarPath $tarPath -Name $nameArg | ConvertTo-Json -Depth 20
        Write-Host "OK uploaded."
    } finally {
        Remove-Item -LiteralPath $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-RunaiGet {
    param([string[]]$Rest)
    $name = ""
    $targetArg = ""
    for ($i = 0; $i -lt $Rest.Count; $i++) {
        switch ($Rest[$i]) {
            "--target" { $i++; $targetArg = $Rest[$i] }
            "--help" {
                Write-Host "runai-client get <name> [--target claude,codex,gemini,opencode|all]"
                return
            }
            "-h" {
                Write-Host "runai-client get <name> [--target claude,codex,gemini,opencode|all]"
                return
            }
            default {
                if ($Rest[$i].StartsWith("-")) { throw "runai-client get: unknown option '$($Rest[$i])'" }
                if ([string]::IsNullOrWhiteSpace($name)) { $name = $Rest[$i] } else { throw "runai-client get: extra arg '$($Rest[$i])'" }
            }
        }
    }
    if ([string]::IsNullOrWhiteSpace($name)) {
        if (-not (Get-Command fzf -ErrorAction SilentlyContinue)) {
            throw "runai-client get: provide a skill name (fzf not installed for picker)"
        }
        $data = Invoke-RunaiJson -Method Get -Path "/api/skills"
        $name = @($data.skills | ForEach-Object { $_.name }) | fzf --prompt "install skill locally> "
        if ([string]::IsNullOrWhiteSpace($name)) { throw "runai-client get: nothing selected" }
    }
    if (-not (Test-RunaiSafeSkillName $name)) {
        throw "runai-client get: unsafe skill name '$name'"
    }

    $server = Require-RunaiServer
    $headers = Get-RunaiAuthHeaders
    $tmp = [System.IO.Path]::GetTempFileName()
    try {
        $encoded = [uri]::EscapeDataString($name)
        Invoke-WebRequest -Method Get -Uri "$server/skills/bundle/$encoded" -Headers $headers -OutFile $tmp | Out-Null
        $targets = ConvertTo-RunaiTargetList $targetArg
        $manifestDir = Split-Path $RunaiLocalManifest
        if (-not [string]::IsNullOrWhiteSpace($manifestDir) -and -not (Test-Path -LiteralPath $manifestDir)) {
            New-Item -ItemType Directory -Path $manifestDir -Force | Out-Null
        }
        $installed = 0
        foreach ($target in $targets) {
            $dir = Get-RunaiTargetDir $target
            New-Item -ItemType Directory -Path $dir -Force | Out-Null
            $dest = Join-Path $dir $name
            if (Test-Path -LiteralPath $dest) {
                if (-not (Test-RunaiLocalManifestContains -Target $target -Name $name)) {
                    Write-Warning "refusing to overwrite untracked local skill: $dest"
                    continue
                }
                Remove-Item -LiteralPath $dest -Recurse -Force
            }
            & tar -xzf $tmp -C $dir
            if ($LASTEXITCODE -ne 0) {
                Write-Warning "failed to extract $name into $dir"
                continue
            }
            [System.IO.File]::AppendAllText(
                $RunaiLocalManifest,
                "$target`t$name`r`n",
                [System.Text.UTF8Encoding]::new($false)
            )
            Write-Host "  installed $name -> $dest"
            $installed += 1
        }
        if ($installed -eq 0) { throw "runai-client get: nothing installed" }
        Write-Host "OK $name installed to $installed local agent(s)."
    } finally {
        Remove-Item -LiteralPath $tmp -Force -ErrorAction SilentlyContinue
    }
}

try {
    $cmd = if ($args.Count -gt 0) { $args[0] } else { "" }
    $rest = if ($args.Count -gt 1) { $args[1..($args.Count - 1)] } else { @() }
    switch ($cmd) {
        ""        { Write-RunaiUsage }
        "--help"  { Write-RunaiUsage }
        "-h"      { Write-RunaiUsage }
        "help"    { Write-RunaiUsage }
        "upload"  { Invoke-RunaiUpload -Rest $rest }
        "list"    { Invoke-RunaiList -Rest $rest }
        "install" { Invoke-RunaiInstall -Rest $rest }
        "get"     { Invoke-RunaiGet -Rest $rest }
        default   { throw "runai-client: unknown subcommand '$cmd'" }
    }
} catch {
    Write-Error $_.Exception.Message
    exit 1
}
'@
    [System.IO.File]::WriteAllText($RunaiClientPath, $clientBody, $utf8NoBom)
    $shimBody = @"
@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0runai-client.ps1" %*
"@
    [System.IO.File]::WriteAllText($RunaiClientShimPath, $shimBody, $utf8NoBom)
    Write-Ok ("wrote " + $RunaiClientPath)
    Write-Ok ("wrote " + $RunaiClientShimPath)
    Write-Dim ("  add to PATH if needed: " + (Split-Path $RunaiClientPath))
    Write-Host ""
}

# PLANNING section 1.6 - register the runai-client remote HTTP MCP into Claude Code's
# %USERPROFILE%\.claude.json under mcpServers (the MCP leg of the
# "runai-client trio"). Claude Code talks to the server's streamable-HTTP
# MCP at <ServerUrl>/mcp, authenticating with the api_key from
# ~/.runai-identity as `Authorization: Bearer <key>`:
#
#   "mcpServers": {
#     "runai-client": {
#       "type": "http",
#       "url": "<ServerUrl>/mcp",
#       "headers": { "Authorization": "Bearer <api_key>" }
#     }
#   }
#
# Existing mcpServers entries and unrelated top-level keys are preserved;
# the original is backed up to .runai-bak first. Skipped (with a warning)
# when no api_key is available.
if ($DoHook) {
    Write-Step "5/5" "register remote MCP"
    $ClaudeJsonPath = Join-Path $RunaiProfileRoot ".claude.json"
    $McpApiKey = ""
    if (Test-Path -LiteralPath $IdentityPath) {
        try {
            $idForMcp = Get-Content -LiteralPath $IdentityPath -Raw -Encoding UTF8 | ConvertFrom-Json
            if ($idForMcp.api_key) { $McpApiKey = [string]$idForMcp.api_key }
        } catch { $McpApiKey = "" }
    }

    if ([string]::IsNullOrEmpty($McpApiKey)) {
        Write-Warn2 "no api_key in $IdentityPath - skipping remote MCP registration"
    } else {
        if (Test-Path -LiteralPath $ClaudeJsonPath) {
            Copy-Item -LiteralPath $ClaudeJsonPath -Destination "$ClaudeJsonPath.runai-bak" -Force
            $rawClaude = Get-Content -LiteralPath $ClaudeJsonPath -Raw
            if ([string]::IsNullOrWhiteSpace($rawClaude)) { $rawClaude = "{}" }
            try {
                $claudeData = ConvertTo-RunaiHashtable ($rawClaude | ConvertFrom-Json)
            } catch {
                $claudeData = @{}
            }
        } else {
            $claudeData = @{}
        }
        if ($null -eq $claudeData -or -not ($claudeData -is [hashtable])) { $claudeData = @{} }
        if (-not $claudeData.ContainsKey('mcpServers') -or -not ($claudeData.mcpServers -is [hashtable])) {
            $claudeData.mcpServers = @{}
        }
        $claudeData.mcpServers['runai-client'] = @{
            type    = "http"
            url     = "$ServerUrl/mcp"
            headers = @{ Authorization = "Bearer $McpApiKey" }
        }
        [System.IO.File]::WriteAllText($ClaudeJsonPath, ($claudeData | ConvertTo-Json -Depth 20), $utf8NoBom)
        Write-Ok ("registered remote MCP runai-client at $ServerUrl/mcp")
        Write-Host ""
    }
}
# === RUNAI_SECTION:team-only END ===

Write-Hr
Write-Host ("  " + (Runai-Style "38;5;114m" (Runai-Style "1m" "all set.")) + "  open a " + (Runai-Style "1m" "new") + " Claude Code session and your prompts")
Write-Host ("  will route through " + (Runai-Style "38;5;81m" $ServerUrl))
Write-Hr
Write-Dim ("  dashboard   $ServerUrl")
Write-Dim ("  uninstall   irm $ServerUrl/uninstall.ps1 | iex")
Write-Dim ("  switch user del `"$IdentityPath`" && re-run installer")
Write-Host ""

# ==== completion summary, printed unconditionally ====
# Plain field names so an agent or a human can grep them. Username is
# shown plain; password and raw api_key are never printed. The final
# plain line is intentionally stable: tests pin it as `install complete`.
$maskedKey = ""
$account = if ($env:RUNAI_USERNAME) { $env:RUNAI_USERNAME } elseif ($env:USERNAME) { $env:USERNAME } else { "<reused>" }
if (Test-Path -LiteralPath $IdentityPath) {
  try {
    $idObj = Get-Content -LiteralPath $IdentityPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($idObj.PSObject.Properties.Name -contains 'username' -and $idObj.username) {
      $account = [string]$idObj.username
    }
    if ($idObj.PSObject.Properties.Name -contains 'api_key' -and $idObj.api_key) {
      $k = [string]$idObj.api_key
      if ($k.Length -gt 8) { $maskedKey = "$($k.Substring(0,4))..$($k.Substring($k.Length-2))" }
      elseif ($k.Length -gt 0) { $maskedKey = "$($k.Substring(0,2)).." }
    }
  } catch {}
}
Write-Hr
Write-Host ((Runai-Style "38;5;81m" "|") + "  " + (Runai-Style "38;5;114m" (Runai-Style "1m" "install complete")))
Write-Dim ("  account   $account")
Write-Dim ("  password  <hidden>")
Write-Dim ("  api_key   $(if ($maskedKey) { $maskedKey } else { '<skipped>' })")
Write-Dim ("  server    $ServerUrl")
Write-Dim ("  identity  $IdentityPath")
Write-Dim ("  hook      $HookPath")
Write-Dim ("  config    $SettingsPath")
Write-Dim ("  client    $RunaiClientPath")
Write-Hr
Write-Host "install complete"

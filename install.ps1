<#
runai installer (Windows / PowerShell)

Detects your platform, downloads the matching release binary from GitHub,
verifies its checksum, installs it on PATH, and optionally wires up the
edition you pick.

Interactive:
  irm https://raw.githubusercontent.com/Crosery/runai/main/install.ps1 | iex

Non-interactive (agents / CI) — pass params via a scriptblock:
  & ([scriptblock]::Create((irm https://raw.githubusercontent.com/Crosery/runai/main/install.ps1))) -Edition personal -Yes

Params:
  -Edition personal|team   personal = local router + hook; team = run the server
  -Version <tag|latest>    release to install (default: latest stable)
  -BinDir <path>           where to put runai.exe (default: auto)
  -Yes                     non-interactive; take defaults, never prompt
  -NoHook                  skip installing the Claude Code hook
  -NoSetup                 skip the post-install edition setup entirely
  -DryRun                  print what would happen, change nothing
  -Uninstall               remove runai.exe from its bin dir
#>
[CmdletBinding()]
param(
  [ValidateSet('personal','team','')] [string]$Edition = '',
  [string]$Version = 'latest',
  [string]$BinDir  = '',
  [switch]$Yes,
  [switch]$NoHook,
  [switch]$NoSetup,
  [switch]$DryRun,
  [switch]$Uninstall
)

$ErrorActionPreference = 'Stop'
$Repo = 'Crosery/runai'
$ReleaseBase = "https://github.com/$Repo/releases"

function Step($m) { Write-Host "==> " -ForegroundColor Blue -NoNewline; Write-Host $m }
function Ok($m)   { Write-Host "  [ok] "   -ForegroundColor Green  -NoNewline; Write-Host $m }
function Warn($m) { Write-Host "  [warn] " -ForegroundColor Yellow -NoNewline; Write-Host $m }
function Die($m)  { Write-Host "  [error] " -ForegroundColor Red -NoNewline; Write-Host $m; exit 1 }
function Banner {
  Write-Host ""
  Write-Host "  +--------------------------------------------+" -ForegroundColor Blue
  Write-Host "  |   runai installer  -  AI skill/MCP manager |" -ForegroundColor Blue
  Write-Host "  +--------------------------------------------+" -ForegroundColor Blue
  Write-Host ""
}
function Run($block, $desc) {
  Write-Host "    > $desc" -ForegroundColor DarkGray
  if (-not $DryRun) { & $block }
}

Banner

# ---------- platform ----------
$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -eq 'AMD64' -or $arch -eq 'x86') { $ArchName = 'amd64' }
elseif ($arch -eq 'ARM64') { Die "windows arm64 has no prebuilt release yet; build from source with 'cargo install --git https://github.com/$Repo'" }
else { Die "unsupported arch: $arch" }
$Asset = "runai-windows-$ArchName.zip"
Ok "platform: windows/$ArchName  asset: $Asset"

# ---------- bin dir ----------
if (-not $BinDir) {
  foreach ($d in @("$env:USERPROFILE\.cargo\bin", "$env:LOCALAPPDATA\runai\bin")) {
    if (Test-Path $d) { $BinDir = $d; break }
  }
  if (-not $BinDir) { $BinDir = "$env:LOCALAPPDATA\runai\bin" }
}
Ok "install dir: $BinDir"

# ---------- uninstall ----------
if ($Uninstall) {
  $t = Join-Path $BinDir 'runai.exe'
  if (Test-Path $t) {
    Step "Removing $t"
    Run { Remove-Item -Force $t } "Remove-Item $t"
    Ok "runai.exe removed. Unwire hooks first with 'runai recommend uninstall-hook' / 'runai server --uninstall-hook' if needed."
  } else { Warn "no runai.exe at $t" }
  exit 0
}

# ---------- resolve URLs ----------
if ($Version -eq 'latest') {
  $url  = "$ReleaseBase/latest/download/$Asset"
  $sums = "$ReleaseBase/latest/download/checksums.txt"
} else {
  $url  = "$ReleaseBase/download/$Version/$Asset"
  $sums = "$ReleaseBase/download/$Version/checksums.txt"
}

$tmp = Join-Path $env:TEMP ("runai-install-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
  $zip = Join-Path $tmp $Asset
  Step "Downloading $Asset ($Version)"
  Run { Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing } "Invoke-WebRequest $url"

  Step "Verifying checksum"
  if (-not $DryRun) {
    try {
      $sumFile = Join-Path $tmp 'checksums.txt'
      Invoke-WebRequest -Uri $sums -OutFile $sumFile -UseBasicParsing
      $line = (Get-Content $sumFile | Where-Object { $_ -match [regex]::Escape($Asset) } | Select-Object -First 1)
      if ($line) {
        $want = ($line -split '\s+')[0].ToLower()
        $got  = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLower()
        if ($want -ne $got) { Die "checksum mismatch: expected $want got $got" }
        Ok "sha256 verified"
      } else { Warn "no checksum entry for $Asset — continuing" }
    } catch { Warn "checksums.txt not found — skipping verification" }
  }

  Step "Installing binary to $BinDir"
  if (-not (Test-Path $BinDir)) { Run { New-Item -ItemType Directory -Path $BinDir -Force | Out-Null } "mkdir $BinDir" }
  if (-not $DryRun) {
    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    Copy-Item -Force (Join-Path $tmp 'runai.exe') (Join-Path $BinDir 'runai.exe')
  }
  $Runai = Join-Path $BinDir 'runai.exe'
  if (-not $DryRun) { Ok ("installed: " + (& $Runai --version 2>$null)) }

  # PATH hint (user PATH)
  $userPath = [Environment]::GetEnvironmentVariable('Path','User')
  if ($userPath -notlike "*$BinDir*") {
    if ($Yes -and -not $DryRun) {
      [Environment]::SetEnvironmentVariable('Path', "$userPath;$BinDir", 'User')
      Ok "added $BinDir to your user PATH (restart the shell to pick it up)"
    } else {
      Warn "PATH: add $BinDir to your user PATH, or run with -Yes to do it automatically"
    }
  }

  # ---------- edition setup ----------
  if ($NoSetup) { Ok "skipping edition setup (-NoSetup). Binary is installed."; exit 0 }

  if (-not $Edition) {
    if ($Yes) { $Edition = 'personal' }
    else {
      Write-Host ""
      Write-Host "Pick an edition:"
      Write-Host "  1) personal - local router + Claude Code hook, no account, no server"
      Write-Host "  2) team     - run the multi-user dashboard server"
      $r = Read-Host "choice [1]"
      if ($r -eq '2' -or $r -eq 'team') { $Edition = 'team' } else { $Edition = 'personal' }
    }
  }
  Step "Edition: $Edition"

  switch ($Edition) {
    'personal' {
      if (-not $Yes) {
        $c = Read-Host "Configure the LLM skill router now (provider + API key)? [Y/n]"
        if ($c -notmatch '^[nN]') { Run { & $Runai recommend setup } "runai recommend setup" }
        else { Warn "skipped — run 'runai recommend setup' later" }
      } else { Warn "non-interactive: run 'runai recommend setup' to configure the router (needs an API key)" }
      if (-not $NoHook) {
        Step "Wiring Claude Code hooks (idempotent)"
        Run { & $Runai recommend install-hook } "runai recommend install-hook"
        Run { & $Runai server --install-hook } "runai server --install-hook"
      }
      Ok "personal edition ready. Launch the TUI with: runai"
    }
    'team' {
      Step "Team edition: enabling the dashboard server at login"
      Run { & $Runai server --install-autostart } "runai server --install-autostart"
      Ok "Open http://127.0.0.1:17888 and register — the first account becomes admin."
      Write-Host "  Remote clients connect with:  irm http://<this-host>:17888/install.ps1 | iex"
    }
    default { Die "unknown edition: $Edition" }
  }

  Write-Host ""
  Write-Host "Done. " -ForegroundColor Green -NoNewline
  Write-Host ("runai " + $(if ($DryRun) { '(dry-run)' } else { '' }))
}
finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

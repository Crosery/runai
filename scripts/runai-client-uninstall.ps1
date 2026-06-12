# runai client uninstall - Windows / PowerShell.
#
# Usage:
#   irm http://<SERVER>:<PORT>/uninstall.ps1 | iex
#
# Reverses runai-client-install.ps1: removes the UserPromptSubmit hook
# entry pointing at ~/.runai-hook.ps1, drops the runai-client remote MCP
# entry, deletes the hook + companion CLI + server fingerprint pin, and
# removes only the local skills recorded by runai-client get in
# ~/.runai-local-skills.
# Safe to run if you never installed - every step is idempotent.
# Backs up the prior settings.json / claude.json to .runai-uninstall-bak.

function Resolve-RunaiProfileRoot {
    if (-not [string]::IsNullOrWhiteSpace($env:USERPROFILE)) { return $env:USERPROFILE }
    if (-not [string]::IsNullOrWhiteSpace($env:HOME)) { return $env:HOME }
    if (-not [string]::IsNullOrWhiteSpace($env:HOMEDRIVE) -and -not [string]::IsNullOrWhiteSpace($env:HOMEPATH)) { return "$env:HOMEDRIVE$env:HOMEPATH" }
    throw "runai-uninstall: cannot resolve user profile path; set USERPROFILE or HOME"
}

$RunaiProfileRoot = Resolve-RunaiProfileRoot
$HookPath = Join-Path $RunaiProfileRoot ".runai-hook.ps1"
$ServerPinPath = Join-Path $RunaiProfileRoot ".runai-server.json"
$SettingsPath = Join-Path (Join-Path $RunaiProfileRoot ".claude") "settings.json"
$ClaudeJsonPath = Join-Path $RunaiProfileRoot ".claude.json"
$RunaiClientPath = Join-Path (Join-Path $RunaiProfileRoot ".local\bin") "runai-client.ps1"
$RunaiClientShimPath = Join-Path (Join-Path $RunaiProfileRoot ".local\bin") "runai-client.cmd"
$LocalManifestPath = Join-Path $RunaiProfileRoot ".runai-local-skills"

Write-Host "runai client uninstall (Windows)"
Write-Host ""

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

function Test-RunaiSafeSkillName {
    param([string]$Name)
    if ([string]::IsNullOrWhiteSpace($Name)) { return $false }
    return ($Name -notmatch '[\\/]' -and $Name -ne '.' -and $Name -ne '..')
}

function Get-RunaiTargetDir {
    param([string]$Target)
    $root = if ($env:APPDATA) { $env:APPDATA } else { $RunaiProfileRoot }
    switch ($Target) {
        "claude"   { return (Join-Path $root "claude\skills") }
        "codex"    { return (Join-Path $root "codex\skills") }
        "gemini"   { return (Join-Path $root "gemini\skills") }
        "opencode" { return (Join-Path $root "opencode\skills") }
        default    { return $null }
    }
}

# 1) Strip the hook entry from settings.json. Idempotent.
if (Test-Path $SettingsPath) {
    Copy-Item $SettingsPath "$SettingsPath.runai-uninstall-bak" -Force
    $raw = Get-Content $SettingsPath -Raw
    if ([string]::IsNullOrWhiteSpace($raw)) { $raw = "{}" }
    try {
        $parsed = $raw | ConvertFrom-Json
    } catch {
        Write-Warning "settings.json was not valid JSON, leaving untouched"
        $parsed = $null
    }
    if ($null -ne $parsed) {
        $data = ConvertTo-RunaiHashtable $parsed
        if ($null -ne $data -and $data.ContainsKey('hooks') -and $data.hooks.ContainsKey('UserPromptSubmit')) {
            $removed = 0
            $newUps = @()
            foreach ($g in $data.hooks.UserPromptSubmit) {
                if ($null -eq $g -or -not $g.ContainsKey('hooks')) { $newUps += ,$g; continue }
                $kept = @()
                foreach ($h in @($g.hooks)) {
                    if ($null -ne $h -and $h.command -like "*\.runai-hook.ps1*") {
                        $removed += 1
                    } else {
                        $kept += ,$h
                    }
                }
                if ($kept.Count -gt 0) {
                    $g.hooks = $kept
                    $newUps += ,$g
                } else {
                    # whole group was ours - drop the wrapper too
                    $removed += 1
                }
            }
            if ($newUps.Count -gt 0) {
                $data.hooks.UserPromptSubmit = $newUps
            } else {
                $data.hooks.Remove('UserPromptSubmit')
            }
            $word = if ($removed -eq 1) { 'entry' } else { 'entries' }
            Write-Host "removed $removed runai hook $word from settings.json"
            $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
            [System.IO.File]::WriteAllText($SettingsPath, ($data | ConvertTo-Json -Depth 20), $utf8NoBom)
        } else {
            Write-Host "no runai UserPromptSubmit hook present"
        }
    }
} else {
    Write-Host "no settings.json - nothing to clean"
}

# 2) Remove the runai-client remote MCP entry from ~/.claude.json (PLANNING section 1.6 mcp
#    leg). Only the `runai-client` key under mcpServers is dropped; every
#    other mcpServers entry and top-level key is preserved. Idempotent.
if (Test-Path $ClaudeJsonPath) {
    Copy-Item $ClaudeJsonPath "$ClaudeJsonPath.runai-uninstall-bak" -Force
    $rawClaude = Get-Content $ClaudeJsonPath -Raw
    if ([string]::IsNullOrWhiteSpace($rawClaude)) { $rawClaude = "{}" }
    try {
        $claudeData = ConvertTo-RunaiHashtable ($rawClaude | ConvertFrom-Json)
    } catch {
        Write-Warning "claude.json was not valid JSON, leaving untouched"
        $claudeData = $null
    }
    if ($null -ne $claudeData -and ($claudeData -is [hashtable]) -and `
        $claudeData.ContainsKey('mcpServers') -and ($claudeData.mcpServers -is [hashtable]) -and `
        $claudeData.mcpServers.ContainsKey('runai-client')) {
        $claudeData.mcpServers.Remove('runai-client')
        $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
        [System.IO.File]::WriteAllText($ClaudeJsonPath, ($claudeData | ConvertTo-Json -Depth 20), $utf8NoBom)
        Write-Host "removed runai-client remote MCP from claude.json"
    } else {
        Write-Host "no runai-client remote MCP entry - nothing to clean"
    }
} else {
    Write-Host "no $ClaudeJsonPath - nothing to clean"
}

# 3) Delete the hook wrapper itself.
if (Test-Path $HookPath) {
    Remove-Item $HookPath -Force
    Write-Host "removed $HookPath"
} else {
    Write-Host "no $HookPath - already clean"
}

# 4) Delete the installer-generated server fingerprint pin.
if (Test-Path -LiteralPath $ServerPinPath) {
    Remove-Item -LiteralPath $ServerPinPath -Force
    Write-Host "removed $ServerPinPath"
} else {
    Write-Host "no $ServerPinPath - already clean"
}

# 5) Remove skills materialized by `runai-client get`, tracked as
#    "<target>\t<name>" in ~/.runai-local-skills. This does not scan user
#    skill directories and does not delete untracked skills.
if (Test-Path -LiteralPath $LocalManifestPath) {
    $removedLocal = 0
    foreach ($line in Get-Content -LiteralPath $LocalManifestPath -Encoding UTF8) {
        if ([string]::IsNullOrWhiteSpace($line)) { continue }
        $parts = $line -split "`t", 2
        if ($parts.Count -ne 2) { continue }
        $target = $parts[0]
        $name = $parts[1]
        if (-not (Test-RunaiSafeSkillName $name)) {
            Write-Host "skipped unsafe manifest entry: $target / $name"
            continue
        }
        $targetDir = Get-RunaiTargetDir $target
        if ([string]::IsNullOrWhiteSpace($targetDir)) { continue }
        $skillDir = Join-Path $targetDir $name
        if (Test-Path -LiteralPath $skillDir -PathType Container) {
            Remove-Item -LiteralPath $skillDir -Recurse -Force
            $removedLocal += 1
        }
    }
    Remove-Item -LiteralPath $LocalManifestPath -Force
    Write-Host "removed $removedLocal locally-installed skill(s) + $LocalManifestPath"
} else {
    Write-Host "no locally-installed skills tracked - nothing to clean"
}

# 6) Delete the runai-client companion command and cmd.exe shim.
if (Test-Path -LiteralPath $RunaiClientPath) {
    Remove-Item -LiteralPath $RunaiClientPath -Force
    Write-Host "removed $RunaiClientPath"
} else {
    Write-Host "no $RunaiClientPath - already clean"
}
if (Test-Path -LiteralPath $RunaiClientShimPath) {
    Remove-Item -LiteralPath $RunaiClientShimPath -Force
    Write-Host "removed $RunaiClientShimPath"
} else {
    Write-Host "no $RunaiClientShimPath - already clean"
}

Write-Host ""
Write-Host "done. Claude Code will no longer call runai on UserPromptSubmit."
Write-Host "original settings.json backed up to: $SettingsPath.runai-uninstall-bak"

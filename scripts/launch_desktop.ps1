<#
.SYNOPSIS
    Launch the Call Analyzer Windows desktop app.

.DESCRIPTION
    Prefers a previously built exe. If none exists, starts `tauri dev` so the
    full ingest → transcribe → score → plugins → collector loop is usable
    without waiting for an installer.

    Adds ~/.cargo/bin to PATH for this process if cargo is installed but not
    on the user PATH (common on this machine).
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if (Test-Path (Join-Path $cargoBin 'cargo.exe')) {
    $env:PATH = "$cargoBin;$env:PATH"
}

foreach ($f in @(
        (Join-Path $Root '.env'),
        (Join-Path $env:LOCALAPPDATA 'hermes\.env')
    )) {
    if (-not (Test-Path $f)) { continue }
    Get-Content $f | ForEach-Object {
        if ($_ -match '^\s*#' -or $_ -match '^\s*$') { return }
        if ($_ -match '^([A-Za-z_][A-Za-z0-9_]*)=(.*)$') {
            Set-Item -Path "Env:$($Matches[1])" -Value $Matches[2]
        }
    }
}

$exe = @(
    (Join-Path $Root 'target\release\call-analyzer-desktop.exe'),
    (Join-Path $Root 'target\debug\call-analyzer-desktop.exe')
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if ($exe) {
    Write-Host "Launching $exe"
    Start-Process $exe
    return
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo is not on PATH and no desktop exe was found. Install Rust, then retry."
}

Write-Host "No packaged exe yet — starting tauri dev (full Windows window + hot reload)."
Set-Location (Join-Path $Root 'apps\desktop')
npm run dev

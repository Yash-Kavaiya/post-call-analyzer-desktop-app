<#
.SYNOPSIS
    Downloads the whisper.cpp model weights this project transcribes with.

.DESCRIPTION
    Model weights are build inputs, not source. ggml-small.en.bin alone is
    465 MB — GitHub warns above 50 MB and refuses any single file above
    100 MB, so a repository carrying it cannot be pushed, and CI would clone
    half a gigabyte on every job. They are fetched here instead.

    Each download is verified against a recorded SHA-256 before it is moved
    into place, so a truncated download fails loudly rather than producing
    a model that transcribes garbage.

.PARAMETER Model
    Which weights to fetch. Defaults to small.en, which is what
    ca-asr::WhisperCliConfig is normally pointed at.

    small.en   465 MB  English only. The default; good accuracy, CPU-friendly.
    base.en    142 MB  English only. Faster and noticeably less accurate.
    medium.en  1.5 GB  English only. Slower; use when accuracy matters most.
    small      465 MB  Multilingual. Needed for Hindi and code-switched calls,
                       which the .en models transcribe as though English.

.PARAMETER Force
    Re-download even if the file is already present and verifies.

.EXAMPLE
    pwsh scripts/fetch-models.ps1
    pwsh scripts/fetch-models.ps1 -Model small
    pwsh scripts/fetch-models.ps1 -Model base.en -Force
#>
[CmdletBinding()]
param(
    [ValidateSet('small.en', 'base.en', 'medium.en', 'small')]
    [string]$Model = 'small.en',

    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot  = Split-Path -Parent $PSScriptRoot
$ModelsDir = Join-Path $RepoRoot 'models'
$BaseUrl   = 'https://huggingface.co/ggerganov/whisper.cpp/resolve/main'

# SHA-256 of each release as published by the whisper.cpp project.
#
# Only small.en is filled in: it was computed from the copy this repository
# was developed against, so it is known-good rather than copied from a page
# that may have been edited. The others download and report their hash for
# you to record here the first time you use one — an unverified constant is
# worse than an absent one, because it looks like a guarantee.
$Checksums = @{
    'small.en'  = 'c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d'
    'base.en'   = $null
    'medium.en' = $null
    'small'     = $null
}

$fileName = "ggml-$Model.bin"
$target   = Join-Path $ModelsDir $fileName
$expected = $Checksums[$Model]

New-Item -ItemType Directory -Path $ModelsDir -Force | Out-Null

function Get-Sha256([string]$Path) {
    (Get-FileHash -Path $Path -Algorithm SHA256).Hash.ToLower()
}

if ((Test-Path $target) -and -not $Force) {
    Write-Host "Found $fileName; verifying..." -ForegroundColor DarkGray
    $actual = Get-Sha256 $target

    if ($null -eq $expected) {
        Write-Host "$fileName is present." -ForegroundColor Green
        Write-Host "  SHA-256: $actual" -ForegroundColor DarkGray
        Write-Host "  No recorded checksum for this model — add the hash above to scripts/fetch-models.ps1." -ForegroundColor Yellow
        exit 0
    }
    if ($actual -eq $expected) {
        Write-Host "$fileName is present and verifies." -ForegroundColor Green
        exit 0
    }

    Write-Host "$fileName is present but does not match its recorded checksum." -ForegroundColor Red
    Write-Host "  expected: $expected" -ForegroundColor DarkGray
    Write-Host "  actual:   $actual"   -ForegroundColor DarkGray
    Write-Host "Re-downloading." -ForegroundColor Yellow
}

# Download beside the target and move only after verification, so an
# interrupted run never leaves a half-written model that looks complete.
$temp = "$target.partial"
$url  = "$BaseUrl/$fileName"

Write-Host "Downloading $fileName from $url" -ForegroundColor Cyan
Write-Host "This is a large file; expect several minutes." -ForegroundColor DarkGray

try {
    # Invoke-WebRequest's progress bar costs more time than the download on
    # a fast connection, because it repaints the console per chunk.
    $previousPreference = $ProgressPreference
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest -Uri $url -OutFile $temp -UseBasicParsing
} finally {
    $ProgressPreference = $previousPreference
}

$actual = Get-Sha256 $temp
$size   = (Get-Item $temp).Length

if ($null -ne $expected -and $actual -ne $expected) {
    Remove-Item $temp -Force
    throw @"
Checksum mismatch for $fileName — the download is discarded.
  expected: $expected
  actual:   $actual
Retry; if it fails again, the published weights may have changed.
"@
}

Move-Item -Path $temp -Destination $target -Force

Write-Host ''
Write-Host "$fileName downloaded." -ForegroundColor Green
Write-Host "  path:    $target"                                    -ForegroundColor DarkGray
Write-Host "  size:    $([Math]::Round($size / 1MB, 1)) MB"        -ForegroundColor DarkGray
Write-Host "  SHA-256: $actual"                                    -ForegroundColor DarkGray

if ($null -eq $expected) {
    Write-Host ''
    Write-Host "Record that hash in scripts/fetch-models.ps1 so the next run verifies it." -ForegroundColor Yellow
}

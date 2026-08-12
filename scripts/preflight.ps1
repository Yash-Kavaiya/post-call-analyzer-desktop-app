<#
.SYNOPSIS
    Runs the same checks CI runs, before you push.

.DESCRIPTION
    Every gate in .github/workflows/ci.yml that can run on a developer machine
    runs here, in the same order and with the same flags. The point is that a
    red build should be something you already saw locally, not a surprise ten
    minutes after you pushed.

    Where this and CI disagree, CI is right — it builds on a clean checkout
    with the MSVC toolchain, and this script uses whatever you have.

.PARAMETER Fix
    Apply what can be applied automatically (formatting, clippy's machine-
    applicable suggestions) instead of only reporting it.

.PARAMETER Fast
    Skip the slow gates: doctests, the UI build, and cargo-deny. Useful in a
    tight edit loop; not a substitute for a full run before pushing.

.PARAMETER SkipUi
    Skip the frontend checks. Use when you have not touched apps/desktop/ui
    and do not want to wait for npm.

.EXAMPLE
    pwsh scripts/preflight.ps1
    pwsh scripts/preflight.ps1 -Fix
    pwsh scripts/preflight.ps1 -Fast -SkipUi
#>
[CmdletBinding()]
param(
    [switch]$Fix,
    [switch]$Fast,
    [switch]$SkipUi
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
$UiDir    = Join-Path $RepoRoot 'apps/desktop/ui'

# cargo is installed per-user and is not always on PATH in a non-login shell.
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    if (Test-Path (Join-Path $cargoBin 'cargo.exe')) {
        $env:PATH = "$cargoBin;$env:PATH"
    } else {
        throw "cargo not found. Install Rust from https://rustup.rs and re-run."
    }
}

$script:Results = [System.Collections.Generic.List[object]]::new()
$script:Failed  = 0

function Invoke-Gate {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Body,
        [string]$Hint
    )

    Write-Host ''
    Write-Host "── $Name " -NoNewline -ForegroundColor Cyan
    Write-Host ('─' * [Math]::Max(1, 60 - $Name.Length)) -ForegroundColor DarkGray

    $sw = [Diagnostics.Stopwatch]::StartNew()
    $ok = $true
    try {
        & $Body
        if ($LASTEXITCODE -ne 0) { $ok = $false }
    } catch {
        Write-Host $_.Exception.Message -ForegroundColor Red
        $ok = $false
    }
    $sw.Stop()

    $script:Results.Add([pscustomobject]@{
        Gate    = $Name
        Result  = if ($ok) { 'pass' } else { 'FAIL' }
        Seconds = [Math]::Round($sw.Elapsed.TotalSeconds, 1)
        Hint    = if ($ok) { '' } else { $Hint }
    })

    if (-not $ok) {
        $script:Failed++
        Write-Host "$Name failed." -ForegroundColor Red
        if ($Hint) { Write-Host "  $Hint" -ForegroundColor Yellow }
    }
}

Push-Location $RepoRoot
try {
    Write-Host 'Preflight — running the CI gates locally' -ForegroundColor White
    Write-Host "Repository: $RepoRoot" -ForegroundColor DarkGray

    # ── Toolchain ──────────────────────────────────────────────
    # rust-toolchain.toml declares these, but a stale rustup will not have
    # picked them up, and the failure that follows is cryptic.
    foreach ($component in @('rustfmt', 'clippy')) {
        $installed = (rustup component list --installed) -join "`n"
        if ($installed -notmatch $component) {
            Write-Host "Installing missing component: $component" -ForegroundColor Yellow
            rustup component add $component
        }
    }

    # ── Formatting ─────────────────────────────────────────────
    if ($Fix) {
        Invoke-Gate 'Format (applying)' { cargo fmt --all }
    } else {
        Invoke-Gate 'Format' `
            { cargo fmt --all -- --check } `
            'Run: cargo fmt --all   (or re-run this script with -Fix)'
    }

    # ── Lockfile ───────────────────────────────────────────────
    Invoke-Gate 'Lockfile' `
        { cargo metadata --locked --format-version 1 | Out-Null } `
        'Cargo.lock is out of date. Run: cargo update --workspace  and commit the result.'

    # ── Lints ──────────────────────────────────────────────────
    if ($Fix) {
        Invoke-Gate 'Clippy (applying)' {
            cargo clippy --workspace --all-targets --fix --allow-dirty --allow-staged
        }
    }
    Invoke-Gate 'Clippy' `
        { cargo clippy --workspace --all-targets --locked -- -D warnings } `
        'CI treats every clippy warning as an error. Fix them, or justify one with #[allow(...)] and a comment.'

    # ── Tests ──────────────────────────────────────────────────
    if (Get-Command cargo-nextest -ErrorAction SilentlyContinue) {
        Invoke-Gate 'Tests' `
            { cargo nextest run --workspace --all-targets --locked } `
            'See the failure output above.'
    } else {
        Write-Host 'cargo-nextest not installed; falling back to cargo test.' -ForegroundColor DarkGray
        Write-Host '  Install it for faster runs: cargo install cargo-nextest --locked' -ForegroundColor DarkGray
        Invoke-Gate 'Tests' `
            { cargo test --workspace --locked } `
            'See the failure output above.'
    }

    if (-not $Fast) {
        Invoke-Gate 'Doctests' `
            { cargo test --doc --workspace --locked } `
            'An example in a doc comment no longer compiles.'

        Invoke-Gate 'Rustdoc' `
            { $env:RUSTDOCFLAGS = '-D warnings'; cargo doc --workspace --no-deps --locked } `
            'Usually a broken [Type] link in a doc comment.'
        Remove-Item Env:\RUSTDOCFLAGS -ErrorAction SilentlyContinue
    }

    # ── Supply chain ───────────────────────────────────────────
    if (-not $Fast) {
        if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
            Invoke-Gate 'Supply chain' `
                { cargo deny check } `
                'See deny.toml. A new dependency may carry a licence this product cannot ship.'
        } else {
            Write-Host 'cargo-deny not installed; skipping the supply chain gate.' -ForegroundColor DarkGray
            Write-Host '  Install it: cargo install cargo-deny --locked' -ForegroundColor DarkGray
        }
    }

    # ── Frontend ───────────────────────────────────────────────
    if (-not $SkipUi -and -not $Fast) {
        Push-Location $UiDir
        try {
            if (-not (Test-Path 'node_modules')) {
                Write-Host 'Installing UI dependencies...' -ForegroundColor DarkGray
                npm ci
            }
            Invoke-Gate 'UI lint' { npm run lint } 'Run: npm run lint --prefix apps/desktop/ui'
            Invoke-Gate 'UI build' { npm run build } 'tsc or vite failed; see the output above.'
        } finally {
            Pop-Location
        }
    }

    # ── Summary ────────────────────────────────────────────────
    Write-Host ''
    Write-Host ('═' * 64) -ForegroundColor DarkGray
    $script:Results | Format-Table -AutoSize Gate, Result, Seconds

    if ($script:Failed -gt 0) {
        Write-Host "$($script:Failed) gate(s) failed. CI would reject this." -ForegroundColor Red
        foreach ($r in $script:Results | Where-Object { $_.Result -eq 'FAIL' -and $_.Hint }) {
            Write-Host "  $($r.Gate): $($r.Hint)" -ForegroundColor Yellow
        }
        exit 1
    }

    Write-Host 'All gates passed.' -ForegroundColor Green
    if ($Fast -or $SkipUi) {
        Write-Host 'Note: this was a partial run. Run without -Fast/-SkipUi before pushing.' -ForegroundColor Yellow
    }
    exit 0
} finally {
    Pop-Location
}

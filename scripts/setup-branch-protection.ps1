<#
.SYNOPSIS
    Configures branch protection and repository settings to match the pipeline.

.DESCRIPTION
    The workflows in .github/workflows only report results. What makes them a
    gate is the repository configuration that requires them, and that lives in
    GitHub's settings rather than in the repository — which means it is
    invisible in review, undocumented by default, and easy to lose. This
    script puts it in version control.

    It configures:

      * `main` requires the `CI` and `Security` status checks
      * Pull requests only — no direct pushes, including for administrators
      * Stale approvals dismissed when new commits arrive
      * Code owner review for the paths in .github/CODEOWNERS
      * Conversations resolved before merge
      * Linear history; no force pushes; no branch deletion
      * Squash merge only, taking its message from the PR title and body
      * Branches deleted automatically after merge
      * Dependabot alerts and automated security fixes
      * Secret scanning with push protection

    Run it again after changing required checks; it is idempotent.

.PARAMETER Repository
    owner/name. Defaults to the repository the current directory belongs to.

.PARAMETER AllowSelfMerge
    Set required approvals to zero. On a single-maintainer repository,
    requiring an approval means you cannot merge your own pull requests. The
    status checks still apply, which is the part that catches mistakes.

.PARAMETER WhatIf
    Print what would be sent, and send nothing.

.EXAMPLE
    pwsh scripts/setup-branch-protection.ps1 -WhatIf
    pwsh scripts/setup-branch-protection.ps1 -AllowSelfMerge
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$Repository,
    [switch]$AllowSelfMerge
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Branch = 'main'

if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
    throw "The GitHub CLI is required. Install it from https://cli.github.com and run 'gh auth login'."
}

if (-not $Repository) {
    $Repository = (gh repo view --json nameWithOwner --jq .nameWithOwner)
    if ($LASTEXITCODE -ne 0 -or -not $Repository) {
        throw "Could not determine the repository. Pass -Repository owner/name."
    }
}

Write-Host "Repository: $Repository" -ForegroundColor Cyan
Write-Host "Branch:     $Branch"     -ForegroundColor Cyan
Write-Host ''

# Each entry is a description, an API call, and whether failing it is fatal.
# Several of these depend on the plan or on the repository being public, so a
# refusal is reported and skipped rather than aborting the whole run.
function Invoke-GhApi {
    param(
        [Parameter(Mandatory)][string]$Description,
        [Parameter(Mandatory)][string]$Endpoint,
        [string]$Method = 'PUT',
        [string]$Body,
        [switch]$Required
    )

    Write-Host "→ $Description" -ForegroundColor White

    if ($PSCmdlet.ShouldProcess($Endpoint, $Method)) {
        $output = if ($Body) {
            $Body | gh api --method $Method $Endpoint --input - 2>&1
        } else {
            gh api --method $Method $Endpoint 2>&1
        }

        if ($LASTEXITCODE -eq 0) {
            Write-Host "  done" -ForegroundColor Green
            return
        }

        $message = ($output | Out-String).Trim()
        if ($Required) {
            throw "$Description failed:`n$message"
        }
        Write-Host "  skipped — $($message -split "`n" | Select-Object -First 1)" -ForegroundColor Yellow
        Write-Host "  (usually means the plan or repository visibility does not allow it)" -ForegroundColor DarkGray
    } else {
        Write-Host "  would $Method $Endpoint" -ForegroundColor DarkGray
        if ($Body) { Write-Host "  $Body" -ForegroundColor DarkGray }
    }
}

# ── Branch protection ──────────────────────────────────────────
$approvals = if ($AllowSelfMerge) { 0 } else { 1 }

if ($AllowSelfMerge) {
    Write-Host "Self-merge allowed: required approvals set to 0." -ForegroundColor Yellow
    Write-Host ''
}

$protection = [ordered]@{
    required_status_checks = [ordered]@{
        # `strict` requires a branch to be up to date with main before merging,
        # so a check cannot pass against a base that has since moved.
        strict = $true
        # These are the two roll-up jobs. Every other job feeds into them, so
        # this list does not change when jobs are added or renamed.
        checks = @(
            @{ context = 'CI' }
            @{ context = 'Security' }
        )
    }
    # Applies the rules to administrators too. A rule an admin can walk past
    # is a suggestion.
    enforce_admins                  = $true
    required_pull_request_reviews   = [ordered]@{
        dismiss_stale_reviews           = $true
        require_code_owner_reviews      = $true
        required_approving_review_count = $approvals
        # An approval given before the author pushed more commits is not an
        # approval of what would merge.
        require_last_push_approval      = -not $AllowSelfMerge
    }
    restrictions                     = $null
    required_linear_history          = $true
    allow_force_pushes               = $false
    allow_deletions                  = $false
    required_conversation_resolution = $true
    lock_branch                      = $false
    allow_fork_syncing               = $true
}

Invoke-GhApi -Description "Protect $Branch" `
    -Endpoint "repos/$Repository/branches/$Branch/protection" `
    -Method PUT `
    -Body ($protection | ConvertTo-Json -Depth 10) `
    -Required

# ── Merge behaviour ────────────────────────────────────────────
# Squash-only, with the commit message taken from the pull request. This is
# what makes the Conventional Commits check on the PR title worth having: the
# title becomes the commit subject, which becomes the release notes.
$repoSettings = [ordered]@{
    allow_squash_merge          = $true
    allow_merge_commit          = $false
    allow_rebase_merge          = $false
    delete_branch_on_merge      = $true
    squash_merge_commit_title   = 'PR_TITLE'
    squash_merge_commit_message = 'PR_BODY'
    allow_auto_merge            = $true
    has_wiki                    = $false
}

Invoke-GhApi -Description 'Configure merge behaviour' `
    -Endpoint "repos/$Repository" `
    -Method PATCH `
    -Body ($repoSettings | ConvertTo-Json -Depth 10)

# ── Security features ──────────────────────────────────────────
Invoke-GhApi -Description 'Enable Dependabot alerts' `
    -Endpoint "repos/$Repository/vulnerability-alerts" -Method PUT

Invoke-GhApi -Description 'Enable automated security fixes' `
    -Endpoint "repos/$Repository/automated-security-fixes" -Method PUT

# Push protection rejects a commit containing a recognised credential at push
# time. The gitleaks job in security.yml is the backstop for what it misses.
$security = [ordered]@{
    security_and_analysis = [ordered]@{
        secret_scanning                 = @{ status = 'enabled' }
        secret_scanning_push_protection = @{ status = 'enabled' }
    }
}

Invoke-GhApi -Description 'Enable secret scanning and push protection' `
    -Endpoint "repos/$Repository" `
    -Method PATCH `
    -Body ($security | ConvertTo-Json -Depth 10)

Write-Host ''
Write-Host 'Done.' -ForegroundColor Green
Write-Host ''
Write-Host 'Verify at:' -ForegroundColor DarkGray
Write-Host "  https://github.com/$Repository/settings/branches" -ForegroundColor DarkGray
Write-Host ''
Write-Host 'Note: the CI and Security checks must have run at least once before' -ForegroundColor DarkGray
Write-Host 'GitHub will show them as required. Open a pull request if this is the' -ForegroundColor DarkGray
Write-Host 'first time.' -ForegroundColor DarkGray

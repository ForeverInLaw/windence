# Offline tests for the guards protecting the owner's Partner Center draft.
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'store-submission.ps1')
$valid = @{
    Id = 'app'; PackageIdentityName = 'package'; PublisherName = 'CN=publisher'
    FirstPublishedDate = '2026-10-12T00:00:00Z'
    LastPublishedApplicationSubmission = @{ Id = 'published' }
    PendingApplicationSubmission = $null
}
$arguments = @{ AppId = 'app'; Name = 'package'; Publisher = 'CN=publisher' }
Assert-StoreApplication -Application ([pscustomobject]$valid) @arguments
foreach ($case in @('draft', 'unpublished', 'missing', 'identity', 'publisher', 'app')) {
    $response = $valid.Clone()
    switch ($case) {
        'draft' { $response.PendingApplicationSubmission = @{ Id = 'owner-draft' } }
        'unpublished' { $response.LastPublishedApplicationSubmission = $null }
        'missing' { $response.Remove('PendingApplicationSubmission') }
        'identity' { $response.PackageIdentityName = 'another-package' }
        'publisher' { $response.PublisherName = 'CN=another-publisher' }
        'app' { $response.Id = 'another-app' }
    }
    $rejected = $false
    try { Assert-StoreApplication -Application ([pscustomobject]$response) @arguments } catch { $rejected = $true }
    if (-not $rejected) { throw "Unsafe $case response was accepted" }
}
Write-Host 'Store guards passed: published product accepted; draft, missing data and identity mismatches rejected'

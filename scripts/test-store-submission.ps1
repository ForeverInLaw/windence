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

# Exercise delivery with mocked HTTP, including a draft created after the guard.
. (Join-Path $PSScriptRoot 'store-delivery.ps1')
$script:calls = [Collections.Generic.List[string]]::new()
function Invoke-RestMethod {
    [CmdletBinding()]
    param($Uri, $Method, $Headers, $Body, $ContentType, $TimeoutSec, $MaximumRedirection)
    $script:calls.Add("$Method $Uri")
    if ($Uri.EndsWith('/oauth2/token')) { return @{ access_token='synthetic-token' } }
    if ($Uri.EndsWith('/submissions')) {
        if ($script:scenario -eq 'conflict') { throw 'HTTP 409: private upstream details' }
        return ('{"id":"owned","status":"PendingCommit","fileUploadUrl":' +
            '"https://test.blob.core.windows.net/upload?sig=synthetic",' +
            '"applicationPackages":[{"id":"old","fileName":"old.msix","fileStatus":"Uploaded"}],' +
            '"notesForCertification":"preserve notes","listings":{"en-us":{"title":"Windence"}},' +
            '"pricing":{"priceId":"Free"}}') | ConvertFrom-Json
    }
    if ($Method -eq 'Put') {
        $script:updated = [Text.Encoding]::UTF8.GetString($Body) | ConvertFrom-Json
        return $script:updated
    }
    if ($Uri.EndsWith('/commit')) { return @{ status='CommitStarted' } }
    if ($Uri.EndsWith('/status')) {
        if ($script:scenario -eq 'commit-failure') { return @{ status='CommitFailed' } }
        return @{ status='PreProcessing' }
    }
    throw 'Unexpected Store request'
}
function Invoke-WebRequest {
    [CmdletBinding()]
    param($Uri, $Method, $InFile, $Headers, $ContentType, $TimeoutSec, $MaximumRedirection)
    $script:calls.Add('Upload')
    if ($script:scenario -eq 'upload-failure') { throw 'Private upload failure' }
    $archive = [IO.Compression.ZipFile]::OpenRead($InFile)
    try {
        if ($archive.Entries.Count -ne 1 -or $archive.Entries[0].FullName -ne 'new.msix') {
            throw 'Unexpected upload archive'
        }
        $reader = [IO.StreamReader]::new($archive.Entries[0].Open())
        try {
            if ($reader.ReadToEnd() -ne 'synthetic package') { throw 'Package bytes changed' }
        } finally { $reader.Dispose() }
    } finally { $archive.Dispose() }
}
$directory = Join-Path ([IO.Path]::GetTempPath()) ('windence-store-test-' + [Guid]::NewGuid())
$savedEnvironment = @{}
foreach ($name in @('MSSTORE_TENANT_ID', 'MSSTORE_CLIENT_ID', 'MSSTORE_CLIENT_SECRET')) {
    $savedEnvironment[$name] = [Environment]::GetEnvironmentVariable($name)
    [Environment]::SetEnvironmentVariable($name, 'synthetic')
}
try {
    $null = [IO.Directory]::CreateDirectory($directory)
    $package = Join-Path $directory 'new.msix'
    [IO.File]::WriteAllText($package, 'synthetic package')
    foreach ($script:scenario in @('conflict', 'upload-failure', 'commit-failure', 'success')) {
        $script:calls.Clear()
        $failed = $false
        try { Submit-StorePackage -PackagePath $package -AppId 'app' } catch {
            $failed = $true
            if ($_.Exception.Message -match 'private|synthetic-token') { throw 'Provider details leaked' }
        }
        if ($failed -ne ($script:scenario -ne 'success')) { throw "Unexpected $script:scenario result" }
        if ($script:calls | Where-Object { $_ -like 'Delete *' }) { throw 'Delivery deleted a draft' }
        if ($script:scenario -eq 'conflict' -and $script:calls.Count -ne 2) {
            throw 'Delivery continued after a competing draft appeared'
        }
        if ($script:scenario -eq 'upload-failure' -and
            ($script:calls | Where-Object { $_ -like '*/commit' })) { throw 'Failed upload was committed' }
    }
    if ($script:updated.applicationPackages[0].fileStatus -ne 'PendingDelete' -or
        $script:updated.applicationPackages[1].fileName -ne 'new.msix' -or
        $script:updated.applicationPackages[1].fileStatus -ne 'PendingUpload' -or
        $script:updated.notesForCertification -ne 'preserve notes' -or
        $script:updated.pricing.priceId -ne 'Free' -or
        $script:updated.listings.'en-us'.title -ne 'Windence') {
        throw 'Delivery changed existing metadata or failed to replace the package'
    }
} finally {
    foreach ($name in $savedEnvironment.Keys) {
        [Environment]::SetEnvironmentVariable($name, $savedEnvironment[$name])
    }
    if ([IO.Directory]::Exists($directory)) { [IO.Directory]::Delete($directory, $true) }
}
Write-Host 'Store delivery passed: competing draft preserved, package and metadata retained, failures reported'

<#
.SYNOPSIS
Submits an update to an already published free Microsoft Store product.
.DESCRIPTION
Run only in the protected store GitHub Environment with CLI v0.4.3.
It refuses existing drafts because the CLI would delete them. Do not edit
Partner Center while CI submits: the portal and CLI cannot share a lock.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PackagePath,
    [Parameter(Mandatory = $true)][string]$ExpectedVersion
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'store-submission.ps1')
$appId = '9PJ0NRJRJWH8'
$identity = Get-StorePackageIdentity -PackagePath $PackagePath -ExpectedVersion ($ExpectedVersion -replace '^v', '')
foreach ($key in @('MSSTORE_TENANT_ID', 'MSSTORE_SELLER_ID', 'MSSTORE_CLIENT_ID', 'MSSTORE_CLIENT_SECRET')) {
    if (-not [Environment]::GetEnvironmentVariable($key)) { throw "Configure $key in the store environment" }
}
msstore reconfigure --tenantId $env:MSSTORE_TENANT_ID --sellerId $env:MSSTORE_SELLER_ID `
    --clientId $env:MSSTORE_CLIENT_ID --clientSecret $env:MSSTORE_CLIENT_SECRET
if ($LASTEXITCODE -ne 0) { throw 'Store CLI authentication configuration failed' }
# v0.4.3 sends diagnostics to stderr and PascalCase application JSON to stdout.
$json = & msstore --output-stream stderr apps get $appId
if ($LASTEXITCODE -ne 0) { throw 'Could not read the Store product; nothing submitted' }
$application = ($json -join "`n") | ConvertFrom-Json
Assert-StoreApplication -Application $application -AppId $appId -Name $identity.Name -Publisher $identity.Publisher
msstore publish (Resolve-Path $PackagePath).Path --appId $appId
if ($LASTEXITCODE -ne 0) { throw 'Store submission failed; inspect Partner Center before retrying' }

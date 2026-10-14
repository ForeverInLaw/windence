<#
.SYNOPSIS
Submits an update to an already published free Microsoft Store product.
.DESCRIPTION
Run only in the protected store GitHub Environment with CLI v0.4.3.
It refuses existing drafts and uses the API to create its own submission. Do not edit
Partner Center while CI submits: the portal and CLI cannot share a lock.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$PackagePath,
    [Parameter(Mandatory = $true)][string]$ExpectedVersion
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'store-submission.ps1')
. (Join-Path $PSScriptRoot 'store-delivery.ps1')
. (Join-Path $PSScriptRoot 'store-access.ps1')
$appId = '9PJ0NRJRJWH8'
$identity = Get-StorePackageIdentity -PackagePath $PackagePath -ExpectedVersion ($ExpectedVersion -replace '^v', '')
$application = Get-StoreApplication -AppId $appId
Assert-StoreApplication -Application $application -AppId $appId -Name $identity.Name -Publisher $identity.Publisher
Submit-StorePackage -PackagePath $PackagePath -AppId $appId

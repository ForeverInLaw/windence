<#
.SYNOPSIS
Downloads Microsoft's installer for the latest certified Windence Store version.
#>
[CmdletBinding()]
param([string]$OutputDirectory = 'dist\store-installer')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-package.ps1')
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$installer = Join-Path $OutputDirectory 'Windence-Store-Installer.exe'
# This is the download URL used by Microsoft's official Store badge.
Invoke-WebRequest -Uri 'https://get.microsoft.com/installer/download/9PJ0NRJRJWH8?referrer=appbadge&source=github.com' `
    -OutFile $installer -TimeoutSec 120
$signature = Get-AuthenticodeSignature -LiteralPath $installer
if ($signature.Status -ne 'Valid' -or -not $signature.SignerCertificate -or
    $signature.SignerCertificate.Subject -notmatch '(^|,\s*)O=Microsoft Corporation(,|$)') {
    Remove-Item -LiteralPath $installer -Force
    throw 'Store installer must have a valid Microsoft signature'
}
Write-PackageChecksum -Path $installer
Write-Host 'Microsoft Store installer downloaded; Microsoft signature verified.'

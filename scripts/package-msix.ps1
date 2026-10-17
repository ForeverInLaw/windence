<#
.SYNOPSIS
Builds an unsigned x64 MSIX for manual upload to Microsoft Store.
.DESCRIPTION
Microsoft signs the public package after certification. This script needs the
Windows SDK, but no signing key. Cargo.toml supplies the version; Store requires
stable major.minor.patch.0 versions with each component at most 65535.
#>
[CmdletBinding()]
param([switch]$SkipBuild, [string]$ExpectedVersion)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-package.ps1')
. (Join-Path $PSScriptRoot 'store-resources.ps1')
$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $version = Get-AppVersion -Root $root -ExpectedVersion $ExpectedVersion
    if ($version -notmatch '^([1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
        throw "Store needs a stable version with a nonzero major component: $version"
    }
    if (@($version.Split('.') | Where-Object { [long]$_ -gt 65535 }).Count) {
        throw 'Store version components must not exceed 65535'
    }
    if (-not $SkipBuild) { Build-WindowsRelease }

    $stage = Join-Path $root 'target\msix\stage'
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    Copy-WindowsPayload -Root $root -Directory $stage -ExecutableName 'Windence.exe'
    Copy-Item (Join-Path $root 'packaging\store\Assets') $stage -Recurse

    [xml]$manifest = Get-Content (Join-Path $root 'packaging\store\AppxManifest.xml')
    $manifest.Package.Identity.Version = "$version.0"
    $manifest.Save((Join-Path $stage 'AppxManifest.xml'))

    New-StoreResourceIndex -Directory $stage -ConfigPath (Join-Path $root 'target\msix\priconfig.xml')

    $output = Join-Path $root 'dist\store'
    if (Test-Path $output) { Remove-Item $output -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $output | Out-Null
    $package = Join-Path $output "Windence-$version-windows-x64.msix"
    $makeAppx = Get-WindowsSdkTool -Name 'makeappx.exe'
    & $makeAppx pack /d $stage /p $package /o
    if ($LASTEXITCODE -ne 0) { throw "MakeAppx failed with exit code $LASTEXITCODE" }
    Write-PackageChecksum -Path $package
    Write-Host "Store upload: $package"
} finally {
    Pop-Location
}

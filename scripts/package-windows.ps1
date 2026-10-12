<#
.SYNOPSIS
Builds Windence for Windows and packages it as an installer and a zip.

.DESCRIPTION
The macOS packaging lives in package-app.sh and cannot run here. This is the
Windows counterpart: it builds the release binary, gathers what ships beside
it, and produces both an installer and a plain zip in dist/development/.

The binary imports one library Windows does not always carry, VCRUNTIME140,
so a copy from the Visual Studio redistributable is shipped next to the app.
That is the deployment Microsoft supports for it, and it saves the person
installing Windence a separate download.

.PARAMETER SkipBuild
Package whatever is already in target/release instead of building first.

.PARAMETER ExpectedVersion
Stop unless Cargo.toml carries this version. The release workflow passes the
tag it is building, so a tag that does not match the crate fails before
anything is published. A leading "v" is ignored.
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string]$ExpectedVersion
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-package.ps1')

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $version = Get-AppVersion -Root $root -ExpectedVersion $ExpectedVersion
    Write-Host "Windence $version"

    if (-not $SkipBuild) {
        Write-Host 'Building the release binary...'
        Build-WindowsRelease
    }

    $dist = Join-Path $root 'dist\development'
    $stage = Join-Path $dist "Windence-$version-windows-x64"
    if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
    Copy-WindowsPayload -Root $root -Directory $stage -ExecutableName 'Windence.exe'
    Copy-Item (Join-Path $stage 'vcruntime140.dll') $dist # Inno reads it here.

    $zip = Join-Path $dist "Windence-$version-windows-x64.zip"
    Compress-Archive -Path $stage -DestinationPath $zip -CompressionLevel Optimal -Force
    Write-Host "Wrote $zip"

    $iscc = Get-ChildItem -Path @(
        "$env:LOCALAPPDATA\Programs",
        'C:\Program Files (x86)',
        'C:\Program Files'
    ) -Recurse -Filter 'ISCC.exe' -Depth 3 -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($iscc) {
        # Cargo.toml is the one place the version is written down; the
        # installer script takes it from here.
        & $iscc.FullName "/DAppVersion=$version" (Join-Path $PSScriptRoot 'cadence.iss') | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "ISCC failed with exit code $LASTEXITCODE" }
        Write-Host "Wrote $(Join-Path $dist "Windence-$version-windows-x64-setup.exe")"
    } else {
        Write-Warning 'Inno Setup (ISCC.exe) not found; built the zip only.'
    }

    Get-ChildItem $dist -File | ForEach-Object {
        Write-PackageChecksum -Path $_.FullName
        '{0,-46} {1,8:N2} MB' -f $_.Name, ($_.Length / 1MB)
    }
} finally {
    Pop-Location
}

<#
.SYNOPSIS
Builds Cadence for Windows and packages it as an installer and a zip.

.DESCRIPTION
The macOS packaging lives in package-app.sh and cannot run here. This is the
Windows counterpart: it builds the release binary, gathers what ships beside
it, and produces both an installer and a plain zip in dist/.

The binary imports one library Windows does not always carry, VCRUNTIME140,
so a copy from the Visual Studio redistributable is shipped next to the app.
That is the deployment Microsoft supports for it, and it saves the person
installing Cadence a separate download.

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
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root
try {
    $version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
    Write-Host "Cadence $version"

    if ($ExpectedVersion -and ($ExpectedVersion -replace '^v', '') -ne $version) {
        throw "asked to package $ExpectedVersion, but Cargo.toml says $version"
    }

    if (-not $SkipBuild) {
        Write-Host 'Building the release binary...'
        cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
    }

    $binary = Join-Path $root 'target\release\spotify-gpui-client.exe'
    if (-not (Test-Path $binary)) { throw "no release binary at $binary" }

    $dist = Join-Path $root 'dist'
    $stage = Join-Path $dist "Cadence-$version-windows-x64"
    if (Test-Path $dist) { Remove-Item $dist -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $stage | Out-Null

    # The newest x64 redistributable the installed Build Tools carry. The
    # search starts inside VC\Redist rather than at the Visual Studio root,
    # which is tens of thousands of files deep and takes a minute to walk.
    $redist = Get-ChildItem -Path @(
        'C:\Program Files\Microsoft Visual Studio\*\*\VC\Redist\MSVC',
        'C:\Program Files (x86)\Microsoft Visual Studio\*\*\VC\Redist\MSVC'
    ) -Recurse -Filter 'vcruntime140.dll' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\Microsoft\.VC\d+\.CRT\\' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if (-not $redist) { throw 'no x64 vcruntime140.dll in the Visual Studio redistributable' }

    Copy-Item $binary (Join-Path $stage 'Cadence.exe')
    Copy-Item $redist.FullName $dist          # the installer reads it from here
    Copy-Item $redist.FullName $stage         # the zip carries its own copy
    Copy-Item (Join-Path $root 'LICENSE') $stage
    Copy-Item (Join-Path $root 'THIRD_PARTY_NOTICES.md') $stage

    $zip = Join-Path $dist "Cadence-$version-windows-x64.zip"
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
        Write-Host "Wrote $(Join-Path $dist "Cadence-$version-windows-x64-setup.exe")"
    } else {
        Write-Warning 'Inno Setup (ISCC.exe) not found; built the zip only.'
    }

    Get-ChildItem $dist -File | ForEach-Object {
        $hash = (Get-FileHash $_.FullName -Algorithm SHA256).Hash
        "$hash  $($_.Name)" | Set-Content "$($_.FullName).sha256" -Encoding ascii
        '{0,-46} {1,8:N2} MB' -f $_.Name, ($_.Length / 1MB)
    }
} finally {
    Pop-Location
}

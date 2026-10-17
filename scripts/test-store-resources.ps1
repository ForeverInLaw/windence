# Verify shell resources with the real Windows SDK without building the app.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-package.ps1')
. (Join-Path $PSScriptRoot 'store-resources.ps1')
$scratch = Join-Path $env:TEMP ([Guid]::NewGuid().ToString())
try {
    $root = Split-Path -Parent $PSScriptRoot
    $stage = New-Item -ItemType Directory -Path (Join-Path $scratch 'stage')
    Copy-Item (Join-Path $root 'packaging\store\Assets') $stage.FullName -Recurse
    Copy-Item (Join-Path $root 'packaging\store\AppxManifest.xml') $stage.FullName
    New-StoreResourceIndex -Directory $stage.FullName -ConfigPath (Join-Path $scratch 'priconfig.xml')
    Test-StoreResourceIndex -Directory $stage.FullName -DumpPath (Join-Path $scratch 'resources.xml')
} finally {
    if (Test-Path $scratch) { Remove-Item $scratch -Recurse -Force }
}

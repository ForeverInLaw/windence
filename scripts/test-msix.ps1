<#
.SYNOPSIS
Installs and starts a test-signed copy of the Store MSIX on a clean Windows runner.
.DESCRIPTION
Run through Windows PowerShell with administrator rights. The Store upload
stays unsigned. A temporary certificate is trusted only on the test machine
and removed with the test package. This checks onboarding, not Spotify login,
audio, SMTC or a Store-delivered update.
#>
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$PackagePath)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows-package.ps1')
$scratch = Join-Path $env:TEMP ([Guid]::NewGuid().ToString())
$certificate = $null
$trusted = $null
$installed = $null
$process = $null
$ownsProfile = $false
try {
    $profile = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'Cadence'
    if ((Test-Path $profile) -or (Get-Process Windence,Cadence -ErrorAction SilentlyContinue)) {
        throw 'Use a clean Windows runner; an existing profile or app could invalidate this test'
    }
    $ownsProfile = $true
    New-Item -ItemType Directory -Path $scratch | Out-Null
    $makeAppx = Get-WindowsSdkTool -Name 'makeappx.exe'
    $unpacked = Join-Path $scratch 'unpacked'
    & $makeAppx unpack /p (Resolve-Path $PackagePath).Path /d $unpacked /o
    if ($LASTEXITCODE -ne 0) { throw 'MakeAppx could not unpack the Store artifact' }
    if (Test-Path (Join-Path $unpacked 'AppxSignature.p7x')) { throw 'Store upload should be unsigned' }
    [xml]$manifest = Get-Content (Join-Path $unpacked 'AppxManifest.xml')
    $name = $manifest.Package.Identity.Name
    if (Get-AppxPackage -Name $name) { throw 'A package with this identity is already installed' }
    foreach ($file in @('Windence.exe', 'vcruntime140.dll', 'LICENSE', 'THIRD_PARTY_NOTICES.md')) {
        if (-not (Test-Path (Join-Path $unpacked $file))) { throw "Missing package payload: $file" }
    }
    $testPackage = Join-Path $scratch 'test.msix'
    Copy-Item $PackagePath $testPackage
    $certificate = New-SelfSignedCertificate -Type Custom -Subject $manifest.Package.Identity.Publisher `
        -KeyUsage DigitalSignature -KeyAlgorithm RSA -KeyLength 2048 -HashAlgorithm SHA256 `
        -CertStoreLocation 'Cert:\CurrentUser\My' `
        -TextExtension @('2.5.29.37={text}1.3.6.1.5.5.7.3.3', '2.5.29.19={text}')
    $cer = Join-Path $scratch 'test.cer'
    Export-Certificate -Cert $certificate -FilePath $cer | Out-Null
    $trusted = Import-Certificate -FilePath $cer -CertStoreLocation 'Cert:\LocalMachine\TrustedPeople'
    $signTool = Get-WindowsSdkTool -Name 'signtool.exe'
    & $signTool sign /fd SHA256 /sha1 $certificate.Thumbprint $testPackage
    if ($LASTEXITCODE -ne 0) { throw 'Could not sign the temporary test package' }
    Add-AppxPackage -Path $testPackage
    $installed = Get-AppxPackage -Name $name
    if (-not $installed -or $installed.Version -ne $manifest.Package.Identity.Version) {
        throw 'Windows registered an unexpected package version'
    }
    $env:SPOTIFY_CLIENT_ID = $null
    Start-Process explorer.exe -ArgumentList "shell:AppsFolder\$($installed.PackageFamilyName)!Windence"
    $deadline = (Get-Date).AddSeconds(30)
    do {
        Start-Sleep -Milliseconds 500
        $process = Get-Process Windence -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($process) { $process.Refresh() }
    } until (($process -and $process.MainWindowHandle -ne 0) -or (Get-Date) -gt $deadline)
    if (-not $process -or $process.MainWindowHandle -eq 0) { throw 'Packaged onboarding window did not open' }
    if (-not (Test-Path (Join-Path $profile 'cadence.sqlite3'))) {
        throw 'The packaged app did not use the existing unvirtualized profile path'
    }
    Write-Host "Installed and opened $($installed.PackageFullName). Profile: $profile"
} finally {
    if ($process -and -not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    if ($installed) { Remove-AppxPackage -Package $installed.PackageFullName }
    if ($trusted) { Remove-Item "Cert:\LocalMachine\TrustedPeople\$($trusted.Thumbprint)" -Force }
    if ($certificate) { Remove-Item "Cert:\CurrentUser\My\$($certificate.Thumbprint)" -Force }
    if ($ownsProfile -and (Test-Path $profile)) { Remove-Item $profile -Recurse -Force }
    if (Test-Path $scratch) { Remove-Item $scratch -Recurse -Force }
}

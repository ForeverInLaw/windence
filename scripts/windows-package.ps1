# Shared payload for the Store package and development ZIP/installer.
Set-StrictMode -Version Latest

function Get-AppVersion {
    param([string]$Root, [string]$ExpectedVersion)
    $version = (Select-String -Path (Join-Path $Root 'Cargo.toml') -Pattern '^version = "(.+)"' |
        Select-Object -First 1).Matches[0].Groups[1].Value
    if ($ExpectedVersion -and ($ExpectedVersion -replace '^v', '') -ne $version) {
        throw "asked to package $ExpectedVersion, but Cargo.toml says $version"
    }
    return $version
}

function Build-WindowsRelease {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with exit code $LASTEXITCODE" }
}

function Copy-WindowsPayload {
    param([string]$Root, [string]$Directory, [string]$ExecutableName)
    $binary = Join-Path $Root 'target\release\spotify-gpui-client.exe'
    if (-not (Test-Path $binary)) { throw "no release binary at $binary" }
    $info = (Get-Item $binary).VersionInfo
    if ($info.ProductName -ne 'Windence' -or $info.FileDescription -ne 'Windence' -or
        $info.OriginalFilename -ne 'Windence.exe') {
        throw 'Release executable has stale product metadata; rebuild before packaging'
    }
    New-Item -ItemType Directory -Force -Path $Directory | Out-Null

    # App-local deployment uses the x64 runtime supplied by Visual Studio.
    $redist = Get-ChildItem -Path @(
        'C:\Program Files\Microsoft Visual Studio\*\*\VC\Redist\MSVC',
        'C:\Program Files (x86)\Microsoft Visual Studio\*\*\VC\Redist\MSVC'
    ) -Recurse -Filter 'vcruntime140.dll' -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\Microsoft\.VC\d+\.CRT\\' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if (-not $redist) { throw 'no x64 vcruntime140.dll in the Visual Studio redistributable' }

    Copy-Item $binary (Join-Path $Directory $ExecutableName)
    Copy-Item $redist.FullName $Directory
    Copy-Item (Join-Path $Root 'LICENSE') $Directory
    Copy-Item (Join-Path $Root 'THIRD_PARTY_NOTICES.md') $Directory
}

function Get-WindowsSdkTool {
    param([string]$Name)
    $tool = Get-ChildItem -Path "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\$Name" |
        Sort-Object { [version]$_.Directory.Parent.Name } -Descending |
        Select-Object -First 1
    if (-not $tool) { throw "Windows SDK tool $Name not found; install the Windows SDK" }
    return $tool.FullName
}

function Write-PackageChecksum {
    param([string]$Path)
    $hash = (Get-FileHash $Path -Algorithm SHA256).Hash
    "$hash  $(Split-Path $Path -Leaf)" | Set-Content "$Path.sha256" -Encoding ascii
}

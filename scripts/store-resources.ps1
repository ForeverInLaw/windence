# Windows SDK resource indexing shared by packaging and artifact validation.
# Dot-source windows-package.ps1 first for Get-WindowsSdkTool.

function New-StoreResourceIndex {
    param([string]$Directory, [string]$ConfigPath)
    $makePri = Get-WindowsSdkTool -Name 'makepri.exe'
    $priConfig = $ConfigPath
    & $makePri createconfig /cf $priConfig /dq lang-en-US /pv 10.0.0 /o
    if ($LASTEXITCODE -ne 0) { throw "MakePRI createconfig failed with exit code $LASTEXITCODE" }
    [xml]$config = Get-Content $priConfig
    # Keep every scale and theme in this single MSIX, without resource packs.
    $config.resources.RemoveChild($config.resources.packaging) | Out-Null
    $config.resources.index.startIndexAt = 'Assets'
    $config.Save($priConfig)
    & $makePri new /pr $Directory /cf $priConfig /mn (Join-Path $Directory 'AppxManifest.xml') `
        /of (Join-Path $Directory 'resources.pri') /o
    if ($LASTEXITCODE -ne 0) { throw "MakePRI new failed with exit code $LASTEXITCODE" }

}

function Test-StoreResourceIndex {
    param([string]$Directory, [string]$DumpPath)
    $makePri = Get-WindowsSdkTool -Name 'makepri.exe'
    $priDump = $DumpPath
    & $makePri dump /if (Join-Path $Directory 'resources.pri') /of $priDump /dt Detailed /o
    if ($LASTEXITCODE -ne 0) { throw 'MakePRI could not read the packaged resource index' }
    [xml]$resources = Get-Content $priDump
    [xml]$manifest = Get-Content (Join-Path $Directory 'AppxManifest.xml')
    $logo = $manifest.SelectSingleNode('//*[local-name()="VisualElements"]/@Square44x44Logo').Value
    $resourceUri = "ms-resource://$($manifest.Package.Identity.Name)/Files/" + $logo.Replace('\', '/')
    $resource = @($resources.SelectNodes('//NamedResource') | Where-Object { $_.uri -eq $resourceUri })
    if ($resource.Count -ne 1) {
        $found = @($resources.SelectNodes('//NamedResource[@name="Square44x44Logo.png"]') |
            ForEach-Object { $_.uri }) -join ', '
        throw "Taskbar icon URI does not match the manifest: expected $resourceUri; found $found"
    }
    $candidates = $resource[0].SelectNodes('Candidate')
    Add-Type -AssemblyName System.Drawing
    foreach ($size in @(16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256)) {
        foreach ($form in @('', '_altform-unplated', '_altform-lightunplated')) {
            $asset = "Assets\Square44x44Logo.targetsize-$size$form.png"
            $indexed = @($candidates | Where-Object { $_.SelectSingleNode('Value').InnerText -eq $asset })
            if ($indexed.Count -ne 1) { throw "Taskbar icon missing from its PRI resource: $asset" }
            $image = [System.Drawing.Bitmap]::new((Join-Path $Directory $asset))
            try {
                if ($image.Width -ne $size -or $image.Height -ne $size) { throw "Wrong icon size: $asset" }
                foreach ($x in @(0, ($size - 1))) {
                    foreach ($y in @(0, ($size - 1))) {
                        if ($image.GetPixel($x, $y).A -ne 0) { throw "Opaque icon corner: $asset" }
                    }
                }
            } finally { $image.Dispose() }
        }
    }
    Write-Host 'Verified indexed taskbar icons at every target size and shell theme'
}

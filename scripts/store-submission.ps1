# Checks shared by CI publication and its tests. This file performs no writes.
Set-StrictMode -Version Latest

function Assert-StoreApplication {
    param($Application, [string]$AppId, [string]$Name, [string]$Publisher)
    foreach ($field in @('Id', 'PackageIdentityName', 'PublisherName', 'FirstPublishedDate',
            'LastPublishedApplicationSubmission', 'PendingApplicationSubmission')) {
        if ($Application.PSObject.Properties.Name -cnotcontains $field) {
            throw "Store response has no $field; refusing to submit"
        }
    }
    if ($Application.Id -cne $AppId -or $Application.PackageIdentityName -cne $Name -or
        $Application.PublisherName -cne $Publisher) {
        throw 'Store identity does not match this package'
    }
    if (-not $Application.FirstPublishedDate -or -not $Application.LastPublishedApplicationSubmission -or
        -not $Application.LastPublishedApplicationSubmission.Id) {
        throw 'Complete the first manual Store publication before enabling CI delivery'
    }
    if ($null -ne $Application.PendingApplicationSubmission) {
        throw 'Partner Center has a pending submission; finish or remove it manually before retrying'
    }
}

function Get-StorePackageIdentity {
    param([string]$PackagePath, [string]$ExpectedVersion)
    if ([IO.Path]::GetExtension($PackagePath) -cne '.msix') { throw 'Expected a single unsigned MSIX' }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead((Resolve-Path $PackagePath).Path)
    try {
        if ($archive.GetEntry('AppxSignature.p7x')) { throw 'Submit the unsigned Store artifact' }
        $entry = $archive.GetEntry('AppxManifest.xml')
        if (-not $entry) { throw 'Package has no manifest' }
        $reader = New-Object IO.StreamReader($entry.Open())
        try { [xml]$manifest = $reader.ReadToEnd() } finally { $reader.Dispose() }
        [xml]$source = Get-Content (Join-Path $PSScriptRoot '..\packaging\store\AppxManifest.xml')
        $identity = $manifest.Package.Identity
        if ($identity.Name -cne $source.Package.Identity.Name -or
            $identity.Publisher -cne $source.Package.Identity.Publisher -or
            $identity.ProcessorArchitecture -cne 'x64' -or
            $identity.Version -cne "$ExpectedVersion.0" -or
            $ExpectedVersion -notmatch '^[1-9][0-9]*\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$') {
            throw 'Package identity, architecture or stable version does not match this release'
        }
        return $identity
    } finally { $archive.Dispose() }
}

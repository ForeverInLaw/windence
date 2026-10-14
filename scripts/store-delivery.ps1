# Package-only Store updates. Never delete or reuse another submission.
function Submit-StorePackage {
    [CmdletBinding()]
    param([string]$PackagePath, [string]$AppId)
    $bundle = Join-Path ([IO.Path]::GetTempPath()) ('windence-store-' + [Guid]::NewGuid() + '.zip')
    $operation = 'authentication'
    try {
        $tenant = [Uri]::EscapeDataString($env:MSSTORE_TENANT_ID)
        $token = Invoke-RestMethod -Method Post `
            -Uri "https://login.microsoftonline.com/$tenant/oauth2/token" `
            -ContentType 'application/x-www-form-urlencoded' -TimeoutSec 60 -MaximumRedirection 0 `
            -Body @{ grant_type='client_credentials'; client_id=$env:MSSTORE_CLIENT_ID
                client_secret=$env:MSSTORE_CLIENT_SECRET; resource='https://manage.devcenter.microsoft.com' }
        if (-not $token.access_token) { throw 'Missing Store access token' }
        $headers = @{ Authorization = 'Bearer ' + $token.access_token }
        $root = 'https://manage.devcenter.microsoft.com/v1.0/my/applications/' + [Uri]::EscapeDataString($AppId)
        # The API rejects a competing draft atomically. Do not retry or delete it.
        $operation = 'create submission'
        $submission = Invoke-RestMethod -Method Post -Uri "$root/submissions" `
            -Headers $headers -TimeoutSec 60 -MaximumRedirection 0
        $operation = 'validate new submission'
        if (-not $submission.id -or $submission.status -ne 'PendingCommit' -or
            $null -eq $submission.applicationPackages) { throw 'Invalid new Store submission' }
        $upload = [Uri]$submission.fileUploadUrl
        if ($upload.Scheme -ne 'https' -or -not $upload.Host.EndsWith('.blob.core.windows.net')) {
            throw 'Invalid Store upload URL'
        }
        $owned = "$root/submissions/" + [Uri]::EscapeDataString($submission.id)
        foreach ($package in $submission.applicationPackages) { $package.fileStatus = 'PendingDelete' }
        $name = [IO.Path]::GetFileName($PackagePath)
        $submission.applicationPackages = @($submission.applicationPackages) + @(
            @{ fileName=$name; fileStatus='PendingUpload' })
        $operation = 'update submission'
        $null = Invoke-RestMethod -Method Put -Uri $owned -Headers $headers `
            -ContentType 'application/json; charset=utf-8' -TimeoutSec 60 -MaximumRedirection 0 `
            -Body ([Text.Encoding]::UTF8.GetBytes(($submission | ConvertTo-Json -Depth 100)))
        $operation = 'prepare upload'
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = [IO.Compression.ZipFile]::Open($bundle, [IO.Compression.ZipArchiveMode]::Create)
        try {
            $null = [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive,
                (Resolve-Path $PackagePath).Path, $name, [IO.Compression.CompressionLevel]::Optimal)
        } finally { $archive.Dispose() }
        $operation = 'upload package'
        $null = Invoke-WebRequest -Method Put -Uri $upload.AbsoluteUri -InFile $bundle `
            -Headers @{ 'x-ms-blob-type'='BlockBlob' } -ContentType 'application/zip' `
            -TimeoutSec 1800 -MaximumRedirection 0
        $operation = 'commit submission'
        $null = Invoke-RestMethod -Method Post -Uri "$owned/commit" -Headers $headers `
            -TimeoutSec 60 -MaximumRedirection 0
        $deadline = [DateTime]::UtcNow.AddMinutes(10)
        $operation = 'read commit status'
        do {
            $status = Invoke-RestMethod -Method Get -Uri "$owned/status" -Headers $headers `
                -TimeoutSec 60 -MaximumRedirection 0
            if ($status.status -in @('PreProcessing', 'Certification', 'Release',
                'PendingPublication', 'Publishing', 'Published')) {
                Write-Host 'Store accepted the submission. Certification and publication are pending.'
                return
            }
            if ($status.status -ne 'CommitStarted') { throw 'Store commit failed' }
            Start-Sleep -Seconds 5
        } while ([DateTime]::UtcNow -lt $deadline)
        throw 'Store commit timed out'
    } catch {
        # Provider errors may contain private notes, tokens or SAS URLs.
        $statusCode = [int]$_.Exception.Response.StatusCode
        $http = if ($statusCode -ge 100 -and $statusCode -le 599) { "; HTTP $statusCode" } else { '' }
        throw "Store submission failed during $operation$http. Inspect Partner Center before retrying; no draft was deleted."
    } finally {
        if ([IO.File]::Exists($bundle)) { [IO.File]::Delete($bundle) }
    }
}

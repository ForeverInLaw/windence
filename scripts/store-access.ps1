# Shared CLI access for publication and the manual access check.
function Get-StoreApplication {
    [CmdletBinding()]
    param([string]$AppId)
    foreach ($key in @('MSSTORE_TENANT_ID', 'MSSTORE_SELLER_ID', 'MSSTORE_CLIENT_ID', 'MSSTORE_CLIENT_SECRET')) {
        if (-not [Environment]::GetEnvironmentVariable($key)) { throw "Configure $key in the store environment" }
    }
    # Keep provider diagnostics out of public logs.
    $null = & msstore reconfigure --tenantId $env:MSSTORE_TENANT_ID --sellerId $env:MSSTORE_SELLER_ID `
        --clientId $env:MSSTORE_CLIENT_ID --clientSecret $env:MSSTORE_CLIENT_SECRET 2>&1
    if ($LASTEXITCODE -ne 0) { throw 'Store CLI authentication configuration failed' }
    # v0.4.3 sends diagnostics to stderr and PascalCase application JSON to stdout.
    $json = & msstore --output-stream stderr apps get $AppId 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the Store product; nothing submitted' }
    try { return ($json -join "`n") | ConvertFrom-Json } catch {
        throw 'Could not parse the Store product response; nothing submitted'
    }
}

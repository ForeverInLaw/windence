<#
.SYNOPSIS
Enables or disables temporary Store review access using the owner's saved session.
.DESCRIPTION
Run on the Windows account signed in to Windence. Both refresh credentials travel
over SSH stdin, never command arguments or a local export file. The returned code
is private: paste it only into Partner Center's Notes for certification.
#>
[CmdletBinding()]
param(
    [ValidateSet('Enable','Disable')][string]$Action = 'Enable',
    [string]$ClientId,
    [ValidatePattern('^[a-zA-Z0-9.-]+$')][string]$RemoteHost = 'spotic.qvq.cz',
    [ValidatePattern('^[a-zA-Z0-9_-]+$')][string]$SshUser = 'root',
    [ValidateRange(1,65535)][int]$Port = 22
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Send-ReviewConfiguration {
    param([string]$Payload, [bool]$Disable)
    $command = 'docker exec -i windence-review-broker python configure.py'
    if ($Disable) { $command += ' revoke' }
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = 'ssh.exe'
    $start.Arguments = "-p $Port $SshUser@$RemoteHost $command"
    $start.UseShellExecute = $false
    $start.RedirectStandardInput = $true
    $process = [System.Diagnostics.Process]::Start($start)
    try {
        $process.StandardInput.WriteLine($Payload)
        $process.StandardInput.Close()
        $process.WaitForExit()
        if ($process.ExitCode -ne 0) { throw 'Review configuration failed. No credentials were printed.' }
    } finally {
        $process.Dispose()
    }
}

if ($Action -eq 'Disable') {
    Send-ReviewConfiguration -Payload '' -Disable $true
    return
}
if (Get-Process Windence,Cadence,spotify-gpui-client -ErrorAction SilentlyContinue) {
    throw 'Close Windence completely with Ctrl+Q before sharing the saved session.'
}
if (-not $ClientId) { $ClientId = Read-Host 'Spotify Client ID from Windence Settings' }
if ($ClientId -notmatch '^[a-fA-F0-9]{32}$') { throw 'Client ID must contain 32 hexadecimal characters.' }

if (-not ('WindenceReviewCredentials' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

public static class WindenceReviewCredentials {
    [StructLayout(LayoutKind.Sequential)]
    private struct Credential {
        public uint Flags, Type;
        public IntPtr TargetName, Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint BlobSize;
        public IntPtr Blob;
        public uint Persist, AttributeCount;
        public IntPtr Attributes, TargetAlias, UserName;
    }
    [DllImport("advapi32.dll", EntryPoint="CredReadW", CharSet=CharSet.Unicode, SetLastError=true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ReadCredential(string target, uint type, uint flags, out IntPtr credential);
    [DllImport("advapi32.dll")]
    private static extern void CredFree(IntPtr credential);

    public static string Read(string target) {
        IntPtr pointer;
        if (!ReadCredential(target, 1, 0, out pointer))
            throw new Win32Exception(Marshal.GetLastWin32Error(), "Saved Windence credential is unavailable.");
        try {
            Credential value = (Credential)Marshal.PtrToStructure(pointer, typeof(Credential));
            if (value.BlobSize == 0 || value.BlobSize % 2 != 0)
                throw new InvalidOperationException("Saved credential has an invalid encoding.");
            return Marshal.PtrToStringUni(value.Blob, checked((int)value.BlobSize / 2));
        } finally { CredFree(pointer); }
    }
}
'@
}

try {
    $library = [WindenceReviewCredentials]::Read('oauth-token.com.cadence.spotify') | ConvertFrom-Json
    $playback = [WindenceReviewCredentials]::Read('playback-refresh-token.com.cadence.spotify')
    if (-not $library.refresh_token -or -not $playback) { throw 'Missing saved session' }
    $payload = @{
        client_id = $ClientId
        library_refresh_token = $library.refresh_token
        playback_refresh_token = $playback
    } | ConvertTo-Json -Compress
} catch {
    throw 'Could not read both saved sessions. Sign in to Windence and play a track first, then close it with Ctrl+Q.'
}
Send-ReviewConfiguration -Payload $payload -Disable $false
Remove-Variable payload,library,playback

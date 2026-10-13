# Check the helper without reading real credentials or contacting SSH.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'review-access.ps1'), [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw 'Review helper has PowerShell syntax errors' }
$definition = $ast.FindAll({ param($node)
    $node -is [System.Management.Automation.Language.CommandAst] -and
    $node.GetCommandName() -eq 'Add-Type'
}, $true)
if ($definition.Count -ne 1) { throw 'Expected one credential reader definition' }
Add-Type -TypeDefinition $definition[0].CommandElements[2].Value
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;
public static class WindenceReviewFixture {
    [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)]
    private struct Credential {
        public uint Flags, Type;
        public string TargetName, Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public uint BlobSize;
        public IntPtr Blob;
        public uint Persist, AttributeCount;
        public IntPtr Attributes;
        public string TargetAlias, UserName;
    }
    [DllImport("advapi32.dll", EntryPoint="CredWriteW", CharSet=CharSet.Unicode, SetLastError=true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool WriteCredential(ref Credential credential, uint flags);
    [DllImport("advapi32.dll", EntryPoint="CredDeleteW", CharSet=CharSet.Unicode, SetLastError=true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool DeleteCredential(string target, uint type, uint flags);
    public static void Write(string target, string secret) {
        byte[] bytes = Encoding.Unicode.GetBytes(secret);
        IntPtr blob = Marshal.AllocCoTaskMem(bytes.Length);
        try {
            Marshal.Copy(bytes, 0, blob, bytes.Length);
            Credential credential = new Credential {
                Type=1, TargetName=target, BlobSize=(uint)bytes.Length,
                Blob=blob, Persist=1, UserName="WindenceReviewTest"
            };
            if (!WriteCredential(ref credential, 0)) throw new Win32Exception(Marshal.GetLastWin32Error());
        } finally { Marshal.FreeCoTaskMem(blob); }
    }
    public static void Delete(string target) {
        if (!DeleteCredential(target, 1, 0)) throw new Win32Exception(Marshal.GetLastWin32Error());
    }
}
'@
$target = 'windence-review-test-' + [Guid]::NewGuid().ToString()
$written = $false
try {
    $value = 'synthetic credential with Unicode: ' + [char]0x0416
    [WindenceReviewFixture]::Write($target, $value)
    $written = $true
    if ([WindenceReviewCredentials]::Read($target) -cne $value) { throw 'Credential reader changed the value' }
} finally {
    if ($written) { [WindenceReviewFixture]::Delete($target) }
}
Write-Host 'Review helper syntax and isolated Credential Manager read passed.'

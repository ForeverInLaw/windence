<#
.SYNOPSIS
Captures real onboarding windows from two release ZIPs on an interactive runner.

.DESCRIPTION
This optional probe does not sign in or seed application data. It records native
desktop pixels and a report. Main-window layout, audio and SMTC need a signed-in
Windows check. A failed probe exits with code 1 and keeps its report.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$BaselineRunId,
    [Parameter(Mandatory = $true)][string]$FinalZip,
    [Parameter(Mandatory = $true)][string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$report = [ordered]@{
    status = 'running'; error = $null; baseline_run = $BaselineRunId
    baseline_commit = $null; final_run = $env:GITHUB_RUN_ID
    final_commit = $env:GITHUB_SHA; repository = $env:GITHUB_REPOSITORY
    runner_image = $env:ImageVersion; os = [Environment]::OSVersion.VersionString
    scope = 'Real native onboarding only. Images need visual review. Main window, audio, SMTC and other DPI settings are unverified.'
    captures = @()
}
$reportPath = Join-Path $OutputDirectory 'report.json'
$report | ConvertTo-Json -Depth 6 | Set-Content $reportPath -Encoding UTF8
Start-Transcript -Path (Join-Path $OutputDirectory 'probe.log') | Out-Null
$themePath = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize'
$originalTheme = @{}
$themeChanged = $false
$displayChanged = $false
$originalScreen = $null
$resolutionCommand = $null
$child = $null
$appData = $null
$failed = $false

try {
    if ($env:OS -ne 'Windows_NT') { throw 'Capture requires native Windows.' }
    if ($BaselineRunId -notmatch '^[1-9][0-9]*$') { throw 'Baseline run ID must be a positive integer.' }
    if (-not [Environment]::UserInteractive -or [Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0) {
        throw 'The runner has no interactive user session.'
    }

    # Read composed desktop pixels. GPUI renders through DirectComposition and
    # does not implement the WM_PRINT painting path used by PrintWindow.
    Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class CadenceCaptureNative {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint access);
    [DllImport("user32.dll")] public static extern bool CloseDesktop(IntPtr desktop);
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr after, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] public static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint msg, UIntPtr wparam, string lparam, uint flags, uint timeout, out UIntPtr result);
}
'@
    $desktop = [CadenceCaptureNative]::OpenInputDesktop(0, $false, 1)
    if ($desktop -eq [IntPtr]::Zero) { throw 'Windows denied access to the input desktop.' }
    [CadenceCaptureNative]::CloseDesktop($desktop) | Out-Null
    [CadenceCaptureNative]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
    Add-Type -AssemblyName System.Drawing, System.Windows.Forms
    $screen = [Windows.Forms.Screen]::PrimaryScreen.Bounds
    $originalScreen = $screen
    $report.desktop = @{ x = $screen.X; y = $screen.Y; width = $screen.Width; height = $screen.Height }
    $report.original_desktop = $report.desktop

    # BaseDirs uses the Windows known folder, not an APPDATA env override.
    $appData = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'Cadence'
    if (Test-Path $appData) { throw "Expected a clean runner profile; Cadence data already exists at $appData." }
    if (Get-Process -Name Cadence -ErrorAction SilentlyContinue) { throw 'Another Cadence process is already running.' }
    $env:SPOTIFY_CLIENT_ID = $null

    $metadataText = & gh run view $BaselineRunId --repo $env:GITHUB_REPOSITORY --json conclusion,headSha,workflowName
    if ($LASTEXITCODE -ne 0) { throw 'Could not read the baseline workflow run.' }
    $metadata = $metadataText | ConvertFrom-Json
    if ($metadata.conclusion -ne 'success' -or $metadata.workflowName -ne 'Release') {
        throw 'The baseline must be a successful Release workflow run.'
    }
    $report.baseline_commit = $metadata.headSha
    $buildRoot = Join-Path $env:RUNNER_TEMP "cadence-capture-builds-$env:GITHUB_RUN_ID"
    $download = Join-Path $buildRoot 'download'
    & gh run download $BaselineRunId --repo $env:GITHUB_REPOSITORY --name cadence-windows-x64 --dir $download
    if ($LASTEXITCODE -ne 0) { throw 'Could not download the complete baseline artifact.' }
    $baselineZip = @(Get-ChildItem $download -File -Filter 'Cadence-*-windows-x64.zip')
    if ($baselineZip.Count -ne 1) { throw 'Expected one baseline Windows ZIP.' }
    $builds = [ordered]@{ baseline = $baselineZip[0].FullName; final = (Resolve-Path $FinalZip).Path }
    $executables = @{}
    foreach ($name in $builds.Keys) {
        $destination = Join-Path $buildRoot $name
        Expand-Archive -LiteralPath $builds[$name] -DestinationPath $destination
        $exe = @(Get-ChildItem $destination -Recurse -File -Filter 'Cadence.exe')
        if ($exe.Count -ne 1) { throw "Expected one Cadence.exe in the $name ZIP." }
        $executables[$name] = $exe[0].FullName
    }

    function Save-DesktopPixels([Drawing.Rectangle]$Bounds, [string]$Path) {
        $bitmap = New-Object Drawing.Bitmap $Bounds.Width, $Bounds.Height
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen($Bounds.X, $Bounds.Y, 0, 0, $Bounds.Size)
            $colors = New-Object 'Collections.Generic.HashSet[int]'
            $brightness = 0.0
            $samples = 0
            for ($y = 0; $y -lt $bitmap.Height; $y += 16) {
                for ($x = 0; $x -lt $bitmap.Width; $x += 16) {
                    $color = $bitmap.GetPixel($x, $y)
                    $colors.Add($color.ToArgb()) | Out-Null
                    $brightness += (2 * $color.R + 5 * $color.G + $color.B) / 8
                    $samples++
                }
            }
            $bitmap.Save($Path, [Drawing.Imaging.ImageFormat]::Png)
            if ($colors.Count -lt 8) { throw "Screen copy appears blank ($($colors.Count) sampled colors): $Path" }
            return [Math]::Round($brightness / $samples, 1)
        } finally {
            $graphics.Dispose()
            $bitmap.Dispose()
        }
    }

    foreach ($property in @('AppsUseLightTheme', 'SystemUsesLightTheme')) {
        $originalTheme[$property] = if (Test-Path $themePath) {
            Get-ItemPropertyValue $themePath -Name $property -ErrorAction SilentlyContinue
        } else { $null }
    }
    if (-not (Test-Path $themePath)) { New-Item -Path $themePath -Force | Out-Null }
    $themeChanged = $true
    foreach ($theme in @('light', 'dark')) {
        $value = if ($theme -eq 'light') { 1 } else { 0 }
        foreach ($property in $originalTheme.Keys) {
            New-ItemProperty $themePath -Name $property -Value $value -PropertyType DWord -Force | Out-Null
        }
        $result = [UIntPtr]::Zero
        [CadenceCaptureNative]::SendMessageTimeout([IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, 'ImmersiveColorSet', 2, 1000, [ref]$result) | Out-Null
        $settings = [Windows.UI.ViewManagement.UISettings, Windows.UI.ViewManagement, ContentType = WindowsRuntime]::new()
        $themeDeadline = [DateTime]::UtcNow.AddSeconds(5)
        do {
            $color = $settings.GetColorValue([Windows.UI.ViewManagement.UIColorType, Windows.UI.ViewManagement, ContentType = WindowsRuntime]::Foreground)
            $actualDark = (5 * $color.G + 2 * $color.R + $color.B) -gt (8 * 128)
            if ($actualDark -eq ($theme -eq 'dark')) { break }
            Start-Sleep -Milliseconds 200
        } while ([DateTime]::UtcNow -lt $themeDeadline)
        if ($actualDark -ne ($theme -eq 'dark')) { throw "Windows UISettings did not honor the requested $theme appearance." }

        foreach ($name in $builds.Keys) {
            $label = "$name-$theme"
            try {
                $child = Start-Process -FilePath $executables[$name] -WorkingDirectory (Split-Path $executables[$name]) -PassThru
                $deadline = [DateTime]::UtcNow.AddSeconds(30)
                do {
                    if ($child.HasExited) { throw "The $label app exited before opening a window (code $($child.ExitCode))." }
                    $child.Refresh()
                    $hwnd = $child.MainWindowHandle
                    if ($hwnd -ne [IntPtr]::Zero) { break }
                    Start-Sleep -Milliseconds 200
                } while ([DateTime]::UtcNow -lt $deadline)
                if ($hwnd -eq [IntPtr]::Zero) { throw "The $label app did not open a native window within 30 seconds." }
                $dpi = [CadenceCaptureNative]::GetDpiForWindow($hwnd)
                if ($dpi -eq 0) { throw "Windows did not report DPI for $label." }
                $width = [int](1200 * $dpi / 96)
                $height = [int](720 * $dpi / 96)
                if (($screen.Width -lt $width -or $screen.Height -lt $height) -and -not $displayChanged) {
                    # Use the built-in Server Core command once. No alternate
                    # driver, remote desktop or cropped-window fallback.
                    # https://learn.microsoft.com/en-us/powershell/module/servercore/set-displayresolution
                    $resolutionCommand = Get-Command 'ServerCore\Set-DisplayResolution' -ErrorAction SilentlyContinue
                    if ($null -eq $resolutionCommand) {
                        throw 'The desktop cannot fit onboarding and the built-in ServerCore Set-DisplayResolution command is unavailable.'
                    }
                    $report.resolution_attempt = @{ command = 'ServerCore Set-DisplayResolution'; width = 1920; height = 1080 }
                    $displayChanged = $true
                    & $resolutionCommand -Width 1920 -Height 1080 -Force | Out-Null
                    $displayDeadline = [DateTime]::UtcNow.AddSeconds(5)
                    do {
                        $screen = [Windows.Forms.Screen]::FromPoint($originalScreen.Location).Bounds
                        if ($screen.Width -eq 1920 -and $screen.Height -eq 1080) { break }
                        Start-Sleep -Milliseconds 200
                    } while ([DateTime]::UtcNow -lt $displayDeadline)
                    $report.desktop = @{ x = $screen.X; y = $screen.Y; width = $screen.Width; height = $screen.Height }
                    $report.resolution_applied = $screen.Width -eq 1920 -and $screen.Height -eq 1080
                    if (-not $report.resolution_applied) {
                        throw "Windows did not apply the requested 1920 by 1080 display mode; actual size is $($screen.Width) by $($screen.Height)."
                    }
                }
                if ($screen.Width -lt $width -or $screen.Height -lt $height) {
                    throw "The desktop is too small for the complete $width by $height onboarding window at $dpi DPI."
                }
                if (-not [CadenceCaptureNative]::SetWindowPos($hwnd, [IntPtr]::Zero, $screen.X, $screen.Y, $width, $height, 0x40)) {
                    throw "Could not position the $label window."
                }
                [CadenceCaptureNative]::SetForegroundWindow($hwnd) | Out-Null
                Start-Sleep -Seconds 2
                if ([CadenceCaptureNative]::GetForegroundWindow() -ne $hwnd) { throw "Windows denied foreground activation for $label." }
                $rect = New-Object CadenceCaptureNative+Rect
                if (-not [CadenceCaptureNative]::GetWindowRect($hwnd, [ref]$rect)) { throw 'Could not read window bounds.' }
                $bounds = New-Object Drawing.Rectangle $rect.Left, $rect.Top, ($rect.Right - $rect.Left), ($rect.Bottom - $rect.Top)
                if (-not $screen.Contains($bounds)) { throw "The $label window is clipped by the display." }
                $brightness = Save-DesktopPixels $bounds (Join-Path $OutputDirectory "$label.png")
                [Windows.Forms.SendKeys]::SendWait('{TAB}')
                Start-Sleep -Milliseconds 300
                if ([CadenceCaptureNative]::GetForegroundWindow() -ne $hwnd) { throw "Foreground changed after Tab for $label." }
                Save-DesktopPixels $bounds (Join-Path $OutputDirectory "$label-keyboard.png") | Out-Null
                Save-DesktopPixels $screen (Join-Path $OutputDirectory "$label-desktop.png") | Out-Null
                $report.captures += @{
                    name = $label; hwnd = $hwnd.ToInt64(); dpi = $dpi; foreground_verified = $true
                    tab_sent = $true; x = $bounds.X; y = $bounds.Y; width = $bounds.Width; height = $bounds.Height
                    sampled_brightness = $brightness
                }
            } finally {
                # Closing a window leaves Cadence alive. Stop only this probe's
                # child so the next build cannot activate the previous instance.
                if ($null -ne $child) {
                    if (-not $child.HasExited) { $child.Kill(); $child.WaitForExit(5000) | Out-Null }
                    $child.Dispose()
                    $child = $null
                }
                $log = Join-Path $appData 'cadence.log'
                if (Test-Path $log) { Copy-Item $log (Join-Path $OutputDirectory "$label-app.log") }
                # Keep each build's profile separate: a newer SQLite schema
                # would prevent the next baseline build from opening.
                if (Test-Path $appData) {
                    Move-Item -LiteralPath $appData -Destination (Join-Path $buildRoot "$label-profile")
                }
            }
        }
    }
    $measurements = @($report.captures | ForEach-Object { "$($_.dpi)/$($_.width)/$($_.height)" } | Select-Object -Unique)
    if ($measurements.Count -ne 1) { throw 'The compared windows used different DPI or dimensions.' }
    foreach ($name in $builds.Keys) {
        $light = $report.captures | Where-Object { $_.name -eq "$name-light" }
        $dark = $report.captures | Where-Object { $_.name -eq "$name-dark" }
        if ($light.sampled_brightness - $dark.sampled_brightness -lt 30) {
            throw "The $name images did not show distinct light and dark appearances."
        }
    }
    $report.status = 'captured-needs-visual-review'
} catch {
    $failed = $true
    $report.status = 'failed'
    $report.error = $_.Exception.Message
    Write-Warning $report.error
} finally {
    if ($displayChanged) {
        try {
            & $resolutionCommand -Width $originalScreen.Width -Height $originalScreen.Height -Force | Out-Null
            $displayDeadline = [DateTime]::UtcNow.AddSeconds(5)
            do {
                $restored = [Windows.Forms.Screen]::FromPoint($originalScreen.Location).Bounds
                if ($restored.Width -eq $originalScreen.Width -and $restored.Height -eq $originalScreen.Height) { break }
                Start-Sleep -Milliseconds 200
            } while ([DateTime]::UtcNow -lt $displayDeadline)
            $report.original_resolution_restored = $restored.Width -eq $originalScreen.Width -and $restored.Height -eq $originalScreen.Height
            if (-not $report.original_resolution_restored) { throw 'Windows did not restore the original display dimensions.' }
        } catch {
            $failed = $true
            $report.status = 'failed'
            $report.error = "$($report.error) Could not restore runner resolution: $($_.Exception.Message)".Trim()
        }
    }
    if ($themeChanged) {
        try {
            foreach ($property in $originalTheme.Keys) {
                if ($null -eq $originalTheme[$property]) {
                    Remove-ItemProperty $themePath -Name $property -ErrorAction SilentlyContinue
                } else {
                    New-ItemProperty $themePath -Name $property -Value $originalTheme[$property] -PropertyType DWord -Force | Out-Null
                }
            }
            $result = [UIntPtr]::Zero
            [CadenceCaptureNative]::SendMessageTimeout([IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, 'ImmersiveColorSet', 2, 1000, [ref]$result) | Out-Null
        } catch {
            $failed = $true
            $report.status = 'failed'
            $report.error = "$($report.error) Could not restore runner theme: $($_.Exception.Message)".Trim()
        }
    }
    $report | ConvertTo-Json -Depth 6 | Set-Content $reportPath -Encoding UTF8
    if ($env:GITHUB_STEP_SUMMARY) {
        @("### Native onboarding capture", "", "Status: $($report.status)", "", $report.error, "", $report.scope) |
            Add-Content $env:GITHUB_STEP_SUMMARY -Encoding UTF8
    }
    Stop-Transcript | Out-Null
}
if ($failed) { exit 1 }

; Windows installer for Cadence. Build it with:
;
;   scripts\package-windows.ps1
;
; which builds the release binary first and then calls this script. The
; macOS side of packaging lives in package-app.sh and does not run here.
;
; The install is per-user on purpose: it lands in the user's own Programs
; folder, so nothing asks for an administrator and the app can be removed
; from Settings like any other.

#define AppName "Cadence"
#define AppVersion "0.4.0"
#define AppPublisher "Cadence"
#define AppUrl "https://github.com/ForeverInLaw/windence"
#define AppExe "Cadence.exe"
#define SourceDir "..\"

[Setup]
AppId={{8C3F5D2A-6B41-4E88-9A17-2D5C7E4B9F31}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
AppUpdatesURL={#AppUrl}
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile={#SourceDir}LICENSE
SetupIconFile={#SourceDir}assets\AppIcon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName} {#AppVersion}
OutputDir={#SourceDir}dist
OutputBaseFilename={#AppName}-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
MinVersion=10.0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}target\release\spotify-gpui-client.exe"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion
; The one runtime library the binary imports that Windows does not ship
; itself. Kept beside the app so nothing has to be installed separately.
Source: "{#SourceDir}dist\vcruntime140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

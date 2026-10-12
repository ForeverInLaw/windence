; Windows installer for Windence. Build it with:
;
;   scripts\package-windows.ps1
;
; which builds the release binary first and then calls this script. The
; macOS side of packaging lives in package-app.sh and does not run here.
;
; The install is per-user on purpose: it lands in the user's own Programs
; folder, so nothing asks for an administrator and the app can be removed
; from Settings like any other.

#define AppName "Windence"
; package-windows.ps1 passes the version from Cargo.toml as /DAppVersion. The
; fallback only shows up when ISCC is run on its own.
#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#define AppPublisher "redstone.md"
#define AppUrl "https://github.com/ForeverInLaw/windence"
#define AppExe "Windence.exe"
#define SourceDir "..\"

[Setup]
; Keep the existing identity so this installer upgrades earlier Cadence installs.
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
UsePreviousGroup=no
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
LicenseFile={#SourceDir}LICENSE
SetupIconFile={#SourceDir}assets\AppIcon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName} {#AppVersion}
OutputDir={#SourceDir}dist\development
OutputBaseFilename={#AppName}-{#AppVersion}-windows-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
MinVersion=10.0

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; Only remove files installed under the old public name. The profile is elsewhere.
Type: files; Name: "{app}\Cadence.exe"
Type: files; Name: "{userprograms}\Cadence\Cadence.lnk"
Type: dirifempty; Name: "{userprograms}\Cadence"
Type: files; Name: "{autodesktop}\Cadence.lnk"

[Files]
Source: "{#SourceDir}target\release\spotify-gpui-client.exe"; DestDir: "{app}"; DestName: "{#AppExe}"; Flags: ignoreversion
; The one runtime library the binary imports that Windows does not ship
; itself. Kept beside the app so nothing has to be installed separately.
Source: "{#SourceDir}dist\development\vcruntime140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

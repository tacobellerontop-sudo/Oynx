; Inno Setup script for the Windows installer published with each release.
;
; Built by scripts/build-installer.ps1, which passes the version, the exe to
; package, and the output name:
;
;   iscc /DAppVersion=0.1.3 /DSourceExe=C:\path\to\oynx.exe installer\oynx.iss
;
; Oynx installs per user under %LOCALAPPDATA%\Programs\Oynx, so neither
; installing nor the in-app updater needs administrator rights. Settings,
; credentials and caches live elsewhere (%APPDATA%\Oynx and
; %LOCALAPPDATA%\Oynx) and are left in place by the uninstaller.
;
; The in-app updater runs this installer silently. It passes /LAUNCH=0 when
; Oynx is quitting and the update should only take effect on the next start;
; otherwise the installer starts Oynx once it finishes.

#ifndef AppVersion
  #error Pass the version to build with /DAppVersion=x.y.z
#endif
#ifndef SourceExe
  #define SourceExe "..\target\release\oynx.exe"
#endif

[Setup]
; Never change AppId: it is how upgrades find the existing installation.
AppId={{E6360966-7A5E-4D46-8B04-72FC521A4DBD}
AppName=Oynx
AppVersion={#AppVersion}
AppVerName=Oynx {#AppVersion}
AppPublisher=tacobellerontop-sudo
AppPublisherURL=https://github.com/tacobellerontop-sudo/Oynx
AppSupportURL=https://github.com/tacobellerontop-sudo/Oynx/issues
AppUpdatesURL=https://github.com/tacobellerontop-sudo/Oynx/releases
DefaultDirName={autopf}\Oynx
DefaultGroupName=Oynx
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir=..\dist
OutputBaseFilename=oynx-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\oynx.exe
UninstallDisplayName=Oynx
; Close a running Oynx before replacing its files, and leave starting it again
; to the [Run] entry below so /LAUNCH=0 is respected.
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "oynx.exe"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Oynx"; Filename: "{app}\oynx.exe"
Name: "{autodesktop}\Oynx"; Filename: "{app}\oynx.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\oynx.exe"; Description: "{cm:LaunchProgram,Oynx}"; Flags: nowait postinstall; Check: ShouldLaunch

[Code]
function ShouldLaunch: Boolean;
begin
  Result := ExpandConstant('{param:LAUNCH|1}') <> '0';
end;

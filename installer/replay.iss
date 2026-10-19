; Replay installer (Inno Setup 6). Per-user install: no admin prompt, and
; the auto-updater can replace the program file in place.
; Build: iscc /DAppVersion=0.1.0 /DSourceDir=..\dist\stage installer\replay.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\target\release"
#endif

[Setup]
AppId={{5B8E2C6A-3F1D-4C9A-9E2B-7A4D1F0C8E31}
AppName=Replay
AppVersion={#AppVersion}
AppVerName=Replay {#AppVersion}
AppPublisher=Replay contributors
AppPublisherURL=https://github.com/Car-Role/replay
AppSupportURL=https://github.com/Car-Role/replay/issues
AppUpdatesURL=https://github.com/Car-Role/replay/releases
DefaultDirName={localappdata}\Programs\Replay
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
OutputDir=..\dist
OutputBaseFilename=Replay-Setup-windows-x64
SetupIconFile=..\assets\generated\replay.ico
UninstallDisplayIcon={app}\replay.exe
UninstallDisplayName=Replay
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=force
RestartApplications=no
LicenseFile=..\LICENSE

[Tasks]
Name: "autostart"; Description: "Start Replay when I sign in to Windows"; GroupDescription: "Options:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Options:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\replay.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Replay"; Filename: "{app}\replay.exe"; Comment: "Instant replay for games"
Name: "{userprograms}\Replay library"; Filename: "{app}\replay.exe"; Parameters: "--library"; Comment: "Your clips, editor and settings"
Name: "{userdesktop}\Replay"; Filename: "{app}\replay.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Replay"; ValueData: """{app}\replay.exe"""; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\replay.exe"; Description: "Start Replay now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM replay.exe /F"; Flags: runhidden; RunOnceId: "StopReplay"

[UninstallDelete]
Type: files; Name: "{app}\replay.old.exe"

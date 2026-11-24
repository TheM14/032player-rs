; Windows installer for 032player (Inno Setup 6.5+).
; Built by .github/workflows/release.yml:
;   iscc /DAppVersion=0.1.0 /DSourceDir=<dir with 032player.exe and libmpv-2.dll> /DOutputDir=dist 032player.iss
; Per-user install (no administrator rights); user data in %APPDATA%\032player
; stays on uninstall.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\target\x86_64-pc-windows-msvc\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

[Setup]
AppId={{6F1C3E52-7D0B-4E1A-9C55-032032032032}
AppName=032player
AppVersion={#AppVersion}
AppVerName=032player {#AppVersion}
AppPublisher=032
AppPublisherURL=https://github.com/lux032/032player-rs
AppSupportURL=https://github.com/lux032/032player-rs/issues
DefaultDirName={localappdata}\Programs\032player
DefaultGroupName=032player
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=032player-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\crates\app\assets\icon.ico
UninstallDisplayIcon={app}\032player.exe
LicenseFile=..\..\LICENSE
Compression=lzma2/normal
LZMANumBlockThreads=2
LZMAUseSeparateProcess=yes
SolidCompression=yes
WizardStyle=modern
; The single-instance guard of the app is a named mutex per data dir; ask
; the user to close the running player instead.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\032player.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\..\crates\app\assets\fonts\OFL-NotoSansSC.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "..\..\crates\app\assets\fonts\OFL-SourceSerif4.txt"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "..\..\crates\app\assets\fonts\README.md"; DestDir: "{app}\licenses"; DestName: "FONTS.md"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\032player"; Filename: "{app}\032player.exe"
Name: "{autodesktop}\032player"; Filename: "{app}\032player.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\032player.exe"; Description: "{cm:LaunchProgram,032player}"; Flags: nowait postinstall skipifsilent

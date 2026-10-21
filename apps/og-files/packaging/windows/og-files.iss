; Inno Setup script for the OG Files installer (built by the release
; workflow). Compile with:
;   iscc /DAppVersion=X.Y.Z /DArch=x64|arm64 /DSrcDir=<staged folder> og-files.iss
; Installs per user by default (no admin prompt); the first page offers
; "install for all users" instead.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Arch
  #define Arch "x64"
#endif
#ifndef SrcDir
  #define SrcDir "..\..\dist\stage"
#endif

[Setup]
AppId={{6A8E4C1F-3B2D-4E7A-9C51-0F0D5B2A7E11}
AppName=OG Files
AppVersion={#AppVersion}
AppVerName=OG Files {#AppVersion}
AppPublisher=OmegaGiven
AppPublisherURL=https://github.com/OmegaGiven/og-files
AppSupportURL=https://github.com/OmegaGiven/og-files/issues
DefaultDirName={autopf}\OG Files
DefaultGroupName=OG Files
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed={#Arch}compatible
ArchitecturesInstallIn64BitMode={#Arch}compatible
LicenseFile={#SrcDir}\LICENSE.txt
SetupIconFile={#SrcDir}\og-files.ico
UninstallDisplayIcon={app}\og-files.exe
UninstallDisplayName=OG Files
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir=.
OutputBaseFilename=og-files-{#AppVersion}-windows-{#Arch}-setup
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "foldermenu"; Description: "Add ""Open in OG Files"" to the folder right-click menu"; GroupDescription: "Explorer:"

[Files]
Source: "{#SrcDir}\og-files.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#SrcDir}\og-files.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SrcDir}\THIRD-PARTY.txt"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{group}\OG Files"; Filename: "{app}\og-files.exe"
Name: "{autodesktop}\OG Files"; Filename: "{app}\og-files.exe"; Tasks: desktopicon

[Registry]
; HKA = HKCU for a per-user install, HKLM for all users
Root: HKA; Subkey: "Software\Classes\Directory\shell\OGFiles"; ValueType: string; ValueName: ""; ValueData: "Open in OG Files"; Flags: uninsdeletekey; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\OGFiles"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\og-files.exe"",0"; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\OGFiles\command"; ValueType: string; ValueName: ""; ValueData: """{app}\og-files.exe"" ""%1"""; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\OGFiles"; ValueType: string; ValueName: ""; ValueData: "Open in OG Files"; Flags: uninsdeletekey; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\OGFiles"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\og-files.exe"",0"; Tasks: foldermenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\OGFiles\command"; ValueType: string; ValueName: ""; ValueData: """{app}\og-files.exe"" ""%V"""; Tasks: foldermenu

[Run]
Filename: "{app}\og-files.exe"; Description: "{cm:LaunchProgram,OG Files}"; Flags: nowait postinstall skipifsilent

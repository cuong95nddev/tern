; Inno Setup script for the Windows installer, built by
; scripts/release-windows.ps1:
;   iscc /DVersion=0.1.2 /DStageDir=<folder with tern.exe> /DOutputDir=<dir> tern.iss
; Installs per user (no admin prompt) into %LOCALAPPDATA%\Programs\Tern, so
; Tern can update itself: the updater runs this installer with /VERYSILENT.

#ifndef Version
  #error Pass /DVersion=X.Y.Z
#endif
#ifndef StageDir
  #error Pass /DStageDir=<folder with tern.exe>
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
AppId={{6F1C2B57-7E0A-4C8B-9B1E-3D5A2C9F4E61}
AppName=Tern
AppVersion={#Version}
AppVerName=Tern {#Version}
AppPublisher=Tern
AppPublisherURL=https://github.com/cuong95nddev/tern
AppSupportURL=https://github.com/cuong95nddev/tern/issues
AppUpdatesURL=https://github.com/cuong95nddev/tern/releases
DefaultDirName={localappdata}\Programs\Tern
DefaultGroupName=Tern
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Tern-{#Version}-windows-x64-setup
SetupIconFile=..\..\assets\icon\windows\tern.ico
UninstallDisplayIcon={app}\tern.exe
UninstallDisplayName=Tern
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no
MinVersion=10.0.17763

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Add the ""tern"" command to PATH (open a folder with ""tern <folder>"")"; GroupDescription: "Other:"
Name: "contextmenu"; Description: "Add ""Open in Tern"" to the folder context menu"; GroupDescription: "Other:"

[Files]
Source: "{#StageDir}\tern.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{autoprograms}\Tern"; Filename: "{app}\tern.exe"
Name: "{autodesktop}\Tern"; Filename: "{app}\tern.exe"; Tasks: desktopicon

[Registry]
; "Open in Tern" on folders and on a folder's background (per user).
Root: HKA; Subkey: "Software\Classes\Directory\shell\Tern"; ValueType: string; ValueName: ""; ValueData: "Open in Tern"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Tern"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\tern.exe"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Tern\command"; ValueType: string; ValueName: ""; ValueData: """{app}\tern.exe"" ""%V"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Tern"; ValueType: string; ValueName: ""; ValueData: "Open in Tern"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Tern"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\tern.exe"""; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Tern\command"; ValueType: string; ValueName: ""; ValueData: """{app}\tern.exe"" ""%V"""; Tasks: contextmenu

[Run]
Filename: "{app}\tern.exe"; Description: "{cm:LaunchProgram,Tern}"; Flags: nowait postinstall skipifsilent

[Code]
const
  EnvKey = 'Environment';

function PathHas(Paths, Dir: string): Boolean;
begin
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0;
end;

procedure AddToPath(Dir: string);
var
  Paths: string;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Paths) then
    Paths := '';
  if PathHas(Paths, Dir) then
    exit;
  if (Paths <> '') and (Copy(Paths, Length(Paths), 1) <> ';') then
    Paths := Paths + ';';
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Paths + Dir);
end;

procedure RemoveFromPath(Dir: string);
var
  Paths: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Paths) then
    exit;
  Paths := ';' + Paths + ';';
  P := Pos(';' + Uppercase(Dir) + ';', Uppercase(Paths));
  if P = 0 then
    exit;
  Delete(Paths, P, Length(Dir) + 1);
  Paths := Copy(Paths, 2, Length(Paths) - 2);
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Paths);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    RemoveFromPath(ExpandConstant('{app}'));
end;

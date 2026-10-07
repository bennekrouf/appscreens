; AppScreens — Windows installer (Inno Setup)
; Build: iscc /DMyAppVersion=X.Y.Z installer\installer.iss
; Output: dist\appscreens-windows-setup.exe

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName      "AppScreens"
#define MyAppPublisher "Mayorana"
#define MyAppURL       "https://mayorana.ch/en/apps/appscreens"
#define MyAppExeName   "appscreens.exe"
; Must never change: Windows recognises upgrades of this app by it.
#define MyAppId        "88D3E921-9260-472D-AE4E-434F430C5186"

[Setup]
AppId={{{#MyAppId}}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL=https://mayorana.ch/en/apps/appscreens/releases
VersionInfoVersion={#MyAppVersion}
VersionInfoProductVersion={#MyAppVersion}
VersionInfoProductName={#MyAppName}
VersionInfoDescription={#MyAppName} {#MyAppVersion} Installer
VersionInfoCompany={#MyAppPublisher}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
; No version in the name, so the "latest" download link never changes.
OutputBaseFilename=appscreens-windows-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Per-user by default: no UAC prompt, works on locked-down machines. IT can
; still install for everyone with:  appscreens-windows-setup.exe /ALLUSERS /VERYSILENT
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; WebView2 ships with Windows 10 1809+ and 11.
MinVersion=10.0.17763
UninstallDisplayName={#MyAppName} {#MyAppVersion}
CloseApplications=yes
#if FileExists(AddBackslash(SourcePath) + "..\assets\icon.ico")
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\icon.ico
#endif
LicenseFile=..\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"

[Files]
Source: "..\target\release\{#MyAppExeName}";     DestDir: "{app}"; Flags: ignoreversion
; Only present when the WebView2 loader is linked dynamically.
Source: "..\target\release\WebView2Loader.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
#if FileExists(AddBackslash(SourcePath) + "..\assets\icon.ico")
; Shortcuts point at this explicitly — some Windows builds fail to extract
; the icon embedded in the .exe and show a generic one.
Source: "..\assets\icon.ico";                   DestDir: "{app}"; Flags: ignoreversion
#endif

[Icons]
#if FileExists(AddBackslash(SourcePath) + "..\assets\icon.ico")
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"
Name: "{autodesktop}\{#MyAppName}";     Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon
#else
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}";     Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon
#endif
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"

[Run]
; As the user, not elevated — WebView2 shows a black window in an admin process.
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent runascurrentuser

[Code]
const
  UninstallRoot = 'Software\Microsoft\Windows\CurrentVersion\Uninstall';

{ Versions up to 0.1.24 were .msi packages. Find that install, if any, by
  its name in Programs and Features, so it can be removed first — otherwise
  the user ends up with two AppScreens. }
function FindMsiInstall(RootKey: Integer; var ProductCode: String): Boolean;
var
  Keys: TArrayOfString;
  I: Integer;
  Name: String;
  IsMsi: Cardinal;
begin
  Result := False;
  if not RegGetSubkeyNames(RootKey, UninstallRoot, Keys) then
    Exit;
  for I := 0 to GetArrayLength(Keys) - 1 do
  begin
    if RegQueryStringValue(RootKey, UninstallRoot + '\' + Keys[I], 'DisplayName', Name)
       and (Name = '{#MyAppName}')
       and RegQueryDWordValue(RootKey, UninstallRoot + '\' + Keys[I], 'WindowsInstaller', IsMsi)
       and (IsMsi = 1) then
    begin
      ProductCode := Keys[I];
      Result := True;
      Exit;
    end;
  end;
end;

function RemoveMsiInstall(): Boolean;
var
  ProductCode: String;
  ResultCode: Integer;
  Found: Boolean;
begin
  Result := True;
  Found := FindMsiInstall(HKCU, ProductCode);
  if not Found then
    Found := FindMsiInstall(HKLM64, ProductCode);
  if not Found then
    Found := FindMsiInstall(HKLM32, ProductCode);
  if not Found then
    Exit;
  if MsgBox('An older AppScreens (installed from an .msi) is on this computer.' + #13#10 + #13#10 +
            'It will be removed first. Your projects and settings are kept.' + #13#10 + #13#10 +
            'Continue?', mbConfirmation, MB_YESNO) = IDNO then
  begin
    Result := False;
    Exit;
  end;
  { msiexec asks for administrator rights itself if that install was per-machine. }
  Exec('msiexec.exe', '/x ' + ProductCode + ' /qb', '', SW_SHOW, ewWaitUntilTerminated, ResultCode);
end;

function GetInstalledVersion(): String;
var
  Key: String;
begin
  Key := UninstallRoot + '\{{#MyAppId}}_is1';
  if not RegQueryStringValue(HKCU, Key, 'DisplayVersion', Result) then
    if not RegQueryStringValue(HKLM, Key, 'DisplayVersion', Result) then
      Result := '';
end;

function InitializeSetup(): Boolean;
var
  Installed: String;
begin
  Result := RemoveMsiInstall();
  if not Result then
    Exit;
  Installed := GetInstalledVersion();
  if (Installed <> '') and (Installed = '{#MyAppVersion}') then
    Result := MsgBox('AppScreens {#MyAppVersion} is already installed.' + #13#10 + #13#10 +
                     'Install it again?', mbConfirmation, MB_YESNO) = IDYES;
  { A newer or older version is simply replaced in place: same AppId, same folder. }
end;

; AIS Analytics — Windows Installer
; Build: iscc /DMyAppVersion=X.Y.Z installer\installer.iss
; Output: dist\ais-analytics-setup.exe

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName      "AIS Analytics"
#define MyAppPublisher "Bennekrouf"
#define MyAppURL       "https://github.com/bennekrouf/ais-analytics"
#define MyAppExeName   "ais-analytics.exe"

[Setup]
; Distinct from ais-monitor's AppId — the two apps must not share an
; uninstall registry entry, or installing one would offer to remove the other.
AppId={{E26DC47F-9F9F-4A69-8AD0-C5E17B2CE016}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases/latest
; Embed version info into the compiled setup.exe so File Explorer
; (right-click → Properties → Details) shows the version.
VersionInfoVersion={#MyAppVersion}
VersionInfoProductVersion={#MyAppVersion}
VersionInfoProductName={#MyAppName}
VersionInfoDescription={#MyAppName} {#MyAppVersion} Installer
VersionInfoCompany={#MyAppPublisher}
; Admin install — UAC appears once so the dependency script can install
; the Azure CLI without self-elevation.
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
; No version in the filename — stable name lets the same download URL
; ("…/releases/latest/download/ais-analytics-setup.exe") always work. Version
; info is still visible in the installer wizard, exe properties, and the
; app's own title bar.
OutputBaseFilename=ais-analytics-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Per-user by default, so a developer on a locked-down machine installs with
; no UAC prompt at all. IT keeps the machine-wide path via the command line:
;   installer.exe /ALLUSERS /VERYSILENT
; One artifact serves both audiences instead of forcing everyone to elevate.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
UninstallDisplayName={#MyAppName} {#MyAppVersion}
CloseApplications=yes
; Branding — uses assets\icon.ico if present. Skipped silently otherwise.
#if FileExists(AddBackslash(SourcePath) + "..\assets\icon.ico")
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\icon.ico
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; \
  Description: "Create a &desktop shortcut"; \
  GroupDescription: "Additional shortcuts:"

[Files]
Source: "..\target\release\ais-analytics.exe";      DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\WebView2Loader.dll";   DestDir: "{app}"; Flags: ignoreversion
Source: "..\scripts\setup-windows.ps1";           DestDir: "{app}"; Flags: ignoreversion
; Ship the .ico alongside the .exe so shortcuts can point at it explicitly —
; otherwise some Windows builds fail to extract the embedded icon for shortcut
; display, leaving a generic icon on the Start menu / desktop.
#if FileExists(AddBackslash(SourcePath) + "..\assets\icon.ico")
Source: "..\assets\icon.ico";                     DestDir: "{app}"; Flags: ignoreversion
#endif

[Icons]
#if FileExists(AddBackslash(SourcePath) + "..\assets\icon.ico")
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}";   Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon
#else
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}";   Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon
#endif

[Run]

; Launch as the original (non-elevated) user — WebView2 refuses to render
; when the host process has admin privileges (shows black screen).
Filename: "{app}\{#MyAppExeName}"; \
  Description: "Launch {#MyAppName}"; \
  Flags: nowait postinstall skipifsilent runascurrentuser

; ── Upgrade detection ─────────────────────────────────────────────────────────
[Code]

function GetInstalledVersion(): String;
var
  RegKey: String;
  Ver:    String;
begin
  RegKey := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{E26DC47F-9F9F-4A69-8AD0-C5E17B2CE016}_is1';
  if not RegQueryStringValue(HKLM, RegKey, 'DisplayVersion', Ver) then
    if not RegQueryStringValue(HKCU, RegKey, 'DisplayVersion', Ver) then
      Ver := '';
  Result := Ver;
end;

function GetUninstallString(): String;
var
  RegKey:    String;
  UninstStr: String;
begin
  RegKey := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{E26DC47F-9F9F-4A69-8AD0-C5E17B2CE016}_is1';
  if not RegQueryStringValue(HKLM, RegKey, 'QuietUninstallString', UninstStr) then
    if not RegQueryStringValue(HKCU, RegKey, 'QuietUninstallString', UninstStr) then
      UninstStr := '';
  Result := UninstStr;
end;

function InitializeSetup(): Boolean;
var
  InstalledVer: String;
  NewVer:       String;
  Msg:          String;
  UninstStr:    String;
  ResultCode:   Integer;
  NL:           String;
begin
  Result := True;
  NL := #13#10;

  InstalledVer := GetInstalledVersion();
  if InstalledVer = '' then
    Exit;

  NewVer := '{#MyAppVersion}';

  if InstalledVer = NewVer then
    Msg := 'Version ' + InstalledVer + ' of {#MyAppName} is already installed.' + NL + NL +
           'Do you want to reinstall it?'
  else
    Msg := '{#MyAppName} is already installed.' + NL + NL +
           '  Installed version:  ' + InstalledVer + NL +
           '  New version:        ' + NewVer + NL + NL +
           'The old version will be removed before installing the new one.' + NL +
           'Your settings and data will be preserved.' + NL + NL +
           'Continue?';

  if MsgBox(Msg, mbConfirmation, MB_YESNO) = IDNO then
  begin
    Result := False;
    Exit;
  end;

  UninstStr := GetUninstallString();
  if UninstStr <> '' then
  begin
    Exec('>', UninstStr, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(500);
  end;
end;

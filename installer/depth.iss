; Inno Setup script for depth.
;
; There is a single, full offline setup: English on Cactus Whistle plus the Indonesian
; Whisper final/draft checkpoints and runtime. Build it with:
;   .toolchain\innosetup\ISCC.exe installer\depth.iss
;     -> dist\depth-setup.exe
;
; It installs machine-wide into Program Files. Note Inno Setup 7 always launches its loader
; unelevated (asInvoker manifest, so no UAC prompt and no shield icon at start) and only
; elevates partway through; where even temp creation is locked down that fails before ever
; prompting. For a UAC prompt at launch, ship the NSIS build instead (see README).
; Uninstalling while the app is still running shows a Retry/Cancel dialog asking the user to
; close it first (detected through the DepthRunning mutex the app holds).

#define AppName "depth"
#define AppVersion "0.1.1"
#define AppExe "depth.exe"
#define Publisher "Depth"

; What the user sees. Shortcuts, the wizard and Apps & features say "Depth"; the
; executable, the install folder and the AppId keep the lowercase program name, while each
; user's transcripts live in their Documents\Depth folder.
#define DisplayName "Depth"
#define IconFile "..\assets\icon\depth.ico"

[Setup]
; A stable AppId keeps upgrades and uninstalls tied to the same product entry.
AppId={{7C1E4B92-3A5D-4F68-9E02-1D4A6C8B5F31}
AppName={#DisplayName}
AppVersion={#AppVersion}
AppPublisher={#Publisher}
DefaultDirName={autopf}\depth
DefaultGroupName={#DisplayName}
DisableProgramGroupPage=yes
; Machine-wide install: Windows shows a UAC prompt, so the wizard runs as administrator.
; NOTE: kept for reference only — do not ship Inno builds. The bundled Inno 7.1.0 stamps
; Setup.exe asInvoker despite the directive below (verified against a minimal script), and
; re-stamping with mt.exe truncates the payload. Ship the NSIS build instead (see README).
PrivilegesRequired=admin
; The brand icon: the setup executable, the wizard pages and unins000.exe all carry it.
SetupIconFile={#IconFile}
OutputDir=..\dist
OutputBaseFilename=depth-setup
Compression=lzma2/normal
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#DisplayName}
UninstallDisplayIcon={app}\{#AppExe}
MinVersion=10.0
AllowNoIcons=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; \
    GroupDescription: "Shortcuts:"; Flags: checkedonce
Name: "startupicon"; Description: "Start automatically when I sign in (stays idle until the hotkey)"; \
    GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
; The engines are looked up relative to the executable, so these folders must keep their
; names: vendor\needle\windows-x86_64 for the Cactus runner, vendor\whisper for whisper.cpp.
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\sherpa-onnx-c-api.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\onnxruntime.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\onnxruntime_providers_shared.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\models\speaker-segmentation.int8.onnx"; DestDir: "{app}\models"; Flags: ignoreversion
Source: "..\models\nemo_en_titanet_small.onnx"; DestDir: "{app}\models"; Flags: ignoreversion
Source: "..\vendor\speakers\licenses\*"; DestDir: "{app}\vendor\speakers\licenses"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "..\models\whistle.cact"; DestDir: "{app}\models"; Flags: ignoreversion
Source: "..\vendor\needle\windows-x86_64\needle.exe"; DestDir: "{app}\vendor\needle\windows-x86_64"; \
    Flags: ignoreversion

; Indonesian: final and draft checkpoints plus the ggml runtime DLLs beside the CLI.
; The checkpoint is already quantised, so compressing it only wastes build time.
Source: "..\models\ggml-small-id-q8_0.bin"; DestDir: "{app}\models"; \
    Flags: ignoreversion nocompression
Source: "..\models\ggml-base-q5_1.bin"; DestDir: "{app}\models"; \
    Flags: ignoreversion nocompression
Source: "..\vendor\whisper\cpu\whisper-cli.exe"; DestDir: "{app}\vendor\whisper\cpu"; Flags: ignoreversion
Source: "..\vendor\whisper\cpu\*.dll"; DestDir: "{app}\vendor\whisper\cpu"; Flags: ignoreversion
Source: "..\vendor\whisper\cuda\whisper-cli.exe"; DestDir: "{app}\vendor\whisper\cuda"; Flags: ignoreversion
Source: "..\vendor\whisper\cuda\*.dll"; DestDir: "{app}\vendor\whisper\cuda"; Flags: ignoreversion
Source: "..\vendor\whisper\licenses\*"; DestDir: "{app}\vendor\whisper\licenses"; Flags: ignoreversion
Source: "..\models\licenses\*"; DestDir: "{app}\models\licenses"; Flags: ignoreversion

[Icons]
; IconFilename is spelled out so every shortcut carries the app's mark rather than the icon of
; whatever it points at (a folder, for the transcript shortcut).
Name: "{group}\{#DisplayName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; \
    IconFilename: "{app}\{#AppExe}"
Name: "{group}\Transcript folder"; Filename: "{userdocs}\Depth\transcripts"; \
    IconFilename: "{app}\{#AppExe}"
Name: "{group}\Uninstall {#DisplayName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#DisplayName}"; Filename: "{app}\{#AppExe}"; WorkingDir: "{app}"; \
    IconFilename: "{app}\{#AppExe}"; Tasks: desktopicon
Name: "{userstartup}\{#DisplayName}"; Filename: "{app}\{#AppExe}"; Parameters: "--background"; WorkingDir: "{app}"; \
    IconFilename: "{app}\{#AppExe}"; Tasks: startupicon

[Run]
Filename: "{app}\{#AppExe}"; \
    Description: "Start {#DisplayName} now (it sits in the tray, idle until you press the hotkey)"; \
    Flags: nowait postinstall skipifsilent

[InstallDelete]
; Pre-rename leftovers: the product used to install as transcribe-ai, into Program Files for
; the NSIS builds and %LocalAppData%\Programs for the per-user Inno builds. Removing those
; folders keeps an upgrade from orphaning ~200 MB plus dead shortcuts. The transcripts live
; in Documents and are migrated by the app itself, so they are never touched here.
Type: filesandordirs; Name: "{autopf}\transcribe-ai"
Type: filesandordirs; Name: "{localappdata}\Programs\transcribe-ai"
Type: files; Name: "{commondesktop}\Transcribe AI.lnk"
Type: files; Name: "{userdesktop}\Transcribe AI.lnk"
Type: files; Name: "{commonstartup}\Transcribe AI.lnk"
Type: files; Name: "{userstartup}\Transcribe AI.lnk"
Type: filesandordirs; Name: "{commonprograms}\Transcribe AI"
Type: filesandordirs; Name: "{commonprograms}\transcribe-ai"
Type: filesandordirs; Name: "{userprograms}\Transcribe AI"
Type: filesandordirs; Name: "{userprograms}\transcribe-ai"

[UninstallDelete]
; The app writes its config, log and transcripts under Documents; leave those alone so an
; uninstall never destroys transcripts. Only the installed copy is removed.
Type: filesandordirs; Name: "{app}\vendor"

[Registry]
; Drop the registry entries the pre-rename NSIS builds wrote under the old product id.
; The AppId above is unchanged, so this install's own Apps & features entry updates itself.
Root: HKLM64; Subkey: "Software\Microsoft\Windows\CurrentVersion\Uninstall\transcribe-ai"; Flags: deletekey
Root: HKLM64; Subkey: "Software\transcribe-ai"; Flags: deletekey

[Code]
// The app holds the DepthRunning mutex while it runs (see hold_running_mutex in
// src\main.rs). Uninstalling over a running instance would leave locked files behind, so the
// uninstaller asks the user to close it first, with Retry/Cancel until it is gone.
const
  SYNCHRONIZE = $100000;

function OpenMutex(dwDesiredAccess: DWORD; bInheritHandle: BOOL; lpName: string): THandle;
  external 'OpenMutexW@kernel32.dll stdcall';
function CloseHandle(hObject: THandle): BOOL;
  external 'CloseHandle@kernel32.dll stdcall';

function IsAppRunning(): Boolean;
var
  Handle: THandle;
begin
  Handle := OpenMutex(SYNCHRONIZE, False, 'DepthRunning');
  Result := Handle <> 0;
  if Result then
    CloseHandle(Handle);
end;

function InitializeUninstall(): Boolean;
begin
  Result := True;
  // Silent uninstalls have nobody to click Retry, so locked files fall through to Inno's
  // default handling (scheduled removal on reboot) instead of hanging.
  if UninstallSilent() then
    exit;
  while IsAppRunning() do
  begin
    if MsgBox('The app is still running. Please close the app.',
      mbError, MB_RETRYCANCEL) = IDCANCEL then
    begin
      Result := False;
      exit;
    end;
  end;
end;


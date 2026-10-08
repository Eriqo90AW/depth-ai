; NSIS script for depth.
;
; There is a single, full offline setup: English on Cactus Whistle plus the Indonesian
; Whisper final/draft checkpoints and runtime. Build it with (from the installer folder):
;   ..\.toolchain\nsis\makensis.exe depth.nsi
;     -> ..\dist\depth-setup.exe
;
; The result is a wizard that REQUESTS ELEVATION and installs machine-wide into Program Files,
; with shortcuts for every user and an entry in the 64-bit uninstall registry key.
; Uninstalling while the app is still running shows a Retry/Cancel dialog asking the user to
; close it first (detected through the DepthRunning mutex the app holds).
;
; Why machine-wide rather than per-user: once the wizard elevates, $LOCALAPPDATA, $DESKTOP and
; HKCU all resolve to the account that approved the UAC prompt. If that is a different
; administrator, a per-user install would land in *their* profile and the person who ran it would
; get nothing. An elevated installer therefore has to target the all-users locations.
;
; NSIS is licensed under the zlib licence, so there is no commercial-use restriction here.

Unicode true
!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"

!define APPNAME "depth"
!define APPEXE "depth.exe"
!define APPVERSION "0.1.1"
!define PUBLISHER "Depth"
!define UNINSTKEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APPNAME}"

; What the user sees. Shortcuts, the wizard, the tray tooltip and Apps & features all say
; "Depth"; the executable, the install folder and the registry key keep the lowercase program
; name, while each user's transcripts live in their Documents\Depth folder.
!define DISPLAYNAME "Depth"
!define ICONFILE "..\assets\icon\depth.ico"

Name "${DISPLAYNAME} ${APPVERSION}"
OutFile "..\dist\depth-setup.exe"

; Machine-wide install, so a UAC prompt appears on launch.
InstallDir "$PROGRAMFILES64\depth"
InstallDirRegKey HKLM "Software\${APPNAME}" "InstallDir"
RequestExecutionLevel admin

; The 181 MB checkpoint is already quantised, so zlib keeps the build quick without losing
; meaningful size over LZMA.
SetCompressor zlib

VIProductVersion "0.1.1.0"
VIAddVersionKey "ProductName" "${DISPLAYNAME}"
VIAddVersionKey "FileDescription" "${DISPLAYNAME} Setup"
VIAddVersionKey "FileVersion" "${APPVERSION}"
VIAddVersionKey "ProductVersion" "${APPVERSION}"
VIAddVersionKey "CompanyName" "${PUBLISHER}"
VIAddVersionKey "LegalCopyright" "Apache-2.0"

; The brand icon, for the setup executable, the wizard pages and the uninstaller. MUI reads
; these when the pages are inserted below, so they have to be defined first.
!define MUI_ICON "${ICONFILE}"
!define MUI_UNICON "${ICONFILE}"

!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TITLE "Install ${DISPLAYNAME}"
!define MUI_WELCOMEPAGE_TEXT "This installs ${DISPLAYNAME}, a tray app that transcribes the audio your PC plays — not your microphone.$\r$\n$\r$\nIt installs for all users, so Windows will ask for administrator permission when you click Next.$\r$\n$\r$\nIt records nothing until you press the hotkey (Ctrl+Alt+Space by default), and everything stays on this machine."

!define MUI_FINISHPAGE_RUN "$INSTDIR\${APPEXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${DISPLAYNAME} now (idle until you press the hotkey)"
!define MUI_FINISHPAGE_TEXT "${DISPLAYNAME} will appear as a tray icon and a small status pill at the corner of the screen.$\r$\n$\r$\nPlay something, then press Ctrl+Alt+Space to start. Each user's transcripts land in their own Documents\Depth\transcripts."

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; ---------------------------------------------------------------- components

Section "Application, English transcription and Indonesian transcription with live drafts" SEC_CORE
  SectionIn RO ; always installed
  SetOutPath "$INSTDIR"
  File "..\target\release\${APPEXE}"
  File "..\README.md"
  ; Layout matters: the app resolves its engines relative to the executable.
  SetOutPath "$INSTDIR\models"
  File "..\models\whistle.cact"
  SetOutPath "$INSTDIR\vendor\needle\windows-x86_64"
  File "..\vendor\needle\windows-x86_64\needle.exe"
  ; Indonesian: final and draft checkpoints plus the ggml runtime DLLs beside the CLI.
  SetOutPath "$INSTDIR\models"
  File "..\models\ggml-small-q5_1.bin"
  File "..\models\ggml-base-q5_1.bin"
  SetOutPath "$INSTDIR\vendor\whisper"
  File "..\vendor\whisper\Release\whisper-cli.exe"
  File "..\vendor\whisper\Release\*.dll"
SectionEnd

Section "Desktop shortcut (all users)" SEC_DESKTOP
  CreateShortCut "$DESKTOP\${DISPLAYNAME}.lnk" "$INSTDIR\${APPEXE}" "" "$INSTDIR\${APPEXE}" 0 \
    SW_SHOWNORMAL "" "Transcribe the audio your PC plays"
SectionEnd

Section "Start Menu shortcuts (all users)" SEC_STARTMENU
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortCut "$SMPROGRAMS\${APPNAME}\${DISPLAYNAME}.lnk" "$INSTDIR\${APPEXE}" "" "$INSTDIR\${APPEXE}"
  ; The folder shortcut carries the app icon too, so the group reads as one product.
  CreateShortCut "$SMPROGRAMS\${APPNAME}\Transcript folder.lnk" "$DOCUMENTS\Depth\transcripts" "" "$INSTDIR\${APPEXE}" 0
  CreateShortCut "$SMPROGRAMS\${APPNAME}\Uninstall ${DISPLAYNAME}.lnk" "$INSTDIR\uninstall.exe"
SectionEnd

Section "Start with Windows for all users (stays idle until the hotkey)" SEC_STARTUP
  ; $SMSTARTUP is the all-users Startup folder, matching SetShellVarContext all in .onInit.
  CreateShortCut "$SMSTARTUP\${DISPLAYNAME}.lnk" "$INSTDIR\${APPEXE}" "--background" "$INSTDIR\${APPEXE}"
SectionEnd

; Removes pre-rename installs so an upgrade does not orphan ~200 MB and dead shortcuts.
; Runs before anything else. $LOCALAPPDATA here is the elevating administrator's profile, so a
; per-user install belonging to a different account is left for its own uninstaller.
Section -LegacyCleanup
  RMDir /r "$PROGRAMFILES64\transcribe-ai"
  RMDir /r "$LOCALAPPDATA\Programs\transcribe-ai"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\transcribe-ai"
  DeleteRegKey HKLM "Software\transcribe-ai"
  Delete "$DESKTOP\Transcribe AI.lnk"
  Delete "$SMSTARTUP\Transcribe AI.lnk"
  Delete "$SMPROGRAMS\Transcribe AI\Transcribe AI.lnk"
  Delete "$SMPROGRAMS\Transcribe AI\Transcript folder.lnk"
  Delete "$SMPROGRAMS\Transcribe AI\Uninstall Transcribe AI.lnk"
  RMDir "$SMPROGRAMS\Transcribe AI"
  Delete "$SMPROGRAMS\transcribe-ai\Transcribe AI.lnk"
  Delete "$SMPROGRAMS\transcribe-ai\Transcript folder.lnk"
  Delete "$SMPROGRAMS\transcribe-ai\Uninstall Transcribe AI.lnk"
  RMDir "$SMPROGRAMS\transcribe-ai"
SectionEnd

; Always runs, after the visible components.

Section -Registration
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\${APPNAME}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayName" "${DISPLAYNAME}"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayVersion" "${APPVERSION}"
  WriteRegStr HKLM "${UNINSTKEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKLM "${UNINSTKEY}" "DisplayIcon" "$INSTDIR\${APPEXE}"
  WriteRegStr HKLM "${UNINSTKEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "${UNINSTKEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "${UNINSTKEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTKEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKLM "${UNINSTKEY}" "EstimatedSize" $0
SectionEnd

!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_CORE} "The application, the Cactus engine with the Whistle model for English, and Whisper small plus base for Indonesian final results and live drafts."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_DESKTOP} "Put a shortcut on every user's desktop."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_STARTMENU} "Add entries to the all-users Start Menu."
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_STARTUP} "Launch when any user signs in. It starts idle and records nothing until the hotkey is pressed."
!insertmacro MUI_FUNCTION_DESCRIPTION_END

; ---------------------------------------------------------------- uninstall

Section Uninstall
  ; The app holds the DepthRunning mutex while it runs (see hold_running_mutex in
  ; src\main.rs). Deleting files from under a running instance leaves locked files behind,
  ; so ask the user to close it first.
  Call un.CheckAppRunning

  Delete "$DESKTOP\${DISPLAYNAME}.lnk"
  Delete "$SMSTARTUP\${DISPLAYNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\${DISPLAYNAME}.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\Transcript folder.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\Uninstall ${DISPLAYNAME}.lnk"
  ; Shortcuts from before the rename to Depth, so an upgrade over an older install does not
  ; leave dead entries behind.
  Delete "$DESKTOP\Transcribe AI.lnk"
  Delete "$SMSTARTUP\Transcribe AI.lnk"
  Delete "$SMPROGRAMS\Transcribe AI\Transcribe AI.lnk"
  Delete "$SMPROGRAMS\Transcribe AI\Transcript folder.lnk"
  Delete "$SMPROGRAMS\Transcribe AI\Uninstall Transcribe AI.lnk"
  RMDir "$SMPROGRAMS\Transcribe AI"
  Delete "$SMPROGRAMS\transcribe-ai\Transcribe AI.lnk"
  Delete "$SMPROGRAMS\transcribe-ai\Transcript folder.lnk"
  Delete "$SMPROGRAMS\transcribe-ai\Uninstall Transcribe AI.lnk"
  RMDir "$SMPROGRAMS\transcribe-ai"
  RMDir "$SMPROGRAMS\${APPNAME}"

  ; Remove the installed copy only. Each user's config, logs and transcripts live in their own
  ; Documents\Depth and are deliberately left alone.
  RMDir /r "$INSTDIR"

  DeleteRegKey HKLM "${UNINSTKEY}"
  DeleteRegKey HKLM "Software\${APPNAME}"

  IfFileExists "$DOCUMENTS\Depth\*.*" 0 +2
    MessageBox MB_OK "Your transcripts and settings were left in:$\r$\n$DOCUMENTS\Depth"
SectionEnd

Function .onInit
  ; Machine-wide: shortcuts and registry entries target the all-users locations, and the file
  ; system redirector is switched off so this 32-bit stub writes to the real Program Files
  ; rather than being silently redirected to Program Files (x86).
  SetShellVarContext all
  ; Write the 64-bit registry view so the app is listed under Apps & features rather than
  ; hidden in WOW6432Node.
  SetRegView 64
  ${If} ${RunningX64}
    ${DisableX64FSRedirection}
  ${EndIf}
FunctionEnd

Function un.onInit
  SetShellVarContext all
  SetRegView 64
  ${If} ${RunningX64}
    ${DisableX64FSRedirection}
  ${EndIf}
FunctionEnd

; Retry/Cancel loop until the app's running mutex is gone. A silent uninstall has nobody to
; click Retry, so it aborts instead of hanging.
Function un.CheckAppRunning
  check_running:
    ; SYNCHRONIZE access is enough to test for the mutex's existence.
    System::Call 'kernel32::OpenMutexW(i 0x100000, i 0, w "DepthRunning") i .r0'
    ${If} $0 != 0
      System::Call 'kernel32::CloseHandle(i $0)'
      IfSilent 0 +2
        Quit
      MessageBox MB_RETRYCANCEL "The app is still running. Please close the app." IDRETRY check_running
      Quit
    ${EndIf}
FunctionEnd


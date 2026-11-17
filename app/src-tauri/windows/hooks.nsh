; Installer hooks for Backsight (Tauri 2 `installerHooks`). Included before the sections in installer.nsi.

; Refuses while the old Clipwise runs. installer.nsi uses it next to BacksightRequireQuit, before any
; file is touched.
!macro BacksightRequireClipwiseQuit
  Push $0
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Clipwise" "UninstallString"
  ${If} $0 != ""
    nsis_tauri_utils::FindProcessCurrentUser "clipwise.exe"
    Pop $0
    ; 1 means not running, as in BacksightRequireQuit.
    ${If} $0 != 1
      MessageBox MB_OK|MB_ICONEXCLAMATION "Clipwise (Backsight's old name) is still open. Right-click its icon near the clock, choose Quit Clipwise, then run this installer again." /SD IDOK
      DetailPrint "Clipwise (Backsight's old name) is still open: quit it, then install again."
      SetErrorLevel 1
      Quit
    ${EndIf}
  ${EndIf}
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  ; Backsight used to be called Clipwise. Remove that copy so there aren't two. Its uninstaller
  ; deletes only program files, shortcuts and its own startup entry, never settings or clips
  ; (Backsight copies the settings over on its first start).
  ReadRegStr $R0 HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Clipwise" "UninstallString"
  ${If} $R0 != ""
    ; The folder comes from UninstallString ("<folder>\uninstall.exe", quoted).
    StrCpy $R1 $R0
    StrCpy $R2 $R1 1
    ${If} $R2 == '"'
      StrCpy $R1 $R1 -1 1
    ${EndIf}
    ${GetParent} $R1 $R1
    ; Only the old installer's own folder, so a stale or odd registry value never runs some other
    ; program. (It can't stop a program already running as this user; nothing here could.)
    ${If} $R1 == "$LOCALAPPDATA\Clipwise"
    ${AndIf} ${FileExists} "$R1\uninstall.exe"
      ; The old uninstaller deletes its "Start with Windows" entry, so move it here first.
      ; Same text as autostart.rs: the quoted exe path plus --minimized.
      ReadRegStr $R2 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Clipwise"
      ${If} $R2 != ""
        WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "Backsight" '"$INSTDIR\${MAINBINARYNAME}.exe" --minimized'
      ${EndIf}
      StrCpy $R0 '"$R1\uninstall.exe"'
      ClearErrors
      ; _?= runs the uninstaller in place so ExecWait really waits for it.
      ExecWait '$R0 /S _?=$R1' $R2
      DetailPrint "Old Clipwise copy removed (exit code $R2)"
      ; In place, the uninstaller can't delete itself or its folder. Keep it when it failed, so
      ; Clipwise can still be removed from Windows Settings.
      ${IfNot} ${Errors}
      ${AndIf} $R2 = 0
        Delete "$R1\uninstall.exe"
        RMDir "$R1"
      ${EndIf}
    ${Else}
      DetailPrint "An old Clipwise copy is installed elsewhere: remove it in Windows Settings > Apps."
    ${EndIf}
  ${EndIf}
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; "Backsight problem report" in the Start menu: runs backsight.exe --report, which works even when
  ; the main window is frozen.
  CreateShortcut "$SMPROGRAMS\Backsight problem report.lnk" "$INSTDIR\${MAINBINARYNAME}.exe" "--report"

  ; A short install log next to the app's own logs, so a problem report shows how the install went.
  ; The WebView2 runtime is the usual thing that goes wrong on a fresh Windows 10.
  ${If} ${RunningX64}
    ReadRegStr $R8 HKLM "SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${Else}
    ReadRegStr $R8 HKLM "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}
  ${If} $R8 == ""
    ReadRegStr $R8 HKCU "SOFTWARE\Microsoft\EdgeUpdate\Clients\${WEBVIEW2APPGUID}" "pv"
  ${EndIf}
  ${If} $R8 == ""
    StrCpy $R8 "not found"
  ${EndIf}
  CreateDirectory "$APPDATA\Backsight\logs"
  FileOpen $R9 "$APPDATA\Backsight\logs\install.log" w
  FileWrite $R9 "Backsight ${VERSION} installed to $INSTDIR$\r$\n"
  FileWrite $R9 "WebView2 runtime after install: $R8$\r$\n"
  FileClose $R9
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; An update keeps its shortcuts; an uninstall removes this one too.
  ${If} $UpdateMode <> 1
    Delete "$SMPROGRAMS\Backsight problem report.lnk"
  ${EndIf}
!macroend

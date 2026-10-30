; NSIS hooks for the cc-usage installer (bundle > windows > nsis > installerHooks).
;
; Installing registers cc-hook in Claude Code's settings.json (in
; %CLAUDE_CONFIG_DIR% when that is set, else in %USERPROFILE%\.claude, the
; folder Claude Code itself uses), so the widget gets fine-grained session
; states without any manual step. Uninstalling removes exactly those entries
; again, from the file the same rule finds then. `cc-hook install` appends after other
; tools' hooks, keeps a timestamped backup and is idempotent, so running it on
; every install or update only rewrites the command path when it changed.
;
; The same `install` also chains cc-hook in front of the user's status line:
; statusLine.command becomes `"<INSTDIR>/cc-hook.exe" statusline`, which writes
; each session's exact context use to %LOCALAPPDATA%\cc-usage\live\ and then
; runs the original command (recorded in statusline-chain.json), whose output is
; unchanged. `uninstall` puts the original statusLine back, unless the user
; changed it in the meantime. Updates (/UPDATE) keep both.
;
; Sessions that were already open keep reporting busy/idle only until they are
; restarted: Claude Code reads its hooks when a session starts.

!macro NSIS_HOOK_POSTINSTALL
  ; Only when Claude Code has its folder: never create one for it. The folder
  ; is the one cc-hook picks: CLAUDE_CONFIG_DIR when set (cc-hook inherits this
  ; installer's environment), else %USERPROFILE%\.claude. $1 is saved around
  ; the check so the installer template's own use of it is not disturbed.
  Push $1
  ReadEnvStr $1 "CLAUDE_CONFIG_DIR"
  ${If} $1 == ""
    StrCpy $1 "$PROFILE\.claude"
  ${EndIf}
  ${If} ${FileExists} "$1\*.*"
    DetailPrint "Registering the Claude Code hooks in $1\settings.json"
    nsExec::ExecToLog '"$INSTDIR\cc-hook.exe" install'
    Pop $0
    ${If} $0 <> 0
      DetailPrint "Could not register the hooks (code $0). The panel still works, with less detail."
    ${EndIf}
  ${EndIf}
  Pop $1
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ; An update runs the old uninstaller with /UPDATE before installing the new
  ; version; keep the hooks then, the new cc-hook.exe lands at the same path.
  ${If} $UpdateMode <> 1
  ${AndIf} ${FileExists} "$INSTDIR\cc-hook.exe"
    DetailPrint "Removing the Claude Code hooks"
    nsExec::ExecToLog '"$INSTDIR\cc-hook.exe" uninstall'
    Pop $0
  ${EndIf}
  ; "Start with Windows" (the tray menu) is the app's own value under the Run
  ; key, plus Task Manager's note on it when there is one. Left behind, Windows
  ; would try to start an exe that is gone at every sign-in. Only those two
  ; values; the keys stay. An update keeps them: the exe lands at the same path.
  ${If} $UpdateMode <> 1
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "cc-usage"
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run" "cc-usage"
  ${EndIf}
!macroend

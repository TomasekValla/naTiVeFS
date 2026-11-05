; naTiVeFS installer. makensis -DVERSION=0.9.0 -DSRC=<folder with the exes> -DOUT=<setup.exe> nativefs.nsi
; Starts without admin rights; "For all users" restarts it elevated. Silent: /S, /ALLUSERS for Program Files,
; /UPDATE waits for the running app and starts the new version; /HIDDEN starts it in the tray.
Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user

!include MUI2.nsh
!include nsDialogs.nsh
!include LogicLib.nsh
!include FileFunc.nsh

!define APP "naTiVeFS"
!define UNINST "Software\Microsoft\Windows\CurrentVersion\Uninstall\${APP}"

Name "${APP}"
OutFile "${OUT}"
BrandingText "${APP} ${VERSION}"
VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Tomas Valla, GPL-3.0-or-later"

Var AllUsers
Var Update
Var Hidden
Var ModeUser
Var ModeAll

!define MUI_ICON "..\..\assets\app.ico"
!define MUI_UNICON "..\..\assets\app.ico"
!define MUI_FINISHPAGE_RUN
!define MUI_FINISHPAGE_RUN_FUNCTION StartApp

Page custom ModePage ModeLeave
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_COMPONENTS
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Function SetDefaultDir
  ${If} $AllUsers == 1
    StrCpy $INSTDIR "$PROGRAMFILES64\${APP}"
  ${Else}
    StrCpy $INSTDIR "$LOCALAPPDATA\Programs\${APP}"
  ${EndIf}
FunctionEnd

Function .onInit
  SetRegView 64
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/ALLUSERS" $1
  ${IfNot} ${Errors}
    StrCpy $AllUsers 1
    SetShellVarContext all
  ${EndIf}
  ClearErrors
  ${GetOptions} $0 "/UPDATE" $1
  ${IfNot} ${Errors}
    StrCpy $Update 1
  ${EndIf}
  ClearErrors
  ${GetOptions} $0 "/HIDDEN" $1
  ${IfNot} ${Errors}
    StrCpy $Hidden 1
  ${EndIf}
  Call SetDefaultDir
FunctionEnd

Function ModePage
  ${If} $AllUsers == 1
    Abort
  ${EndIf}
  !insertmacro MUI_HEADER_TEXT "Choose who gets ${APP}" "Installing just for you needs no administrator rights."
  nsDialogs::Create 1018
  Pop $0
  ${NSD_CreateRadioButton} 0 10u 100% 12u "Just for me (recommended)"
  Pop $ModeUser
  ${NSD_CreateRadioButton} 0 28u 100% 12u "For all users of this computer (needs administrator rights)"
  Pop $ModeAll
  ${NSD_Check} $ModeUser
  nsDialogs::Show
FunctionEnd

Function ModeLeave
  ${NSD_GetState} $ModeAll $0
  ${If} $0 == ${BST_CHECKED}
    ClearErrors
    ExecShell "runas" "$EXEPATH" "/ALLUSERS"
    ${If} ${Errors}
      MessageBox MB_OK|MB_ICONEXCLAMATION "Administrator rights are needed to install for all users."
      Abort
    ${EndIf}
    Quit
  ${EndIf}
FunctionEnd

Function StartApp
  ; a silent update back to the tray (per user, so never elevated)
  ${If} $Hidden == 1
    Exec '"$INSTDIR\naTiVeFS.exe" --minimized'
    Return
  ${EndIf}
  ; through Explorer: an elevated installer must not start the app elevated
  Exec '"$WINDIR\explorer.exe" "$INSTDIR\naTiVeFS.exe"'
FunctionEnd

Section "${APP}" SecApp
  SectionIn RO
  ; a running copy holds its files
  ${If} ${FileExists} "$INSTDIR\naTiVeFS.exe"
    ExecWait '"$INSTDIR\naTiVeFS.exe" --quit'
    Sleep 1500
  ${EndIf}
  SetOutPath "$INSTDIR"
  File /r "${SRC}\*.*"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  CreateShortcut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\naTiVeFS.exe"
  ${If} $AllUsers == 1
    StrCpy $0 "$\"$INSTDIR\uninstall.exe$\" /ALLUSERS"
  ${Else}
    StrCpy $0 "$\"$INSTDIR\uninstall.exe$\""
  ${EndIf}
  WriteRegStr SHCTX "${UNINST}" "DisplayName" "${APP}"
  WriteRegStr SHCTX "${UNINST}" "DisplayVersion" "${VERSION}"
  WriteRegStr SHCTX "${UNINST}" "Publisher" "Tomas Valla"
  WriteRegStr SHCTX "${UNINST}" "DisplayIcon" "$INSTDIR\naTiVeFS.exe"
  WriteRegStr SHCTX "${UNINST}" "InstallLocation" "$INSTDIR"
  WriteRegStr SHCTX "${UNINST}" "UninstallString" "$0"
  WriteRegDWORD SHCTX "${UNINST}" "NoModify" 1
  WriteRegDWORD SHCTX "${UNINST}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $1 $2 $3
  IntFmt $1 "0x%08X" $1
  WriteRegDWORD SHCTX "${UNINST}" "EstimatedSize" $1
  ${If} $Update == 1
    Call StartApp
  ${EndIf}
SectionEnd

Section /o "Desktop shortcut" SecDesktop
  CreateShortcut "$DESKTOP\${APP}.lnk" "$INSTDIR\naTiVeFS.exe"
SectionEnd

Section /o "Start with Windows" SecAutostart
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "${APP}" '"$INSTDIR\naTiVeFS.exe" --autostart'
SectionEnd

Function un.onInit
  SetRegView 64
  ${GetParameters} $0
  ClearErrors
  ${GetOptions} $0 "/ALLUSERS" $1
  ${IfNot} ${Errors}
    StrCpy $AllUsers 1
    SetShellVarContext all
    UserInfo::GetAccountType
    Pop $1
    ${If} $1 != "Admin"
      ExecShell "runas" "$EXEPATH" "/ALLUSERS _?=$INSTDIR"
      Quit
    ${EndIf}
  ${EndIf}
FunctionEnd

Section "Uninstall"
  ; quits a running copy and removes this user's autostart, browser registration and file manager menus
  ExecWait '"$INSTDIR\naTiVeFS.exe" --cleanup'
  Sleep 800
  Delete "$INSTDIR\naTiVeFS.exe"
  Delete "$INSTDIR\naTiVeFS-link.exe"
  Delete "$INSTDIR\libmpv-2.dll"
  Delete "$INSTDIR\LICENSE"
  Delete "$INSTDIR\THIRD-PARTY.txt"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey SHCTX "${UNINST}"
SectionEnd

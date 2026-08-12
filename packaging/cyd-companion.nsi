; CYD Companion — simple Windows setup wizard (NSIS)
!include "MUI2.nsh"

Name "CYD Companion"
OutFile "..\dist\CYD-Companion-Setup.exe"
Unicode true
RequestExecutionLevel admin
InstallDir "$PROGRAMFILES64\CYD Companion"
InstallDirRegKey HKLM "Software\CYDCompanion" "Install_Dir"

!define MUI_ABORTWARNING
!define MUI_ICON "${NSISDIR}\Contrib\Graphics\Icons\modern-install.ico"
!define MUI_UNICON "${NSISDIR}\Contrib\Graphics\Icons\modern-uninstall.ico"
!define MUI_WELCOMEPAGE_TITLE "CYD Companion Setup"
!define MUI_WELCOMEPAGE_TEXT "Install the Windows app that configures your ESP32-2432S028 scrypt miner over USB.$\r$\n$\r$\nAfter install: plug USB → Connect → Setup wizard → Save & reboot."
!define MUI_FINISHPAGE_RUN "$INSTDIR\cyd-companion.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Launch CYD Companion"
!define MUI_FINISHPAGE_SHOWREADME "$INSTDIR\README.txt"
!define MUI_FINISHPAGE_SHOWREADME_TEXT "Open quick-start notes"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "CYD Companion (required)" SecMain
  SectionIn RO
  SetOutPath $INSTDIR
  File "..\dist\cyd-companion-windows\cyd-companion.exe"
  File "..\dist\cyd-companion-windows\README.txt"
  File "..\dist\cyd-companion-windows\COMPANION.md"

  WriteRegStr HKLM "Software\CYDCompanion" "Install_Dir" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\CYDCompanion" "DisplayName" "CYD Companion"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\CYDCompanion" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\CYDCompanion" "Publisher" "GutFarms"
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\CYDCompanion" "NoModify" 1
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\CYDCompanion" "NoRepair" 1
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\CYD Companion"
  CreateShortCut "$SMPROGRAMS\CYD Companion\CYD Companion.lnk" "$INSTDIR\cyd-companion.exe"
  CreateShortCut "$SMPROGRAMS\CYD Companion\Uninstall.lnk" "$INSTDIR\Uninstall.exe"
  CreateShortCut "$DESKTOP\CYD Companion.lnk" "$INSTDIR\cyd-companion.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\cyd-companion.exe"
  Delete "$INSTDIR\README.txt"
  Delete "$INSTDIR\COMPANION.md"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\CYD Companion\CYD Companion.lnk"
  Delete "$SMPROGRAMS\CYD Companion\Uninstall.lnk"
  RMDir "$SMPROGRAMS\CYD Companion"
  Delete "$DESKTOP\CYD Companion.lnk"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\CYDCompanion"
  DeleteRegKey HKLM "Software\CYDCompanion"
SectionEnd

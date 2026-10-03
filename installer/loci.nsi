Unicode True
!include "MUI2.nsh"
!include "LogicLib.nsh"
!include "x64.nsh"
!ifndef LOCI_VERSION
  !error "Use scripts/build-installer.ps1 to build the installer."
!endif
Name "Loci"
OutFile "${LOCI_OUTPUT}"
InstallDir "$PROGRAMFILES64\Loci"
RequestExecutionLevel admin
SetCompressor /SOLID lzma
!define MUI_ABORTWARNING
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "SimpChinese"

!macro LociInstallerMutex
  System::Call 'kernel32::CreateMutexW(p0, i0, w "Global\Loci.Installer") p.r0 ?e'
  Pop $1
  ${If} $0 == 0
  ${OrIf} $1 == 183
    MessageBox MB_ICONSTOP|MB_OK "另一个 Loci 安装或卸载程序正在运行。请等待它完成后重试。"
    SetErrorLevel 1
    Abort
  ${EndIf}
  ; Keep the handle open until this installer process exits.
!macroend

Function .onInit
  !insertmacro LociInstallerMutex
  ${IfNot} ${RunningX64}
    MessageBox MB_ICONSTOP|MB_OK "Loci 需要 64 位 Windows。"
    SetErrorLevel 1
    Abort
  ${EndIf}
  SetRegView 64
  ReadRegStr $0 HKLM "Software\Loci" "InstallLocation"
  ${If} $0 != ""
  ${AndIf} $0 != "$PROGRAMFILES64\Loci"
    MessageBox MB_ICONSTOP|MB_OK "旧 Loci 安装目录不是受保护的默认目录。请先卸载旧版本。"
    SetErrorLevel 1
    Abort
  ${EndIf}
  StrCpy $INSTDIR "$PROGRAMFILES64\Loci"
FunctionEnd

Section "Loci 文件索引服务"
  SectionIn RO
  ; Also ignore NSIS /D command-line overrides of the installation directory.
  StrCpy $INSTDIR "$PROGRAMFILES64\Loci"
  SetRegView 64
  ReadRegStr $0 HKLM "Software\Loci" "InstallLocation"
  ${If} $0 != ""
  ${AndIf} $0 != $INSTDIR
    MessageBox MB_ICONSTOP|MB_OK "Loci 已安装在 $0。请在原目录升级，或先卸载旧版本。"
    SetErrorLevel 1
    Abort
  ${EndIf}
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=install-service.ps1 "${LOCI_PAYLOAD}\install-service.ps1"
  ; Ownership and process exit are checked before replacing any service files.
  StrCpy $0 1
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$PLUGINSDIR\install-service.ps1" -ExecutablePath "$INSTDIR\loci-service.exe" -StopOnly' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "无法安全停止 Loci 服务（退出码 $0）。服务可能属于另一个安装目录，或停止超时。未覆盖服务文件。"
    SetErrorLevel 1
    Abort
  ${EndIf}
  SetOutPath "$INSTDIR"
  File "${LOCI_PAYLOAD}\loci-service.exe"
  File "${LOCI_PAYLOAD}\loci.exe"
  File "${LOCI_PAYLOAD}\install-service.ps1"
  File "${LOCI_PAYLOAD}\README.md"
  File "${LOCI_PAYLOAD}\THIRD_PARTY.md"
  SetOutPath "$INSTDIR\licenses"
  File /r "${LOCI_PAYLOAD}\licenses\*"
  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKLM "Software\Loci" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Loci" "DisplayName" "Loci"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Loci" "DisplayVersion" "${LOCI_VERSION}"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Loci" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Loci" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Loci" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Loci" "NoModify" 1
  WriteRegDWORD HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Loci" "NoRepair" 1
  StrCpy $0 1
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\install-service.ps1" -ExecutablePath "$INSTDIR\loci-service.exe"' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "Loci 文件已安装，但服务未能启动（退出码 $0）。可重新运行安装器，或从 Windows 应用列表卸载。"
    SetErrorLevel 1
    Abort
  ${EndIf}
  DetailPrint "LociIndex 服务已启动。首次索引期间查询可能显示准备状态。"
SectionEnd

Function un.onInit
  !insertmacro LociInstallerMutex
  SetRegView 64
  ${If} $INSTDIR != "$PROGRAMFILES64\Loci"
    MessageBox MB_ICONSTOP|MB_OK "Loci 卸载器必须从受保护的默认安装目录运行。"
    SetErrorLevel 1
    Abort
  ${EndIf}
FunctionEnd

Section "Uninstall"
  SetRegView 64
  StrCpy $0 1
  ExecWait '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -ExecutionPolicy Bypass -File "$INSTDIR\install-service.ps1" -ExecutablePath "$INSTDIR\loci-service.exe" -Uninstall' $0
  ${If} $0 != 0
    MessageBox MB_ICONSTOP|MB_OK "Loci 服务未能安全停止或删除（退出码 $0），安装文件已保留。请处理服务错误后重新卸载。"
    SetErrorLevel 1
    Abort
  ${EndIf}
  Delete "$INSTDIR\loci-service.exe"
  Delete "$INSTDIR\loci.exe"
  Delete "$INSTDIR\install-service.ps1"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\THIRD_PARTY.md"
  RMDir /r "$INSTDIR\licenses"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegKey HKLM "Software\Microsoft\Windows\CurrentVersion\Uninstall\Loci"
  DeleteRegKey HKLM "Software\Loci"
  ; Preserve ProgramData\Loci checkpoints; never delete user index data.
SectionEnd

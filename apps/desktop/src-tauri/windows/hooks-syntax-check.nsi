; hooks.nsh 的语法检查 harness（由 scripts/check-nsis-hooks.sh 驱动），
; 不参与打包——真实安装器由 tauri-bundler 自己的 installer.nsi 生成。
;
; 刻意复刻 installer.nsi 的两处上下文，否则检查不出真正会咬人的问题：
; - hooks 在 INSTALLMODE 定义「之前」include（模板里是 34 行 vs 45 行），
;   宏体内的 !if "${INSTALLMODE}" 依赖「插入时才展开」才能选对分支；
; - 同一个宏在 Section Install 与 Section Uninstall 各插一次，
;   手写标签在重复插入下会撞名（本文件用 LogicLib 规避）。
Unicode true
!addplugindir "${PLUGINDIR}"

!include MUI2.nsh
!include FileFunc.nsh
!include x64.nsh
!include WordFunc.nsh

!include "hooks.nsh"

!ifndef INSTALLMODE
  !define INSTALLMODE "currentUser"
!endif

Name "hooks-syntax-check"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\MCPane"

!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section Install
  SetOutPath $INSTDIR
  !insertmacro NSIS_HOOK_PREINSTALL
  !insertmacro NSIS_HOOK_POSTINSTALL
SectionEnd

Section Uninstall
  !insertmacro NSIS_HOOK_PREUNINSTALL
  !insertmacro NSIS_HOOK_POSTUNINSTALL
SectionEnd

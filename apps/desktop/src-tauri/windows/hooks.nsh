; MCPane NSIS 安装钩子：拷贝文件前释放 sidecar mcpane-hub.exe 的占用。
;
; Tauri 模板只在 File 拷贝前终止主程序（CheckIfAppIsRunning MAINBINARYNAME），
; sidecar 无人管。而应用内更新走 tauri-plugin-updater，Windows 分支是
; ShellExecuteW 拉起安装器后 std::process::exit(0) 直退——RunEvent::Exit 不触发，
; 桌面端的优雅关停整条路径被跳过；被 adopt 的外部 hub 更是从来不归它管。
; 只要有一个 mcpane-hub.exe 活着，NSIS 写文件就报
; "Error opening file for writing: ...\mcpane-hub.exe"，安装中断。
;
; 这里无条件杀干净；万一杀不掉（句柄未释放等），把旧文件改名让出路径——
; Windows 允许改名运行中的 exe（映像句柄带 FILE_SHARE_DELETE）——安装不中断。

!define MCPANE_HUB_EXE "mcpane-hub.exe"

!macro MCPaneReleaseHub
  DetailPrint "正在停止 MCPane Hub 后台进程…"

  ; 0) 先清上一轮改名兜底留下的残留。卸载路径必须赶在模板的
  ;    RMDir "$INSTDIR" 之前清掉——那是非递归删除，目录非空就留下来了。
  Delete "$INSTDIR\${MCPANE_HUB_EXE}.old"

  ; 1) 杀进程并确认真的没了（TerminateProcess 是异步的）。
  StrCpy $R5 0
  ${Do}
    !if "${INSTALLMODE}" == "currentUser"
      nsis_tauri_utils::FindProcessCurrentUser "${MCPANE_HUB_EXE}"
    !else
      nsis_tauri_utils::FindProcess "${MCPANE_HUB_EXE}"
    !endif
    Pop $R6
    ${If} $R6 <> 0
      ${ExitDo}  ; 非 0 = 没找到，已干净
    ${EndIf}
    !if "${INSTALLMODE}" == "currentUser"
      nsis_tauri_utils::KillProcessCurrentUser "${MCPANE_HUB_EXE}"
    !else
      nsis_tauri_utils::KillProcess "${MCPANE_HUB_EXE}"
    !endif
    Pop $R6
    Sleep 200
    IntOp $R5 $R5 + 1
    ${If} $R5 >= 10
      ${ExitDo}
    ${EndIf}
  ${Loop}

  ; 2) 进程没了不等于句柄立刻释放：以「能否写打开」为准绳复核。
  ${If} ${FileExists} "$INSTDIR\${MCPANE_HUB_EXE}"
    StrCpy $R5 0
    ${Do}
      ClearErrors
      FileOpen $R6 "$INSTDIR\${MCPANE_HUB_EXE}" a
      ${IfNot} ${Errors}
        FileClose $R6
        ${ExitDo}
      ${EndIf}
      IntOp $R5 $R5 + 1
      ${If} $R5 >= 10
        ; 3) 仍被占用：改名让路，旧文件择机删除，安装照常进行。
        Delete "$INSTDIR\${MCPANE_HUB_EXE}.old"
        ClearErrors
        Rename "$INSTDIR\${MCPANE_HUB_EXE}" "$INSTDIR\${MCPANE_HUB_EXE}.old"
        ${If} ${Errors}
          DetailPrint "警告：${MCPANE_HUB_EXE} 仍被占用且无法改名，请手动结束该进程后重试"
        ${Else}
          Delete /REBOOTOK "$INSTDIR\${MCPANE_HUB_EXE}.old"
        ${EndIf}
        ${ExitDo}
      ${EndIf}
      Sleep 300
    ${Loop}
  ${EndIf}
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro MCPaneReleaseHub
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ; 上一轮若走过改名兜底，此刻旧进程早已退出，顺手清残留。
  Delete "$INSTDIR\${MCPANE_HUB_EXE}.old"
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro MCPaneReleaseHub
!macroend

!macro NSIS_HOOK_POSTUNINSTALL
  ; PREUNINSTALL 已清过一次，这里只兜底；补一次 RMDir 是因为残留文件会让
  ; 模板的 RMDir "$INSTDIR" 失败留下空目录（非递归，目录非空即安全跳过）。
  Delete /REBOOTOK "$INSTDIR\${MCPANE_HUB_EXE}.old"
  RMDir "$INSTDIR"
!macroend

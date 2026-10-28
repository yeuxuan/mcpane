#!/usr/bin/env bash
# 编译校验 Windows NSIS 安装钩子（apps/desktop/src-tauri/windows/hooks.nsh）。
#
# 这个文件唯一的编译时机是发版那次 tauri build——CI 不跑打包（见
# platform-ci.yml 的说明），语法或宏展开写错要到打包失败才暴露，而那一步
# 既不跑测试也不可逆。这里用 makensis 提前编一遍，两种 INSTALLMODE 各一次。
#
# 插件 DLL 单独取：makensis 会解析 PE 导出表校验 nsis_tauri_utils:: 的函数名，
# 拼错在这一步就报错。版本 pin 死，只要符号在即可（与打包实际用的版本无关）。
set -euo pipefail

PLUGIN_VERSION="nsis_tauri_utils-v0.5.3"
PLUGIN_URL="https://github.com/tauri-apps/nsis-tauri-utils/releases/download/${PLUGIN_VERSION}/nsis_tauri_utils.dll"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOOKS_DIR="$ROOT/apps/desktop/src-tauri/windows"

if ! command -v makensis >/dev/null 2>&1; then
  echo "缺少 makensis：macOS 用 'brew install makensis'，Debian/Ubuntu 用 'apt-get install -y nsis'" >&2
  exit 1
fi

# makensis 在 macOS/Linux 上按 locale 解码 UTF-8 脚本：LANG 为空（非登录 shell、
# 工具调起的 bash）时遇到中文注释就报 Bad text encoding，钩子本身没错。
[[ "$(locale charmap 2>/dev/null)" == "UTF-8" ]] || export LC_ALL=C.UTF-8

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/plugins/x86-unicode"

if ! curl -sfL "$PLUGIN_URL" -o "$WORK/plugins/x86-unicode/nsis_tauri_utils.dll"; then
  echo "下载 NSIS 插件失败：$PLUGIN_URL" >&2
  exit 1
fi

for mode in currentUser perMachine; do
  echo "==> makensis 校验 hooks.nsh（INSTALLMODE=${mode}）"
  # -NOCD：保持 CWD 在 hooks 目录，脚本里 !include "hooks.nsh" 才找得到。
  (cd "$HOOKS_DIR" && makensis -NOCD \
    "-DPLUGINDIR=$WORK/plugins/x86-unicode" \
    "-DOUTFILE=$WORK/hooks-check-$mode.exe" \
    "-DINSTALLMODE=$mode" \
    hooks-syntax-check.nsi) >"$WORK/log-$mode.txt" 2>&1 || {
    cat "$WORK/log-$mode.txt" >&2
    echo "NSIS 安装钩子编译失败（INSTALLMODE=${mode}）" >&2
    exit 1
  }
done

echo "NSIS 安装钩子编译通过"

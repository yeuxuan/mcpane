#!/usr/bin/env bash
# 启动 MCPane 桌面开发环境（Vite + Tauri + sidecar Hub）。
# 用法：pnpm dev   或   bash scripts/dev.sh
# 兼容 macOS 自带 bash 3.2。

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "缺少 $1，请先安装后再启动。" >&2
    exit 1
  }
}

need node
need pnpm
need rustc
need cargo

if [ ! -d "$ROOT/node_modules" ] || [ ! -d "$ROOT/apps/desktop/node_modules" ]; then
  echo "依赖未安装，正在 pnpm install --frozen-lockfile …"
  pnpm install --frozen-lockfile
fi

port_in_use() {
  command -v lsof >/dev/null 2>&1 || return 1
  lsof -iTCP:"$1" -sTCP:LISTEN >/dev/null 2>&1
}

if port_in_use 1420 || port_in_use 24860; then
  echo "开发端口已被占用（Vite :1420 / Hub :24860）。若 MCPane 已在跑，直接用那个窗口即可。" >&2
  exit 1
fi

echo "启动 MCPane（Tauri dev）"
echo "  前端  http://localhost:1420/"
echo "  Hub   http://127.0.0.1:24860/mcp"
echo

exec pnpm --filter @mcpane/desktop tauri dev

#!/usr/bin/env bash
# MCPane 品牌标识扫描（AGENTS.md 规则 1 / 执行计划 §0.1）
# 禁止出现品牌错误写法（McPane / Mcpane / mcPane / "MCP Pane"）。
# 需要引用禁用写法的文档行，追加标记：brand-scan: allow-prohibited-spellings
# 兼容 macOS 自带 bash 3.2。

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

ALLOW_MARKER='brand-scan: allow-prohibited-spellings'
PROHIBITED='McPane|Mcpane|mcPane|MCP Pane'

violations="$(
  git ls-files --cached --others --exclude-standard \
    | grep -Ev '\.(png|jpg|jpeg|gif|ico|icns|woff2?|ttf|lock)$' \
    | grep -Ev '(^|/)(pnpm-lock\.yaml|Cargo\.lock)$' \
    | grep -v '^scripts/brand-scan\.sh$' \
    | xargs grep -EnH "$PROHIBITED" 2>/dev/null \
    | grep -Fv "$ALLOW_MARKER" \
    || true
)"

if [ -n "$violations" ]; then
  echo "brand-scan: 发现禁用写法："
  echo "$violations"
  echo "brand-scan: FAILED"
  exit 1
fi

echo "brand-scan: OK"

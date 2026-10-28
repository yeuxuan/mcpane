#!/usr/bin/env bash
# 发布前静态检查：拦截「本地演练用的临时配置」被误提交。
#
# 做端到端更新演练时，必须把 endpoint 指向本地 http 服务并打开
# dangerousInsecureTransportProtocol（否则 updater 插件在 release 构建下
# 直接 panic，app 起不来）。这两项一旦跟着 tag 发出去，updater 就会接受
# 明文端点——签名校验虽还在，但少一层防护，且老版本永远改不回来。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONF="$ROOT/apps/desktop/src-tauri/tauri.conf.json"
FAIL=0

if grep -q '"dangerousInsecureTransportProtocol"' "$CONF"; then
  echo "tauri.conf.json 残留 dangerousInsecureTransportProtocol——演练配置未清理" >&2
  FAIL=1
fi

# endpoints 数组里的每个 URL 都必须是 https
while IFS= read -r url; do
  if [[ "$url" != https://* ]]; then
    echo "updater endpoint 不是 https: $url" >&2
    FAIL=1
  fi
done < <(sed -n '/"endpoints"/,/]/p' "$CONF" | grep -o 'https\?://[^"]*')

[[ $FAIL -eq 0 ]] || exit 1
echo "release-guard: OK"

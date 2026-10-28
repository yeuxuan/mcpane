#!/usr/bin/env bash
# 构建 mcpane-hub 并按 Tauri externalBin 约定拷贝为 binaries/mcpane-hub-{target-triple}。
# 用法：build-sidecar.sh [debug|release] [target-triple]
#   （默认 debug；tauri dev 用 debug 求快，tauri build 用 release）
#
# target 取值优先级：显式参数 > TAURI_ENV_TARGET_TRIPLE > rustc host。
# 中间那档是发布必需的：Tauri 在 beforeBuildCommand 里注入该变量，交叉编译时
# 它是「目标」架构而非 host。CI 上 `tauri build --target x86_64-apple-darwin`
# 跑在 arm64 runner，只认 host 的话会把 arm64 的 Hub 塞进 Intel 包——装得上，
# 一启动 Hub 就起不来。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROFILE="${1:-debug}"
HOST="$(rustc -vV | sed -n 's/^host: //p')"
TARGET="${2:-${TAURI_ENV_TARGET_TRIPLE:-$HOST}}"

# 数组首元素常驻，避开 bash 3.2（macOS 自带）下 set -u 展开空数组即报错的坑
ARGS=(--manifest-path "$ROOT/Cargo.toml" -p mcpane-hub --bin mcpane-hub)
[[ "$PROFILE" == "release" ]] && ARGS+=(--release)

# 只有非 host 目标才加 --target：host 构建省掉这个参数，产物目录才能与
# tauri dev 的增量缓存共用
OUT_DIR="$ROOT/target/$PROFILE"
if [[ "$TARGET" != "$HOST" ]]; then
  rustup target add "$TARGET"
  ARGS+=(--target "$TARGET")
  OUT_DIR="$ROOT/target/$TARGET/$PROFILE"
fi

EXT=""
[[ "$TARGET" == *windows* ]] && EXT=".exe"

cargo build "${ARGS[@]}"

# 增量编译可能悄悄漏掉刚加的迁移文件（见脚本头注释），编完立刻核对，
# 别把这类隐患一路带到发布产物里才被用户碰见。
bash "$ROOT/scripts/verify-embedded-migrations.sh" "$OUT_DIR/mcpane-hub$EXT"

DEST="$ROOT/apps/desktop/src-tauri/binaries"
mkdir -p "$DEST"
cp "$OUT_DIR/mcpane-hub$EXT" "$DEST/mcpane-hub-$TARGET$EXT"
echo "sidecar 就绪: $DEST/mcpane-hub-$TARGET$EXT ($PROFILE)"

#!/usr/bin/env bash
# 回收 Rust 编译产物占用。
#
# cargo 从不自动清理旧产物：每次源码改动都按新哈希生成一套 .rlib/.rmeta 留在
# deps/，incremental/ 里的增量缓存同样只增不减。本仓库还有两个独立 workspace
# （根 + apps/desktop/src-tauri），共同依赖各编译一份，堆积速度翻倍。
#
# 用法：
#   pnpm clean:target            # 清 7 天前的陈旧产物（保留增量缓存，下次编译仍是增量）
#   pnpm clean:target 2          # 只保留最近 2 天，回收更狠
#   pnpm clean:target --deep     # 连增量缓存一起清，回收最多，下次编译慢一轮
#
# 不动 release/bundle 里的安装包产物。
# 兼容 macOS 自带 bash 3.2。
#
# 光靠 cargo-sweep 不够：它按 fingerprint 认产物，交叉编译 target/<triple>/ 下
# 那几 G 和 release-staging 的旧版本暂存都不在它的账上——本仓库曾因此堆到 16G，
# 其中 4.3G 是几个月前跑一次 Windows 交叉验证留下、之后再没人碰过的 debug 产物。
# 所以这里额外按目录粒度回收下面两类，判据是「整个目录超过 DAYS 天没动过」：
#   1. 非宿主 triple 的 target/<triple>/debug（release/ 留着，那是安装包）
#   2. target/release-staging/ 下非当前版本的暂存目录

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

WORKSPACES=("." "apps/desktop/src-tauri")

DEEP=0
DAYS=7
for arg in "$@"; do
  case "$arg" in
    --deep) DEEP=1 ;;
    ''|*[!0-9]*) echo "用法: clean-target.sh [天数] [--deep]" >&2; exit 2 ;;
    *) DAYS="$arg" ;;
  esac
done

if ! command -v cargo-sweep >/dev/null 2>&1; then
  echo "缺少 cargo-sweep，正在安装……"
  cargo install cargo-sweep
fi

before="$(du -sk "$ROOT" | cut -f1)"

# 宿主 triple 的产物是日常 build/test 在用的，只有交叉编译的才按陈旧回收。
HOST="$(rustc -vV | awk '/^host: /{print $2}')"

if [ "$DEEP" -eq 1 ]; then
  for ws in "${WORKSPACES[@]}"; do
    # 交叉编译同样会生成 incremental/，别只盯着宿主那一份
    rm -rf "$ROOT/$ws/target/debug/incremental"
    for inc in "$ROOT/$ws"/target/*/debug/incremental; do
      [ -d "$inc" ] && rm -rf "$inc"
    done
  done
  echo "[done] 已清除增量编译缓存"
fi

# 陈旧的交叉编译 debug 产物：整目录超过 DAYS 天没动过就回收。
# 保留同级 release/——release-local.sh 要从 <triple>/release/bundle 取安装包。
for ws in "${WORKSPACES[@]}"; do
  for tdir in "$ROOT/$ws"/target/*/; do
    triple="$(basename "$tdir")"
    case "$triple" in
      debug|release|"$HOST"|release-staging|dist-repo|latest-json|tmp|package) continue ;;
    esac
    # 只认 triple 形态的目录，避免误伤将来新增的自定义产物目录
    case "$triple" in *-*-*) ;; *) continue ;; esac
    [ -d "$tdir/debug" ] || continue
    # -mtime 是 POSIX，GNU/BSD/bfs 通吃；-newermt 的日期串各实现解析不一致。
    # -type f 不可省：目录自身的 mtime 会被子项增删刷新，带上目录判据恒为「新」。
    if [ -z "$(find "$tdir/debug" -type f -mtime "-$DAYS" -print -quit 2>/dev/null)" ]; then
      echo "[clean] 陈旧交叉产物 $ws/target/$triple/debug（$(du -sh "$tdir/debug" | cut -f1)）"
      rm -rf "$tdir/debug"
    fi
  done
done

# 旧版本的发版暂存：当前版本以外的整目录回收。
CUR_VER="$(awk '/^\[workspace\.package\]/{f=1} f&&/^version = /{gsub(/"/,"",$3); print $3; exit}' "$ROOT/Cargo.toml")"
if [ -n "$CUR_VER" ] && [ -d "$ROOT/target/release-staging" ]; then
  for sdir in "$ROOT"/target/release-staging/*/; do
    [ -d "$sdir" ] || continue
    ver="$(basename "$sdir")"
    if [ "$ver" != "v$CUR_VER" ]; then
      # ${} 不可省：变量名后紧跟全角字符时，bash 会把多字节首字节并进变量名
      echo "[clean] 旧版发版暂存 ${ver}（$(du -sh "$sdir" | cut -f1)）"
      rm -rf "$sdir"
    fi
  done
fi

for ws in "${WORKSPACES[@]}"; do
  ( cd "$ROOT/$ws" && cargo sweep --time "$DAYS" )
done

after="$(du -sk "$ROOT" | cut -f1)"
printf '[done] %s -> %s（回收 %s）\n' \
  "$(echo "$before" | awk '{printf "%.1fG", $1/1048576}')" \
  "$(echo "$after"  | awk '{printf "%.1fG", $1/1048576}')" \
  "$(echo "$before $after" | awk '{printf "%.1fG", ($1-$2)/1048576}')"

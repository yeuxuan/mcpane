#!/usr/bin/env bash
# 校验一个或多个二进制里真的嵌入了最新迁移。sqlx::migrate! 宏是文档里明确
# 承认的已知限制：只增删 migrations/*.sql、不动本 crate 任何 .rs 时，cargo
# 增量编译不一定会感知目录变化，编出的二进制可能悄悄缺最新几条迁移。
# crates/mcpane-storage/build.rs 已经在源头堵这个坑（声明 migrations 目录
# 变化要强制重编），这里是发布前最后一道复核，双保险，不依赖前面那道有没有
# 真的生效。
#
# 用法：verify-embedded-migrations.sh <binary...>
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MIGRATIONS_DIR="$ROOT/crates/mcpane-storage/migrations"

[[ $# -ge 1 ]] || { echo "用法: $0 <binary...>" >&2; exit 1; }

LATEST="$(ls "$MIGRATIONS_DIR"/*.sql | sort | tail -1)"
BASENAME="$(basename "$LATEST" .sql)"
VERSION="$((10#${BASENAME%%_*}))"
# sqlx 由文件名派生 description（这个字符串会作为 Migration 结构体的字段被
# 编译进二进制，且只有该迁移真的被宏解析到才会存在）：去掉版本号前缀，
# 下划线换空格。不用文件里的原始 SQL 片段当标记——列名/表名不一定等于
# 文件名（如 0020 文件名含 workspace_，列名却只叫 usage_guidance），拿它
# 当标记会测出偏差。
DESC="${BASENAME#*_}"
DESC="${DESC//_/ }"

FAILED=0
for bin in "$@"; do
  if [[ ! -f "$bin" ]]; then
    echo "✗ 找不到二进制: $bin" >&2
    FAILED=1
    continue
  fi
  # grep -a 直接在二进制文件上找，不经 strings：Windows CI 的 windows-latest
  # + MSVC 工具链默认不装 binutils，strings 不一定在，grep 到处都有。
  # 顺带也避开了 `strings | grep -q` 在 pipefail 下的 SIGPIPE 假阴性
  # （grep -q 一命中就提前退出关闭读端，上游进程收到 SIGPIPE 非零退出，
  # pipefail 下整条管道被判失败——哪怕 grep 真的命中了）。
  if grep -qaF "$DESC" "$bin"; then
    echo "  ✓ $(basename "$bin") 已含最新迁移 ${VERSION}（${DESC}）"
  else
    echo "✗ $(basename "$bin") 缺少最新迁移 ${VERSION}（${DESC}）——增量编译很可能没吃到新迁移文件。" >&2
    echo "  修复：cargo clean -p mcpane-storage 后重新构建这个二进制再重试。" >&2
    FAILED=1
  fi
done

exit "$FAILED"

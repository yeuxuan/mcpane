#!/usr/bin/env bash
# 从 CHANGELOG.md 切出指定版本的条目——Release Notes 的唯一来源。
# 用法：changelog-notes.sh 0.2.2   （或 v0.2.2 / Unreleased）
#
# 取「## [x.y.z]」标题之后、下一个版本标题或文末链接定义之前的内容；
# 找不到或为空即失败。release.yml / rebuild-latest-json.sh / release-local.sh /
# release-tag.sh 都用这一份，切片规则只在这里维护。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="${1:?用法: changelog-notes.sh <版本号>}"
VERSION="${VERSION#v}"

NOTES="$(awk -v ver="## [$VERSION]" '
  index($0, ver) == 1 { grab = 1; next }
  grab && (/^## \[/ || /^\[.*\]: http/) { exit }
  grab { print }
' "$ROOT/CHANGELOG.md")"

[[ -n "${NOTES//[[:space:]]/}" ]] || {
  echo "CHANGELOG.md 里找不到 $VERSION 的条目（或条目为空）" >&2
  exit 1
}
printf '%s\n' "$NOTES"

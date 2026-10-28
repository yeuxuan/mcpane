#!/usr/bin/env bash
# 把 Release 附件名统一成 MCPane_<版本>_<os>-<arch>.<ext>。
# 用法：normalize-release-assets.sh <tag>   例：normalize-release-assets.sh v0.3.2
# 由 publish-release.sh 在重建 latest.json 之前调用。
#
# 为什么需要这一步：Tauri 原名里 macOS Intel 的 dmg 和 Windows 安装包同叫
# x64、Linux 叫 amd64，Release 页上只能靠扩展名猜平台。release-local.sh 在
# 本地改名时已经消掉了，但 CI 的 tauri-action 按原名上传（改不了），两条
# 通道混用就会出现两套名字。收在发布前统一，附件从哪来都一样。
#
# 只改名不重传：GitHub 的 asset PATCH 不动字节，.sig 签的是内容、与名字无关。
# 幂等——已经是规范名的原样跳过，重跑安全。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="${RELEASE_REPO:-yeuxuan/mcpane}"
TAG="${1:?用法: normalize-release-assets.sh <tag>}"
VERSION="${TAG#v}"

LIST="$(gh api "repos/$REPO/releases" --paginate \
  --jq ".[] | select(.tag_name==\"$TAG\") | .id")" \
  || { echo "查询 Release 列表失败" >&2; exit 1; }
ID="$(head -1 <<<"$LIST")"
[[ "$ID" =~ ^[0-9]+$ ]] || { echo "找不到 $TAG 的 Release" >&2; exit 1; }

ASSETS="$(gh api "repos/$REPO/releases/$ID/assets" --paginate \
  --jq '.[] | [.id, .name] | @tsv')" \
  || { echo "查询附件列表失败" >&2; exit 1; }

# 输出「asset_id<TAB>新名」，只列需要改的。附件列表走环境变量而非 stdin：
# stdin 已经被这里的 heredoc 占去传脚本了
RENAMES="$(VERSION="$VERSION" ASSETS="$ASSETS" python3 - <<'PY'
import os, sys

version = os.environ["VERSION"]
prefix = f"MCPane_{version}_"

# Tauri 原名的架构段 -> 规范平台段。按扩展名分组，因为原名里 x64 在
# dmg 上指 macOS Intel、在 -setup.exe 上指 Windows
BY_EXT = {
    ".dmg":         {"aarch64": "macos-aarch64", "x64": "macos-x86_64"},
    ".app.tar.gz":  {"aarch64": "macos-aarch64", "x64": "macos-x86_64"},
    "-setup.exe":   {"x64": "windows-x86_64", "arm64": "windows-aarch64"},
    ".AppImage":    {"amd64": "linux-x86_64", "x86_64": "linux-x86_64",
                     "aarch64": "linux-aarch64", "arm64": "linux-aarch64"},
}
CANON = {v for m in BY_EXT.values() for v in m.values()}

for line in os.environ["ASSETS"].splitlines():
    if not line.strip():
        continue
    aid, name = line.split("\t", 1)
    base, sig = (name[:-4], ".sig") if name.endswith(".sig") else (name, "")
    if not base.startswith(prefix):
        continue                                  # latest.json 等非产物附件
    for ext, mapping in BY_EXT.items():
        if base.endswith(ext):
            arch = base[len(prefix):-len(ext)]
            if arch in CANON:
                break                             # 已经是规范名
            slug = mapping.get(arch)
            if slug:
                print(f"{aid}\t{prefix}{slug}{ext}{sig}")
            else:
                print(f"认不出架构段 {arch!r}: {name}", file=sys.stderr)
                sys.exit(1)
            break
PY
)" || { echo "附件名归一化失败" >&2; exit 1; }

if [[ -z "${RENAMES//[[:space:]]/}" ]]; then
  echo "  · 附件名已符合规范，无需改名"
  exit 0
fi

while IFS=$'\t' read -r AID NEW; do
  [[ -n "$AID" ]] || continue
  gh api --method PATCH "repos/$REPO/releases/assets/$AID" \
    -f name="$NEW" --jq '"  · 改名为 " + .name'
done <<<"$RENAMES"

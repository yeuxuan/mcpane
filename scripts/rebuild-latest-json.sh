#!/usr/bin/env bash
# 按草稿 Release 里实际存在的 .sig 附件，确定性地重建 latest.json。
# 用法：rebuild-latest-json.sh <tag>   例：rebuild-latest-json.sh v0.2.0
# 常规路径下由 publish-release.sh 在发布前调用（latest.json 的唯一写者）。
#
# 为什么需要这一步：产物来自多条独立通道（macOS / Windows / Linux 各机
# 上传），谁先谁后不定；让任何一条通道顺手写 latest.json 都是无锁的读-改-写，
# 晚到的写入会覆盖早到的，被覆盖的平台从更新清单里消失且全程无报错
# （tauri-action issue #1270 同款竞态）。所以收口在发布前：按 .sig 一次
# 算全并校验平台齐全，缺平台就失败——宁可红，不发悄悄少平台的清单。
#
# 认证：gh 凭据需对分发仓库有 Contents: Read and write。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="${RELEASE_REPO:-yeuxuan/mcpane}"
TAG="${1:-}"
[[ -n "$TAG" ]] || { echo "用法: $0 <tag>  例: $0 v0.2.0" >&2; exit 1; }
VERSION="${TAG#v}"

# 期望齐全的平台；缺任何一个都算失败，不接受静默少一个平台
EXPECTED="${EXPECTED_PLATFORMS:-darwin-aarch64 darwin-x86_64 windows-x86_64 linux-x86_64}"

WORK="$ROOT/target/latest-json/$TAG"
rm -rf "$WORK"; mkdir -p "$WORK/sigs"

# 草稿的 tag 尚不存在，gh release download 按 tag 查不到草稿（v0.2.1 教训）
# ——走列表接口找 id，附件按 asset id 取
LIST="$(gh api "repos/$REPO/releases" --paginate \
  --jq ".[] | select(.tag_name==\"$TAG\") | .id")" \
  || { echo "查询 Release 列表失败" >&2; exit 1; }
ID="$(head -1 <<<"$LIST")"
[[ "$ID" =~ ^[0-9]+$ ]] || { echo "找不到 $TAG 的 Release" >&2; exit 1; }

echo "==> 从 $REPO 的 Release $ID 拉取 .sig 附件"
ASSETS="$(gh api "repos/$REPO/releases/$ID/assets" --paginate \
  --jq '.[] | [.id, .name] | @tsv')" \
  || { echo "查询附件列表失败" >&2; exit 1; }
while IFS=$'\t' read -r AID NAME; do
  [[ "$NAME" == *.sig ]] || continue
  gh api "repos/$REPO/releases/assets/$AID" \
    -H "Accept: application/octet-stream" > "$WORK/sigs/$NAME"
  echo "  · $NAME"
done <<<"$ASSETS"

NOTES="$(bash "$ROOT/scripts/changelog-notes.sh" "$VERSION")"

NOTES="$NOTES" EXPECTED="$EXPECTED" python3 - "$WORK" "$VERSION" "$TAG" "$REPO" <<'PY'
import json, os, sys, pathlib, datetime

work, version, tag, repo = sys.argv[1:5]
base = f"https://github.com/{repo}/releases/download/{tag}"
sigs = pathlib.Path(work) / "sigs"

SLUG_TO_PLATFORM = {
    "macos-aarch64": "darwin-aarch64",
    "macos-x86_64": "darwin-x86_64",
    "windows-x86_64": "windows-x86_64",
    "linux-x86_64": "linux-x86_64",
    "linux-aarch64": "linux-aarch64",
}


def platform_of(pkg: str):
    """按产物名里的 _<os>-<arch> 段判定 updater 平台键。release-local.sh 已把
    所有附件统一改成这套名字，直接查表即可——不再按扩展名加架构关键字反推
    （Tauri 原名里 macOS Intel 与 Windows 同叫 x64，那种反推分不开）。"""
    if not pkg.endswith((".app.tar.gz", "-setup.exe", ".AppImage")):
        return None
    for slug, key in SLUG_TO_PLATFORM.items():
        if f"_{slug}" in pkg:
            return key
    return None

platforms, skipped = {}, []
for sig in sorted(sigs.glob("*.sig")):
    pkg = sig.name[:-4]                      # 去掉 .sig
    key = platform_of(pkg)
    if key is None:
        skipped.append(pkg)
        continue
    if key in platforms:
        raise SystemExit(f"平台 {key} 对应到多个产物，命名规则可能变了: {pkg}")
    platforms[key] = {
        "signature": sig.read_text().strip(),
        "url": f"{base}/{pkg}",
    }

for name in skipped:
    print(f"  跳过（不进 updater 清单）: {name}")

expected = os.environ["EXPECTED"].split()
missing = [p for p in expected if p not in platforms]
if missing:
    raise SystemExit(
        f"latest.json 缺平台: {', '.join(missing)}\n"
        f"实际只有: {', '.join(platforms) or '（空）'}\n"
        "对应平台的产物还没传上来（macOS / Windows / Linux 半程），"
        "或产物命名规则变了。"
    )

notes = os.environ.get("NOTES", "").strip()
out = pathlib.Path(work) / "latest.json"
out.write_text(json.dumps({
    "version": version,
    "notes": notes or f"MCPane {version}",
    "pub_date": datetime.datetime.now(datetime.timezone.utc)
                  .strftime("%Y-%m-%dT%H:%M:%SZ"),
    "platforms": platforms,
}, ensure_ascii=False, indent=2))
print(f"  平台齐全: {', '.join(sorted(platforms))}")
PY

echo "==> 覆盖上传 latest.json"
OLD_ID="$(awk -F'\t' '$2=="latest.json" {print $1; exit}' <<<"$ASSETS")"
if [[ -n "$OLD_ID" ]]; then
  gh api --method DELETE "repos/$REPO/releases/assets/$OLD_ID" >/dev/null
fi
gh api --method POST \
  -H "Content-Type: application/json" \
  "https://uploads.github.com/repos/$REPO/releases/$ID/assets?name=latest.json" \
  --input "$WORK/latest.json" --jq '.name' >/dev/null
echo "latest.json 已按实际产物重建完毕"

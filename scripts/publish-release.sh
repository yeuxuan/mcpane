#!/usr/bin/env bash
# 发版收尾：重建 latest.json（单写者收口）→ 核对草稿 Release → Publish
# （含 Latest 标记）→ 验证更新通道生效。
# 用法：publish-release.sh 0.2.2 [--yes]
#
# 为什么要脚本化：这是发版全流程唯一不可逆的动作——latest.json 一旦能从
# releases/latest 取到，老用户的 updater 立即开始拉新版。v0.2.1 的两个教训
# 固化在这里：草稿的 tag 尚不存在，gh release view/download 按 tag 都查不到，
# 只能走列表接口；PATCH draft=false 不会自动置 Latest，漏了 make_latest
# 的话 releases/latest 仍指旧版，老用户永远收不到更新。
#
# 环境变量：EXPECTED_PLATFORMS 覆盖平台清单（默认四平台齐全；Actions
# 彻底不可用、决定只发部分平台时用例如
# EXPECTED_PLATFORMS="darwin-aarch64 darwin-x86_64"，其余平台老用户会
# 收不到该版更新，慎用）。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="${RELEASE_REPO:-yeuxuan/mcpane}"
EXPECTED_PLATFORMS="${EXPECTED_PLATFORMS:-darwin-aarch64 darwin-x86_64 windows-x86_64 linux-x86_64}"

ok()   { echo "  ✓ $*"; }
fail() { echo "  ✗ $*" >&2; exit 1; }
confirm() {
  [[ "$ASSUME_YES" == "--yes" ]] && return 0
  read -r -p "$1 [y/N] " REPLY
  [[ "$REPLY" =~ ^[Yy]$ ]]
}

VERSION="${1:-}"
VERSION="${VERSION#v}"
ASSUME_YES="${2:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] \
  || { echo "用法: $0 <版本号> [--yes]   例: $0 0.2.2" >&2; exit 1; }
TAG="v$VERSION"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

echo "==> 定位 $REPO 的 Release $TAG"
# 不与 head 连管道：管道会吞掉 gh 的退出码（v0.2.1 事故根因之一）
LIST="$(gh api "repos/$REPO/releases" --paginate \
  --jq ".[] | select(.tag_name==\"$TAG\") | .id")" \
  || fail "查询 Release 列表失败（gh 未认证？）"
ID="$(head -1 <<<"$LIST")"
[[ "$ID" =~ ^[0-9]+$ ]] || fail "找不到 $TAG 的 Release——Release workflow 跑完了吗？"
gh api "repos/$REPO/releases/$ID" > "$WORK/release.json"
ok "Release id=$ID"

# 草稿态先按实际 .sig 重建 latest.json：产物来自多机半程上传，
# 收口只能放在发布动作之前；已发布状态默认不改写——补齐平台时先手动
# 跑 rebuild-latest-json.sh 再重跑本脚本做核对。
if [[ "$(python3 -c 'import json,sys; print(str(json.load(open(sys.argv[1]))["draft"]).lower())' \
    "$WORK/release.json")" == "true" ]]; then
  # 先统一附件名：CI 的 tauri-action 按 Tauri 原名上传（x64 在 mac 和 win 上
  # 撞词），本地 release-local.sh 已改好名，两条通道混用时收在这里拉平
  echo "==> 统一附件命名"
  bash "$ROOT/scripts/normalize-release-assets.sh" "$TAG"
  echo "==> 按实际 .sig 重建 latest.json"
  EXPECTED_PLATFORMS="$EXPECTED_PLATFORMS" \
    bash "$ROOT/scripts/rebuild-latest-json.sh" "$TAG"
  # latest.json 附件换了个 asset id，重取 release.json 再核对
  gh api "repos/$REPO/releases/$ID" > "$WORK/release.json"
fi

echo "==> 核对附件"
read -r LATEST_AID DRAFT < <(
  EXPECTED_PLATFORMS="$EXPECTED_PLATFORMS" \
  python3 - "$WORK/release.json" "$VERSION" <<'PY'
import json, os, sys

path, version = sys.argv[1:3]
rel = json.load(open(path))
names = {a["name"]: a["id"] for a in rel["assets"]}

# 附件名由 release-local.sh 统一成 MCPane_<ver>_<os>-<arch>.<ext>，这里照单核对；
# 对不上就红——名字漂了比少一个平台更该拦下来
def mac(slug: str) -> list[str]:
    stem = f"MCPane_{version}_{slug}"
    return [f"{stem}.dmg", f"{stem}.app.tar.gz", f"{stem}.app.tar.gz.sig"]

per_platform = {
    "darwin-aarch64": mac("macos-aarch64"),
    "darwin-x86_64": mac("macos-x86_64"),
    "windows-x86_64": [
        f"MCPane_{version}_windows-x86_64-setup.exe",
        f"MCPane_{version}_windows-x86_64-setup.exe.sig",
    ],
    "linux-x86_64": [
        f"MCPane_{version}_linux-x86_64.AppImage",
        f"MCPane_{version}_linux-x86_64.AppImage.sig",
    ],
    "linux-aarch64": [
        f"MCPane_{version}_linux-aarch64.AppImage",
        f"MCPane_{version}_linux-aarch64.AppImage.sig",
    ],
}

expected = ["latest.json"]
for p in os.environ["EXPECTED_PLATFORMS"].split():
    expected += per_platform[p]

missing = [n for n in expected if n not in names]
if missing:
    sys.exit("附件缺失: " + ", ".join(missing)
             + "\n对应平台的构建没传上来，或产物命名规则变了")
extra = sorted(set(names) - set(expected))
if extra:
    print(f"  · 有计划外附件（不拦截）: {', '.join(extra)}", file=sys.stderr)
if not (rel.get("body") or "").strip():
    sys.exit("Release 描述为空——create-release 的 CHANGELOG 切片失败了？")
print(f"  ✓ 附件齐全（{len(expected)} 件），描述非空", file=sys.stderr)
print(names["latest.json"], str(rel["draft"]).lower())
PY
) || fail "附件核对未通过"

echo "==> 校验 latest.json"
# 草稿附件没有公开下载地址，必须带认证按 asset id 取
gh api "repos/$REPO/releases/assets/$LATEST_AID" \
  -H "Accept: application/octet-stream" > "$WORK/latest.json"
EXPECTED_PLATFORMS="$EXPECTED_PLATFORMS" \
  python3 - "$WORK/latest.json" "$VERSION" "$TAG" <<'PY'
import json, os, sys

path, version, tag = sys.argv[1:4]
d = json.load(open(path))
errs = []
if d.get("version") != version:
    errs.append(f"version 是 {d.get('version')!r}，预期 {version!r}")
for key in os.environ["EXPECTED_PLATFORMS"].split():
    p = (d.get("platforms") or {}).get(key)
    if not p:
        errs.append(f"缺平台 {key}")
        continue
    if not (p.get("signature") or "").strip():
        errs.append(f"{key} 的 signature 为空")
    if f"/releases/download/{tag}/" not in (p.get("url") or ""):
        errs.append(f"{key} 的 url 不指向 {tag}: {p.get('url')}")
notes = (d.get("notes") or "").strip()
if not notes or notes == f"MCPane {version}":
    errs.append("notes 为空或是兜底占位——CHANGELOG 切片失败了？")
if errs:
    sys.exit("  ✗ latest.json 有问题:\n    " + "\n    ".join(errs))
first = notes.splitlines()[0]
print(f"  ✓ latest.json: {version} / {', '.join(sorted(d['platforms']))}")
print(f"    notes 首行: {first}")
PY

if [[ "$DRAFT" == "true" ]]; then
  echo
  echo "即将发布 $REPO ${TAG}（不可逆：老用户 updater 立即可见）"
  confirm "确认 Publish？" || { echo "已取消，草稿原样保留"; exit 1; }
  # make_latest 必须显式给：光 draft=false 不会把 Latest 标记切过来，
  # releases/latest/download/latest.json 会继续指旧版
  URL="$(gh api --method PATCH "repos/$REPO/releases/$ID" \
    -F draft=false -f make_latest=true --jq .html_url)"
  ok "已发布: $URL"
else
  echo "  · Release 已是发布状态，跳过 Publish"
  LATEST_ID="$(gh api "repos/$REPO/releases/latest" --jq .id)" || LATEST_ID=""
  if [[ "$LATEST_ID" != "$ID" ]]; then
    confirm "当前 Latest 标记不在 $TAG 上，切换过来？" \
      && gh api --method PATCH "repos/$REPO/releases/$ID" \
           -f make_latest=true --jq .html_url >/dev/null \
      && ok "Latest 标记已指向 $TAG"
  fi
fi

echo "==> 验证更新通道（releases/latest/download/latest.json）"
GOT=""
for i in $(seq 1 12); do
  GOT="$(curl -fsSL \
    "https://github.com/$REPO/releases/latest/download/latest.json?nocache=$RANDOM$i" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin).get("version",""))' \
    2>/dev/null || true)"
  [[ "$GOT" == "$VERSION" ]] && break
  sleep 5
done
[[ "$GOT" == "$VERSION" ]] \
  || fail "更新通道仍返回「${GOT:-取不到}」（预期 ${VERSION}）——CDN 缓存或 Latest 标记问题，稍后重跑本脚本即可（已发布状态下它只做验证）"
ok "更新通道已生效：老用户将收到 $VERSION"

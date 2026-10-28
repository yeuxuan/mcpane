#!/usr/bin/env bash
# 发版的本地构建半程：在哪台机器上跑，就构建并上传哪个平台的产物。
#   - macOS：Apple Silicon + Intel 双架构（dmg / app.tar.gz / .sig 共 6 件）
#   - Windows（Git Bash）：NSIS 安装包（-setup.exe / .sig 共 2 件）
#   - Linux：AppImage（.AppImage / .sig 共 2 件；须在真 Linux / WSL2 上编）
# 附件一律改名成 MCPane_<版本>_<os>-<arch>.<ext>（Tauri 原名里 macOS Intel 与
# Windows 同叫 x64、Linux 叫 amd64，Release 页上分不清平台）。
# 用法：scripts/release-local.sh 0.3.0
#       scripts/release-local.sh --check-apple-distribution
#
# 分工（流程见 docs/RELEASE_CHECKLIST.md）：macOS 半程走本脚本——updater
# 签名私钥只存在开发者手里，而 macOS runner 按 10 倍计费；Windows 与 Linux
# 默认由 Release workflow 出包，本脚本的这两个分支是 Actions 不可用时的兜底。
# latest.json 统一由 publish-release.sh 在发布前按实际 .sig 重建，本脚本不碰。
# 草稿按 tag_name 找到就复用、找不到才创建——与 CI 的 create-release、
# 另一台机器上的本脚本互相幂等，谁先跑都行。
#
# 前置：
#   - tag 已由 release-tag.sh 打好（预检含 CI 绿、版本一致、CHANGELOG 就绪），
#     本机 HEAD 停在 tag 上且工作树干净——保证产物来自 tag 指向的那份代码
#   - ~/.tauri/mcpane.key 在位（更新包签名私钥；Windows 机器上放
#     %USERPROFILE%\.tauri\mcpane.key，用安全通道传拷，用完即删）
#   - gh 已认证且对分发仓库有写权限；Rust 工具链与 pnpm 就绪
# 环境变量：SKIP_BUILD=1 复用已有产物（上传环节失败重试时不必重编）。
# macOS 还必须设置 APPLE_SIGNING_IDENTITY、APPLE_API_ISSUER、APPLE_API_KEY、
# APPLE_API_KEY_PATH；只从当前进程环境读取，不写入仓库或日志。
#
# 老 tag 尚无本脚本的 Linux 分支时：工作树保持干净停在 tag 上，从更新的
# main 取出本文件和 scripts/macos-distribution.sh 到 /tmp 再跑
# （SCRIPT_DIR 不在仓库内时 ROOT 回退到 cwd）。
set -euo pipefail

# 允许从仓库外的拷贝运行（老 tag 里还没有新版脚本时，从 main 取一份放
# 临时目录跑，工作树保持干净停在 tag 上）：脚本自身不在仓库里就取 cwd 所在仓库
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$ROOT" ]] || ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$ROOT" ]] || { echo "找不到仓库：请在仓库目录里运行本脚本" >&2; exit 1; }
REPO="${RELEASE_REPO:-yeuxuan/mcpane}"
KEY="$HOME/.tauri/mcpane.key"

ok()   { echo "  ✓ $*"; }
fail() { echo "  ✗ $*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin)       PLATFORM=macos ;;
  MINGW*|MSYS*) PLATFORM=windows ;;
  Linux)        PLATFORM=linux ;;
  *) fail "未支持的构建平台 $(uname -s)" ;;
esac

MACOS_DISTRIBUTION="$ROOT/scripts/macos-distribution.sh"
[[ -f "$MACOS_DISTRIBUTION" ]] \
  || MACOS_DISTRIBUTION="$SCRIPT_DIR/macos-distribution.sh"
if [[ "${1:-}" == "--check-apple-distribution" ]]; then
  [[ "$PLATFORM" == "macos" ]] || fail "Apple 分发检查只能在 macOS 上运行"
  [[ -f "$MACOS_DISTRIBUTION" ]] || fail "缺少 $MACOS_DISTRIBUTION"
  bash "$MACOS_DISTRIBUTION" check
  exit 0
fi

VERSION="${1:-}"
VERSION="${VERSION#v}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] \
  || { echo "用法: $0 <版本号>  例: $0 0.3.0" >&2; exit 1; }
TAG="v$VERSION"

cd "$ROOT"
echo "==> 前置检查 ${TAG}（${PLATFORM}）"
# 全部通过再动手构建，免得编译半小时后卡在缺凭据上
if [[ "$PLATFORM" == "macos" ]]; then
  [[ -f "$MACOS_DISTRIBUTION" ]] || fail "缺少 $MACOS_DISTRIBUTION"
  bash "$MACOS_DISTRIBUTION" check
  ok "Apple Developer ID 与公证凭据通过"
fi
[[ -f "$KEY" ]] || fail "找不到签名私钥 $KEY"
gh auth status >/dev/null 2>&1 || fail "gh 未认证，先跑 gh auth login"

TAG_SHA="$(git rev-parse -q --verify "refs/tags/$TAG^{commit}")" \
  || fail "本地没有 tag ${TAG}——先 git fetch --tags（tag 由 release-tag.sh 统一打）"
[[ "$(git rev-parse HEAD)" == "$TAG_SHA" ]] \
  || fail "HEAD 不在 $TAG 上（产物必须来自 tag 那份代码）：git checkout $TAG"
[[ -z "$(git status --porcelain)" ]] || fail "工作树不干净，先提交或清理"
ok "HEAD 在 $TAG (${TAG_SHA:0:8})，工作树干净"

bash scripts/bump-version.sh | grep -qx "版本号一致: $VERSION" \
  || fail "仓库版本号不是 ${VERSION}（tag 打错了？）"
bash scripts/release-guard.sh >/dev/null
ok "版本号一致，无演练配置残留"

# ---- 定位或创建草稿 Release ----
# 草稿的 tag 在分发仓库尚不存在，gh release view/download 按 tag 都查不到
# （v0.2.1 教训）——一律走列表接口按 tag_name 找 id，之后全按 id 操作。
LIST="$(gh api "repos/$REPO/releases" --paginate \
  --jq ".[] | select(.tag_name==\"$TAG\") | .id")" \
  || fail "查询 Release 列表失败"
ID="$(head -1 <<<"$LIST")"
if [[ -n "$ID" ]]; then
  ok "复用已存在的 Release（id=${ID}）"
else
  NOTES_FILE="$(mktemp)"
  bash scripts/changelog-notes.sh "$VERSION" > "$NOTES_FILE"
  # 用 gh api -f/-F 而非 jq 拼 JSON：Windows 的 Git Bash 没有 jq。
  # target_commitish=main：草稿 tag 将来落在分发仓库自己的 main 上，
  # 源码仓库的 commit SHA 在那边不存在
  ID="$(gh api "repos/$REPO/releases" --method POST \
        -f tag_name="$TAG" \
        -f name="MCPane $TAG" \
        -F body="@$NOTES_FILE" \
        -F draft=true \
        -F prerelease=false \
        -f target_commitish=main \
        --jq '.id')"
  rm -f "$NOTES_FILE"
  ok "已建草稿 Release（id=${ID}）"
fi
[[ "$ID" =~ ^[0-9]+$ ]] || fail "releaseId 非数字: '$ID'"

STAGE="$ROOT/target/release-staging/$TAG-$PLATFORM"
rm -rf "$STAGE"; mkdir -p "$STAGE"

# ---- 构建：updater 签名经环境变量注入 ----
# 变量名没有 _PATH 后缀，写错不会报错、只是静默不签名，产物缺 .sig
export TAURI_SIGNING_PRIVATE_KEY="$KEY"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""

[[ "${SKIP_BUILD:-}" == "1" ]] || pnpm install --frozen-lockfile

build_mac() { # $1=rust target  $2=Tauri 产物里的架构后缀  $3=附件名里的平台段
  local target="$1" suffix="$2" slug="$3" expected_arch staged_dmg
  case "$target" in
    aarch64-apple-darwin) expected_arch=arm64 ;;
    x86_64-apple-darwin) expected_arch=x86_64 ;;
    *) fail "未知 macOS target：$target" ;;
  esac
  if [[ "${SKIP_BUILD:-}" == "1" ]]; then
    echo "==> 跳过构建，复用 $target 已有产物"
  else
    echo "==> 构建 $target"
    pnpm --filter @mcpane/desktop tauri build --target "$target"
  fi

  local out="$ROOT/apps/desktop/src-tauri/target/$target/release/bundle"
  # desktop 主程序和 tauri build 里自动跑的 sidecar 各自独立编译同一份
  # mcpane-storage（各有各的 target 目录），增量编译可能只有一边吃到新迁移
  # 文件——build-sidecar.sh 已经查过 hub 那份，这里补上 desktop 主程序这份，
  # 两个二进制都核对到才算数（2026-08-08 v0.3.0 的教训）。
  bash "$ROOT/scripts/verify-embedded-migrations.sh" \
    "$out/macos/MCPane.app/Contents/MacOS/mcpane" \
    "$out/macos/MCPane.app/Contents/MacOS/mcpane-hub"
  # 一律改成 MCPane_<ver>_<os>-<arch>.<ext>：Tauri 原名里 macOS Intel 和
  # Windows 同叫 x64，Release 页上只能靠扩展名猜平台；两个架构的 .app.tar.gz
  # 更是同名撞车。publish-release.sh 与 rebuild-latest-json.sh 都按这套名字核对。
  staged_dmg="$STAGE/MCPane_${VERSION}_${slug}.dmg"
  cp "$out/dmg/MCPane_${VERSION}_${suffix}.dmg" "$staged_dmg"
  cp "$out/macos/MCPane.app.tar.gz"     "$STAGE/MCPane_${VERSION}_${slug}.app.tar.gz"
  cp "$out/macos/MCPane.app.tar.gz.sig" "$STAGE/MCPane_${VERSION}_${slug}.app.tar.gz.sig"

  # 验证最终待上传的 DMG，而不是构建目录里的中间 App。任何一项失败都会在
  # 上传循环开始前退出，杜绝把 ad-hoc、未公证或错误架构的产物传到 Release。
  bash "$MACOS_DISTRIBUTION" notarize "$staged_dmg"
  bash "$MACOS_DISTRIBUTION" verify "$staged_dmg" "$expected_arch"
}

build_windows() {
  if [[ "${SKIP_BUILD:-}" == "1" ]]; then
    echo "==> 跳过构建，复用已有产物"
  else
    echo "==> 构建 NSIS 安装包（host msvc）"
    pnpm --filter @mcpane/desktop tauri build
  fi
  # host 构建不带 --target，产物在 target/release 下。Tauri 原名 x64-setup.exe
  # 与 macOS Intel 的 x64 撞词，改名时补上平台段（扩展名保持 .exe——Windows
  # updater 按 URL 后缀选安装方式）
  bash "$ROOT/scripts/verify-embedded-migrations.sh" \
    "$ROOT/apps/desktop/src-tauri/target/release/mcpane.exe" \
    "$ROOT/apps/desktop/src-tauri/binaries/mcpane-hub-$(rustc -vV | sed -n 's/^host: //p').exe"
  local out="$ROOT/apps/desktop/src-tauri/target/release/bundle/nsis"
  cp "$out/MCPane_${VERSION}_x64-setup.exe" \
     "$STAGE/MCPane_${VERSION}_windows-x86_64-setup.exe"
  cp "$out/MCPane_${VERSION}_x64-setup.exe.sig" \
     "$STAGE/MCPane_${VERSION}_windows-x86_64-setup.exe.sig"
}

build_linux() {
  local host
  host="$(rustc -vV | sed -n 's/^host: //p')"
  case "$host" in
    x86_64-*-linux-*) ;;
    *) fail "Linux 发版目前只支持 x86_64 host（当前 $host）；aarch64 另开通道" ;;
  esac
  if [[ "${SKIP_BUILD:-}" == "1" ]]; then
    echo "==> 跳过构建，复用已有产物"
  else
    echo "==> 构建 AppImage（host，$host）"
    # --bundles 显式指定：老 tag 的 tauri.conf 可能尚未列入 appimage
    pnpm --filter @mcpane/desktop tauri build --bundles appimage
  fi
  bash "$ROOT/scripts/verify-embedded-migrations.sh" \
    "$ROOT/apps/desktop/src-tauri/target/release/mcpane" \
    "$ROOT/apps/desktop/src-tauri/binaries/mcpane-hub-$host"
  # Tauri 2：updater 直接签 AppImage 本体（.AppImage + .AppImage.sig）。
  # Tauri 原名用 amd64，与另两个平台的 x86_64/x64 三种叫法并存，改名统一
  local out="$ROOT/apps/desktop/src-tauri/target/release/bundle/appimage"
  local img
  img="$(ls -1 "$out"/MCPane_"${VERSION}"_*.AppImage 2>/dev/null | head -1)"
  [[ -n "$img" && -f "$img" ]] || fail "找不到 AppImage（在 $out）"
  [[ -s "${img}.sig" ]] || fail "$(basename "$img").sig 缺失或为空"
  cp "$img"       "$STAGE/MCPane_${VERSION}_linux-x86_64.AppImage"
  cp "${img}.sig" "$STAGE/MCPane_${VERSION}_linux-x86_64.AppImage.sig"
}

if [[ "$PLATFORM" == "macos" ]]; then
  build_mac aarch64-apple-darwin aarch64 macos-aarch64
  build_mac x86_64-apple-darwin  x64     macos-x86_64
elif [[ "$PLATFORM" == "windows" ]]; then
  build_windows
else
  build_linux
fi

for f in "$STAGE"/*.sig; do
  [[ -s "$f" ]] || fail "$(basename "$f") 为空——签名没生效，检查 TAURI_SIGNING_PRIVATE_KEY"
done

# ---- 按 asset API 上传（重试幂等：同名旧附件先删再传） ----
COUNT="$(ls -1 "$STAGE" | wc -l | tr -d ' ')"
echo "==> 上传 $COUNT 件产物到 Release $ID"
EXISTING="$(gh api "repos/$REPO/releases/$ID/assets" --paginate \
  --jq '.[] | [.id, .name] | @tsv')" || fail "查询已有附件失败"
for f in "$STAGE"/*; do
  NAME="$(basename "$f")"
  OLD_ID="$(awk -F'\t' -v n="$NAME" '$2==n {print $1; exit}' <<<"$EXISTING")"
  if [[ -n "$OLD_ID" ]]; then
    gh api --method DELETE "repos/$REPO/releases/assets/$OLD_ID" >/dev/null
    echo "  · 已删除同名旧附件 $NAME"
  fi
  gh api --method POST \
    -H "Content-Type: application/octet-stream" \
    "https://uploads.github.com/repos/$REPO/releases/$ID/assets?name=$NAME" \
    --input "$f" --jq '.name' | sed 's/^/  ✓ /'
done

echo
echo "$PLATFORM 半程完成。所有平台产物齐了之后："
echo "  bash scripts/publish-release.sh $VERSION"
echo "（publish-release.sh 会按实际 .sig 重建 latest.json 并校验平台齐全）"

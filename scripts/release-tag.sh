#!/usr/bin/env bash
# 发版入口：预检全部通过后打 tag 并推送。随后 macOS 半程在开发机跑
# scripts/release-local.sh，Windows 与 Linux 由 Release workflow 出包
#（tag 不自动触发任何构建，两边都要手动起）。
# 用法：release-tag.sh 0.2.2 [--yes]
#
# 只校验不改写：版本号用 scripts/bump-version.sh 改，CHANGELOG 手写。
# 预检把「推了 tag 才在 CI 里炸」的错误全部前移——tag 推出去之后再修，
# 就得改完重推 main 再 workflow_dispatch 重跑，慢一轮。
# 环境变量：SKIP_CI_CHECK=1 跳过「HEAD 的 CI 全绿」检查（仅限 CI 基础设施
# 本身故障时使用）。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC_REPO="yeuxuan/mcpane"

ok()   { echo "  ✓ $*"; }
fail() { echo "  ✗ $*" >&2; exit 1; }

VERSION="${1:-}"
VERSION="${VERSION#v}"
ASSUME_YES="${2:-}"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] \
  || { echo "用法: $0 <版本号> [--yes]   例: $0 0.2.2" >&2; exit 1; }
TAG="v$VERSION"

cd "$ROOT"
echo "==> 预检 $TAG"

[[ -z "$(git status --porcelain)" ]] || fail "工作树不干净，先提交或清理"
ok "工作树干净"

BRANCH="$(git branch --show-current)"
[[ "$BRANCH" == "main" ]] || fail "当前在 ${BRANCH}，发版必须从 main 打 tag"
git fetch -q origin main
SHA="$(git rev-parse HEAD)"
[[ "$SHA" == "$(git rev-parse origin/main)" ]] \
  || fail "本地 main 与 origin/main 不一致，先 push 或 pull"
ok "main 与远端同步 (${SHA:0:8})"

bash scripts/bump-version.sh | grep -qx "版本号一致: $VERSION" \
  || fail "仓库版本号不是 ${VERSION}，先跑 scripts/bump-version.sh $VERSION"
ok "版本号四处一致: $VERSION"

bash scripts/release-guard.sh >/dev/null
ok "无演练配置残留"

bash scripts/changelog-notes.sh "$VERSION" >/dev/null \
  || fail "CHANGELOG.md 缺 ## [$VERSION] 条目"
grep -q "^\[$VERSION\]: http" CHANGELOG.md \
  || fail "CHANGELOG.md 文末缺 [$VERSION] 的对比链接定义"
# 发版时 [Unreleased] 必须已并入本版条目——残留内容意味着这批改动
# 不会出现在任何版本的 Release Notes 里
if bash scripts/changelog-notes.sh Unreleased >/dev/null 2>&1; then
  fail "[Unreleased] 段还有内容，先并入 ## [$VERSION]"
fi
ok "CHANGELOG 条目与链接就绪，[Unreleased] 已清空"

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  fail "本地已存在 tag $TAG"
fi
if [[ -n "$(git ls-remote --tags origin "refs/tags/$TAG")" ]]; then
  fail "远端已存在 tag ${TAG}——重跑构建应该用: gh workflow run release.yml -R $SRC_REPO -f tag=$TAG"
fi
ok "tag 未被占用"

if [[ "${SKIP_CI_CHECK:-}" == "1" ]]; then
  echo "  ⚠ 已跳过 CI 状态检查 (SKIP_CI_CHECK=1)"
else
  # 必须限定 --workflow：按 commit 查全部 check-runs 会把 Release workflow
  # 的历史失败尝试也算进来。取该 workflow 最新一次 run 的结论
  check_workflow_green() { # $1=workflow 名  $2=没跑过时的提示
    local state
    state="$(gh run list -R "$SRC_REPO" --commit "$SHA" --workflow "$1" \
      --limit 1 --json status,conclusion \
      --jq 'if length == 0 then "none"
            elif .[0].status != "completed" then "pending"
            else .[0].conclusion end')" \
      || fail "查询 $1 状态失败（gh 未认证？先跑 gh auth login）"
    case "$state" in
      success) ok "HEAD 的 $1 已全绿" ;;
      none)    fail "HEAD 还没有 $1 记录：$2" ;;
      pending) fail "HEAD 的 $1 还在跑，等全绿再发" ;;
      *)       fail "HEAD 的 $1 结论是 ${state}，先修绿（gh run list -R $SRC_REPO --commit ${SHA:0:8}）" ;;
    esac
  }
  check_workflow_green "CI" "等 push 触发的 CI 跑完再发"
  # Windows 专属分支的 Rust 与前端不随 push 跑、也没有定时巡检，发版前必须
  # 在当前 HEAD 上绿过一次——这是它唯一的关口。首次跑可能一次红出自上次
  # 发版以来积压的多个问题，属预期
  check_workflow_green "Platform CI" \
    "先跑 gh workflow run platform-ci.yml -R $SRC_REPO 并等它全绿（按 main HEAD 跑）"
fi

echo
echo "预检全部通过。即将打 tag 并推送："
echo "  $TAG @ ${SHA:0:8}"
if [[ "$ASSUME_YES" != "--yes" ]]; then
  read -r -p "继续？[y/N] " REPLY
  [[ "$REPLY" =~ ^[Yy]$ ]] || { echo "已取消，什么都没做"; exit 1; }
fi

git tag "$TAG"
git push origin "refs/tags/$TAG"

echo
echo "tag 已推送。接下来（两条通道可并行，谁先跑都行）："
echo "  macOS 半程      bash scripts/release-local.sh $VERSION"
echo "  Windows+Linux   gh workflow run release.yml -R $SRC_REPO -f tag=$TAG"
echo "  产物齐了之后    bash scripts/publish-release.sh $VERSION"
echo "  （Actions 不可用时，这两个平台也能在本机跑 release-local.sh 出包）"

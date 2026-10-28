#!/usr/bin/env bash
# macOS 官网分发门禁：Developer ID 预检，以及最终 DMG 的签名、公证和架构验证。
# 只读取环境变量，不保存或输出 Apple 私钥内容。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$ROOT" ]] || ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -n "$ROOT" ]] || { echo "找不到 MCPane 仓库" >&2; exit 1; }

ok()   { echo "  ✓ $*"; }
fail() { echo "  ✗ $*" >&2; exit 1; }

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "缺少命令：$1"
}

require_macos() {
  [[ "$(uname -s)" == "Darwin" ]] || fail "Apple 分发检查只能在 macOS 上运行"
}

require_apple_environment() {
  local name
  for name in \
    APPLE_SIGNING_IDENTITY \
    APPLE_API_ISSUER \
    APPLE_API_KEY \
    APPLE_API_KEY_PATH; do
    [[ -n "${!name:-}" ]] || fail "未设置 $name"
  done

  [[ -f "$APPLE_API_KEY_PATH" && -r "$APPLE_API_KEY_PATH" ]] \
    || fail "APPLE_API_KEY_PATH 不是可读文件"

  local key_mode
  key_mode="$(stat -f '%Lp' "$APPLE_API_KEY_PATH")"
  [[ "$key_mode" == "600" || "$key_mode" == "400" ]] \
    || fail "API Key 权限必须是 600 或 400（当前 $key_mode）"
}

team_id_from_identity() {
  if [[ "$APPLE_SIGNING_IDENTITY" =~ \(([A-Z0-9]{10})\)$ ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
  else
    fail "无法从 APPLE_SIGNING_IDENTITY 末尾解析 10 位 Team ID"
  fi
}

preflight() {
  require_macos

  local command_name
  for command_name in \
    basename codesign file find grep hdiutil lipo mktemp rmdir security sed shasum \
    spctl stat tee xcrun; do
    require_command "$command_name"
  done
  xcrun --find notarytool >/dev/null 2>&1 || fail "Xcode Command Line Tools 缺少 notarytool"
  xcrun --find stapler >/dev/null 2>&1 || fail "Xcode Command Line Tools 缺少 stapler"

  require_apple_environment

  if grep -Eq '"signingIdentity"[[:space:]]*:[[:space:]]*"-"' \
    "$ROOT/apps/desktop/src-tauri/tauri.conf.json"; then
    fail "tauri.conf.json 仍固定使用 ad-hoc 签名"
  fi

  local valid_identities
  valid_identities="$(security find-identity -v -p codesigning)"
  grep -Fq "$APPLE_SIGNING_IDENTITY" <<<"$valid_identities" \
    || fail "钥匙串没有匹配的有效 Developer ID identity"

  team_id_from_identity >/dev/null

  xcrun notarytool history \
    --key "$APPLE_API_KEY_PATH" \
    --key-id "$APPLE_API_KEY" \
    --issuer "$APPLE_API_ISSUER" \
    --output-format json >/dev/null 2>&1 \
    || fail "App Store Connect API Key 无法通过 notarytool 认证"

  ok "Developer ID identity 有效"
  ok "App Store Connect 公证认证有效"
  ok "Apple 分发工具与密钥权限有效"
}

verify_architecture() {
  local binary="$1" expected_arch="$2" actual_arches
  [[ -f "$binary" ]] || fail "缺少二进制：$binary"
  actual_arches="$(lipo -archs "$binary")"
  [[ "$actual_arches" == "$expected_arch" ]] \
    || fail "$(basename "$binary") 架构应为 $expected_arch，实际为 $actual_arches"
}

verify_dmg_signature() {
  local dmg="$1" team_id details
  team_id="$(team_id_from_identity)"
  codesign --verify --strict --verbose=4 "$dmg"
  details="$(codesign --display --verbose=4 "$dmg" 2>&1)"
  grep -Fq "Authority=$APPLE_SIGNING_IDENTITY" <<<"$details" \
    || fail "DMG 的 Developer ID identity 不匹配"
  grep -Fq "TeamIdentifier=$team_id" <<<"$details" \
    || fail "DMG 的 TeamIdentifier 不匹配"
  grep -Eq '^Timestamp=' <<<"$details" \
    || fail "DMG 缺少安全时间戳"
}

verify_app() {
  local app="$1" expected_arch="$2" team_id details item item_details
  team_id="$(team_id_from_identity)"

  [[ -d "$app" ]] || fail "DMG 内缺少 MCPane.app"
  codesign --verify --deep --strict --verbose=2 "$app"
  details="$(codesign --display --verbose=4 "$app" 2>&1)"
  grep -Fq "Authority=$APPLE_SIGNING_IDENTITY" <<<"$details" \
    || fail "MCPane.app 的 Developer ID identity 不匹配"
  grep -Fq "TeamIdentifier=$team_id" <<<"$details" \
    || fail "MCPane.app 的 TeamIdentifier 不匹配"
  grep -Eq '^CodeDirectory .*flags=.*runtime' <<<"$details" \
    || fail "MCPane.app 未启用 Hardened Runtime"
  grep -Eq '^Timestamp=' <<<"$details" \
    || fail "MCPane.app 缺少安全时间戳"
  spctl --assess --type execute --verbose=4 "$app"

  verify_architecture "$app/Contents/MacOS/mcpane" "$expected_arch"
  verify_architecture "$app/Contents/MacOS/mcpane-hub" "$expected_arch"

  while IFS= read -r -d '' item; do
    if [[ "$(file -b "$item")" == *Mach-O* ]]; then
      codesign --verify --strict --verbose=2 "$item"
      item_details="$(codesign --display --verbose=4 "$item" 2>&1)"
      grep -Fq "Authority=$APPLE_SIGNING_IDENTITY" <<<"$item_details" \
        || fail "$(basename "$item") 的 Developer ID identity 不匹配"
      grep -Fq "TeamIdentifier=$team_id" <<<"$item_details" \
        || fail "$(basename "$item") 的 TeamIdentifier 不匹配"
    fi
  done < <(find "$app" -type f -print0)
}

notarize_dmg() {
  local dmg="$1" submit_log submit_status submission_id
  [[ -f "$dmg" && -s "$dmg" ]] || fail "DMG 不存在或为空：$dmg"
  verify_dmg_signature "$dmg"

  submit_log="$(mktemp)"
  set +e
  xcrun notarytool submit "$dmg" \
    --key "$APPLE_API_KEY_PATH" \
    --key-id "$APPLE_API_KEY" \
    --issuer "$APPLE_API_ISSUER" \
    --wait \
    --timeout 30m 2>&1 | tee "$submit_log"
  submit_status="${PIPESTATUS[0]}"
  set -e

  if [[ "$submit_status" != "0" ]]; then
    # 上传成功后若仅轮询网络中断，notarytool 已打印 submission id。接回同一
    # 任务继续等待，避免重新上传一份内容相同的公证请求。
    submission_id="$(sed -nE \
      's/^[[:space:]]*id:[[:space:]]*([0-9a-f-]{36})[[:space:]]*$/\1/p' \
      "$submit_log")"
    submission_id="${submission_id%%$'\n'*}"
    [[ -n "$submission_id" ]] \
      || { rm -f "$submit_log"; fail "DMG 公证提交失败，且未获得 submission id"; }

    echo "==> 接回 Apple 公证任务 $submission_id"
    xcrun notarytool wait "$submission_id" \
      --key "$APPLE_API_KEY_PATH" \
      --key-id "$APPLE_API_KEY" \
      --issuer "$APPLE_API_ISSUER" \
      --timeout 30m
  fi
  rm -f "$submit_log"

  xcrun stapler staple "$dmg"
  xcrun stapler validate "$dmg"
  ok "$(basename "$dmg")：最终 DMG 公证并装订票据完成"
}

verify_dmg() {
  local dmg="$1" expected_arch="$2" mount_dir mounted app
  [[ -f "$dmg" && -s "$dmg" ]] || fail "DMG 不存在或为空：$dmg"

  verify_dmg_signature "$dmg"
  xcrun stapler validate "$dmg"
  spctl --assess --type open \
    --context context:primary-signature \
    --verbose=4 "$dmg"

  mount_dir="$(mktemp -d)"
  mounted=0
  cleanup_mount() {
    if [[ "$mounted" == "1" ]]; then
      hdiutil detach "$mount_dir" >/dev/null 2>&1 || true
    fi
    rmdir "$mount_dir" >/dev/null 2>&1 || true
  }
  trap cleanup_mount EXIT

  hdiutil attach "$dmg" -readonly -nobrowse -mountpoint "$mount_dir" >/dev/null
  mounted=1
  app="$mount_dir/MCPane.app"
  verify_app "$app" "$expected_arch"

  hdiutil detach "$mount_dir" >/dev/null
  mounted=0
  rmdir "$mount_dir"
  trap - EXIT

  ok "$(basename "$dmg")：Developer ID、公证票据、Gatekeeper 与 $expected_arch 架构通过"
  shasum -a 256 "$dmg" | sed 's/^/  SHA-256  /'
}

usage() {
  cat >&2 <<'EOF'
用法：
  scripts/macos-distribution.sh check
  scripts/macos-distribution.sh notarize <DMG路径>
  scripts/macos-distribution.sh verify <DMG路径> <arm64|x86_64>
EOF
  exit 2
}

case "${1:-}" in
  check)
    [[ "$#" == "1" ]] || usage
    preflight
    ;;
  notarize)
    [[ "$#" == "2" ]] || usage
    preflight
    notarize_dmg "$2"
    ;;
  verify)
    [[ "$#" == "3" ]] || usage
    [[ "$3" == "arm64" || "$3" == "x86_64" ]] || usage
    preflight
    verify_dmg "$2" "$3"
    ;;
  *)
    usage
    ;;
esac

#!/usr/bin/env bash
# 桌面版本号同步器。用法：
#   bump-version.sh          校验四处一致，打印当前版本（CI 守卫走这条）
#   bump-version.sh 0.2.0    改写四处再校验
#
# 为什么必须四处一起改：
#   tauri.conf.json 的 version 决定打包产物与 latest.json 里写的新版本号；
#   src-tauri/Cargo.toml 的 version 决定 env!("CARGO_PKG_VERSION")——正是
#   commands/updater.rs 拿去和远端比较的「当前版本」。两者一旦不等，用户会
#   陷入「装完更新仍提示有更新」的死循环，且这种错只在真发布后才暴露。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

TAURI_CONF="$ROOT/apps/desktop/src-tauri/tauri.conf.json"
PKG_JSON="$ROOT/apps/desktop/package.json"
SHELL_CARGO="$ROOT/apps/desktop/src-tauri/Cargo.toml"
WS_CARGO="$ROOT/Cargo.toml"

# JSON：取文件里第一个 "version"，即顶层字段（依赖版本都在更靠后的 deps 块里）
read_json() { grep -m1 '"version"' "$1" | sed 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/'; }

# TOML：限定在指定段内取 version，避开 [dependencies] 里的同名键
read_toml() {
  awk -v sec="$2" '
    $0 ~ "^\\[" sec "\\][[:space:]]*$" { inside = 1; next }
    /^\[/ { inside = 0 }
    inside && /^version[[:space:]]*=/ {
      sub(/^version[[:space:]]*=[[:space:]]*"/, ""); sub(/".*$/, ""); print; exit
    }
  ' "$1"
}

# 写入一律经环境变量传参给 perl，避免版本串进单引号脚本体导致的注入与转义地狱
write_json() { V="$2" perl -0pi -e 's/("version"\s*:\s*")[^"]*"/$1$ENV{V}"/' "$1"; }

# 段头之后、下一个行首 [ 之前的那个 version 才是目标（不以 [ 开头的行逐行吃）
write_toml() {
  SEC="$2" V="$3" perl -0pi -e '
    my $sec = quotemeta $ENV{SEC};
    s/(\[$sec\]\n(?:(?!\[)[^\n]*\n)*?version\s*=\s*")[^"]*"/$1$ENV{V}"/;
  ' "$1"
}

if [[ $# -gt 0 ]]; then
  NEW="$1"
  [[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || {
    echo "版本号不是合法 semver: $NEW" >&2
    exit 1
  }
  write_json "$TAURI_CONF" "$NEW"
  write_json "$PKG_JSON" "$NEW"
  write_toml "$SHELL_CARGO" "package" "$NEW"
  write_toml "$WS_CARGO" "workspace.package" "$NEW"
  # 两个 Cargo.lock 各自记着 workspace 成员的版本，不刷新就与 Cargo.toml 对不上，
  # CI 的 --locked 构建会直接失败。--workspace 只改成员自身的版本条目，
  # 第三方依赖一个不动（输出可见 "Locking 0 packages"）；--offline 保证它不会
  # 顺手去够网络索引。
  cargo update --workspace --offline --manifest-path "$WS_CARGO" >/dev/null
  cargo update --workspace --offline --manifest-path "$SHELL_CARGO" >/dev/null

  # ${} 不能省：紧跟其后的中文标点会被 bash 3.2（macOS 自带）算进变量名，
  # 在 set -u 下直接炸成 unbound variable
  echo "已写入 ${NEW}，正在校验……"
fi

V_CONF="$(read_json "$TAURI_CONF")"
V_PKG="$(read_json "$PKG_JSON")"
V_SHELL="$(read_toml "$SHELL_CARGO" "package")"
V_WS="$(read_toml "$WS_CARGO" "workspace.package")"

if [[ "$V_CONF" == "$V_PKG" && "$V_CONF" == "$V_SHELL" && "$V_CONF" == "$V_WS" ]]; then
  echo "版本号一致: $V_CONF"
  exit 0
fi

cat >&2 <<EOF
版本号不一致，updater 会因此误判「当前版本」：
  tauri.conf.json          $V_CONF
  apps/desktop/package.json      $V_PKG
  src-tauri/Cargo.toml           $V_SHELL
  Cargo.toml [workspace.package] $V_WS
用 scripts/bump-version.sh <版本号> 一并改写。
EOF
exit 1

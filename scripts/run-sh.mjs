#!/usr/bin/env node
// 跨平台入口：把 npm script 里的 `bash <script>` 调用改由这里转发。
//
// 触发原因（Windows 实测复现，2026-08-12）：Windows 10 1607+ 只要开了 WSL 功能，
// `C:\Windows\System32\bash.exe`（转发到 WSL 的桩）就会自动存在；而 Git for
// Windows 官方推荐安装选项**不**把 `Git\bin\bash.exe` 加进 PATH（只加
// `Git\cmd`，避免和系统工具冲突）。两者叠加时，裸 `bash script.sh` 会静默
// 解析到 WSL——脚本里的 `rustc`/`cargo` 在 WSL 侧通常没装，直接
// `command not found` 退出，`pnpm tauri dev`/`build` 全链路断在第一步。
// 这是 Git for Windows + WSL 共存这一常见组合下的默认失败模式，不是本机独有。
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import process from "node:process";

function resolveBash() {
  if (process.platform !== "win32") return "bash";
  const candidates = [
    process.env.MCPANE_GIT_BASH,
    process.env.ProgramFiles && `${process.env.ProgramFiles}\\Git\\bin\\bash.exe`,
    process.env["ProgramFiles(x86)"] && `${process.env["ProgramFiles(x86)"]}\\Git\\bin\\bash.exe`,
    process.env.ProgramW6432 && `${process.env.ProgramW6432}\\Git\\bin\\bash.exe`,
  ].filter(Boolean);
  const found = candidates.find((path) => existsSync(path));
  if (!found) {
    console.error(
      "[run-sh] 找不到 Git Bash（bash.exe）。请安装 Git for Windows（默认选项即可），" +
        "或设置 MCPANE_GIT_BASH 环境变量指向其 bash.exe 路径。",
    );
    process.exit(1);
  }
  return found;
}

const [scriptPath, ...scriptArgs] = process.argv.slice(2);
if (!scriptPath) {
  console.error("[run-sh] 用法: node run-sh.mjs <script.sh> [args...]");
  process.exit(1);
}

const result = spawnSync(resolveBash(), [scriptPath, ...scriptArgs], {
  stdio: "inherit",
});
if (result.error) {
  console.error(`[run-sh] 启动失败: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);

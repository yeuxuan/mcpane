/** 桌面端运行平台（决定使用说明里的 shell 命令语法）。 */
export type Platform = "macos" | "windows" | "linux";

export const PLATFORM_LABELS: Record<Platform, string> = {
  macos: "macOS",
  windows: "Windows",
  linux: "Linux",
};

/** 从 WebView UA 识别当前平台（Tauri WebView 与浏览器均适用）。 */
export function detectPlatform(): Platform {
  const ua = navigator.userAgent;
  if (ua.includes("Windows")) return "windows";
  if (ua.includes("Mac")) return "macos";
  return "linux";
}

/**
 * 各平台的 Token 环境变量设置命令（使用说明 §1 代码块）。
 *
 * 收 `t`：命令本身不翻译，但里面的 `#` 注释与 `<你的 Token>` 占位是给人读的，
 * 英文界面下跟着中文注释走等于没做 i18n。
 */
export function tokenEnvCommand(
  platform: Platform,
  tokenEnv: string,
  t: (key: string, options?: Record<string, unknown>) => string,
): string {
  const token = t("platform.cmd.tokenPlaceholder");
  switch (platform) {
    case "windows":
      return [
        `# ${t("platform.cmd.winPersist")}`,
        `setx ${tokenEnv} "${token}"`,
        "",
        `# ${t("platform.cmd.winSession")}`,
        `$env:${tokenEnv} = "${token}"`,
      ].join("\n");
    case "linux":
      return [
        `# ${t("platform.cmd.shellRc", { files: t("platform.cmd.rcOr") })}`,
        `export ${tokenEnv}="${token}"`,
        "",
        `# ${t("platform.cmd.reload")}`,
        "source ~/.bashrc",
      ].join("\n");
    default:
      return [
        `# ${t("platform.cmd.shellRc", { files: "~/.zshrc" })}`,
        `export ${tokenEnv}="${token}"`,
        "",
        `# ${t("platform.cmd.reload")}`,
        "source ~/.zshrc",
      ].join("\n");
  }
}

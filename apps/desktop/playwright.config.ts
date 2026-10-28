import { defineConfig, devices } from "@playwright/test";

/**
 * E2E（TASK-700 前端工作流）：针对 vite dev（浏览器环境）。
 * 无 Tauri runtime 时 IPC 调用失败，页面必须以错误/空状态优雅渲染——
 * 这本身就是被测行为之一。窗口级交互（托盘等）在打包后由发布验证覆盖。
 */
export default defineConfig({
  testDir: "./e2e",
  timeout: 30_000,
  retries: 0,
  use: {
    baseURL: "http://localhost:1420",
    // 动画禁用（走全局 prefers-reduced-motion 规则）：axe 扫描不再撞上进场动画的
    // 半透明中间态（否则 color-contrast 全线误报），测试也因此确定性。
    // 注意必须放 contextOptions 里——use 顶层的 reducedMotion 会被静默忽略。
    contextOptions: { reducedMotion: "reduce" },
  },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    /**
     * 导览在 WebKit 下再跑一遍。Tauri 在 macOS 上用的是系统 WKWebView，而它与
     * Chromium 的绘制差异是真的会咬人的：聚光灯最初用铺满视口的超大 box-shadow
     * 挖洞，Chromium 画得好好的，WebKit 里 computed style 一模一样却一片都不画，
     * 装成 app 才发现整屏根本没压暗。弹窗焦点循环同理：WebKit 默认 Tab 只走输入框、
     * Option+Tab 才走全部控件，Chromium 测不出来。只挑这两个 spec，避免全量用例翻倍。
     */
    {
      name: "webkit",
      testMatch: /(onboarding|modal-focus)\.spec\.ts/,
      use: { ...devices["Desktop Safari"] },
    },
  ],
  webServer: {
    command: "pnpm dev",
    url: "http://localhost:1420",
    reuseExistingServer: true,
    timeout: 60_000,
  },
});

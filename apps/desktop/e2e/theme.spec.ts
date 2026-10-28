import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "./support";

/**
 * 主题（浅色 / 深色 / 跟随系统）。
 *
 * 断言落在 `html[data-theme]` 上：它是整套换肤的开关——CSS 变量按它覆盖，
 * 组件全用语义 token，故这一个属性对了，整个界面就跟着对。
 * 原生标题栏同步无 Tauri runtime 时静默跳过（见 lib/theme.ts），不在此覆盖。
 */

async function openSettings(page: import("@playwright/test").Page) {
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "设置", exact: true })).toBeVisible();
}

/**
 * 主题的那组选项。必须限定在「外观」这一组里——设置页另有一处「跟随系统」
 * （出网代理），页面级选择器会同时命中两个。
 */
function themeOption(page: import("@playwright/test").Page, name: string) {
  return page.getByRole("group", { name: "外观" }).getByRole("radio", { name, exact: true });
}

test("默认跟随系统：系统暗色时界面即暗色", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

test("默认跟随系统：系统浅色时界面即浅色", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

test("显式选深色后即时生效，且刷新后仍在", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  await openSettings(page);

  await themeOption(page, "深色").click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

test("显式选择不被系统切换掀翻", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await openSettings(page);

  await themeOption(page, "浅色").click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

  // 系统转暗，但用户已显式选了浅色——不该被改。
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

test("选回跟随系统后重新随系统走", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  await openSettings(page);

  await themeOption(page, "深色").click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");

  await themeOption(page, "跟随系统").click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

test("暗色确实换了底色，而非只改了属性", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  const bodyBg = await page.evaluate(() =>
    getComputedStyle(document.body).getPropertyValue("background-color"),
  );
  expect(bodyBg).toBe("rgb(28, 28, 30)");
});

/**
 * 配置编辑器的语法色不能吃 CodeMirror 的 defaultHighlightStyle——那套色是为
 * 浅底硬编码的（string `#a11` 在暗底上仅约 2:1，数字 `#219` 约 1.3:1）。
 * axe 扫不到它（弹窗内 + 动态内容），故在此直接断言渲染出的实际颜色。
 */
test("配置编辑器语法色随主题切换，暗色下不沿用 CodeMirror 的浅底默认色", async ({ page }) => {
  // 读 CodeMirror 真实渲染出的颜色，而非 CSS 变量——变量对了不代表
  // HighlightStyle 生效（basicSetup 自带一份 defaultHighlightStyle）。
  const readStringColor = async (scheme: "light" | "dark") => {
    await page.emulateMedia({ colorScheme: scheme });
    await page.goto("/");
    await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
    await page
      .getByRole("button", { name: /添加 MCP 服务器/ })
      .first()
      .click();
    // 新建默认落在「官方预设」，编辑器在「导入 JSON」页签下。
    await page.getByRole("tab", { name: "导入 JSON" }).click();
    const editor = page.locator(".cm-editor").first();
    await editor.click();
    await page.keyboard.type('{"k":"probe"}');
    await expect(page.locator(".cm-line span").first()).toBeVisible();
    return page.locator(".cm-line span").evaluateAll((els) => {
      const target = els.find((el) => el.textContent === '"probe"');
      return target ? getComputedStyle(target).color : null;
    });
  };

  const dark = await readStringColor("dark");
  const light = await readStringColor("light");

  expect(dark).not.toBeNull();
  expect(light).not.toBe(dark);
  // defaultHighlightStyle 的 string 是 #a11 —— 在暗底上约 2:1，近乎隐形。
  expect(dark).not.toBe("rgb(170, 17, 17)");
  // 暗色下必须是亮色系（通道和足够高），否则再怎么"换了色"也读不出来。
  const channels = dark?.match(/\d+/g)?.map(Number) ?? [];
  expect(channels[0] + channels[1] + channels[2]).toBeGreaterThan(300);
});

/**
 * 暗色的对比度必须机器扫——挑暗色调色板时肉眼很容易把偏灰的文字看成"还行"。
 * app.spec.ts 那个 axe 扫描跑在浅色下，覆盖不到这套变量。
 */
test("a11y：暗色下 axe 扫描核心页面无 serious/critical 违规", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  for (const nav of ["概览", "MCP 服务器", "供应商", "客户端", "工作区", "用量", "日志", "设置"]) {
    await page.getByRole("button", { name: nav, exact: true }).click();
    const results = await new AxeBuilder({ page }).analyze();
    const severe = results.violations.filter((violation) =>
      ["serious", "critical"].includes(violation.impact ?? ""),
    );
    expect(
      severe.flatMap((violation) =>
        violation.nodes.map((node) => `${nav}: ${violation.id} :: ${node.html}`),
      ),
    ).toHaveLength(0);
  }

  // 设置页的分区默认停在「通用」，上面那圈只扫到它——「数据」分区（保留窗口条、
  // 备份列表、云同步表单）得自己点进去才在 DOM 里，否则等于没扫。
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("radio", { name: "数据", exact: true }).click();
  await expect(page.getByText("本机数据")).toBeVisible();
  const dataResults = await new AxeBuilder({ page }).analyze();
  expect(
    dataResults.violations
      .filter((violation) => ["serious", "critical"].includes(violation.impact ?? ""))
      .flatMap((violation) =>
        violation.nodes.map((node) => `设置/数据: ${violation.id} :: ${node.html}`),
      ),
  ).toHaveLength(0);

  // 供应商编辑器：控件最密的一页，且 --color-slot（输入凹槽/编辑器底）只在这里
  // 大面积出现——暗色下它比卡片更暗，落在上面的三级文字最容易掉出 AA。
  await page.getByRole("button", { name: "供应商", exact: true }).click();
  const addProvider = page.getByRole("button", { name: /添加供应商/ }).first();
  if (await addProvider.isVisible().catch(() => false)) {
    await addProvider.click();
    await expect(page.getByRole("main").getByText("未命名供应商")).toBeVisible();
    const results = await new AxeBuilder({ page }).analyze();
    const severe = results.violations.filter((violation) =>
      ["serious", "critical"].includes(violation.impact ?? ""),
    );
    expect(
      severe.flatMap((violation) =>
        violation.nodes.map((node) => `供应商编辑器: ${violation.id} :: ${node.html}`),
      ),
    ).toHaveLength(0);
  }
});

import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";

/**
 * 首启向导：迁移 + 分流。它排在九套导览前面（导览见 onboarding.spec.ts）。
 *
 * 与那边同理，直接用 @playwright/test 而不走 support.ts——那个 fixture 关掉了
 * 自动触发，而这里要测的正是"什么时候自己出来、什么时候不再出来"。
 */

const TOURS_KEY = "mcpane:tours-v1";

const BASE_FIXTURES = {
  workspaces_list: [{ id: "ws_default", name: "默认工作区" }],
  servers_list: [],
  hub_status: {
    name: "mcpane-hub",
    serverVersion: "0.1.0",
    protocolVersion: "2026-07-28",
    legacyProtocolVersion: "2025-11-25",
    endpoint: "http://127.0.0.1:24860/mcp",
    uptimeSecs: 42,
    connections: [],
  },
  hub_diagnostics: { catalogTools: 0, downstreams: {} },
  catalog_list: { tools: [] },
  // 概览页按客户端逐个读 installed，给空对象会让它整页崩掉（向导也就无从显示）。
  detect_clients: {
    claude: {
      installed: true,
      version: "2.1.207",
      connected: true,
      configPath: "/Users/dev/.claude.json",
      tokenEnv: "MCPANE_CLAUDE_TOKEN",
    },
    codex: {
      installed: true,
      version: "0.133.0",
      connected: false,
      configPath: "/Users/dev/.codex/config.toml",
      tokenEnv: "MCPANE_CODEX_TOKEN",
    },
  },
  mcp_clients_status: [],
  providers_list: [],
  provider_current: {
    claude: null,
    codex: null,
    gemini: null,
    grok: null,
    opencode: null,
    openclaw: null,
    hermes: null,
    "claude-desktop": null,
  },
  provider_clients_installed: {
    claude: true,
    codex: true,
    gemini: false,
    grok: false,
    opencode: false,
    openclaw: false,
    hermes: false,
    "claude-desktop": false,
  },
  provider_live_ids: [],
  custom_mcp_clients_list: [],
  provider_config_dirs_get: {},
  ccswitch_detect: { searched: "/Users/dev/.cc-switch", found: null },
};

const FOUND = {
  searched: "/Users/dev/.cc-switch",
  found: {
    path: "/Users/dev/.cc-switch/cc-switch.db",
    providerCount: 3,
    serverCount: 1,
    apps: [{ app: "claude", count: 3 }],
  },
};

const IMPORTED = {
  providers: { imported: ["中转甲", "中转乙"], skipped: [], failed: [] },
  servers: { imported: ["context7"], skipped: [], warnings: [], incomplete: [] },
  warnings: [],
};

function mock(extra: Record<string, unknown> = {}) {
  const fixtures = { ...BASE_FIXTURES, ...extra };
  return `
    const fixtures = ${JSON.stringify(fixtures)};
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd) => {
        if (cmd in fixtures) return structuredClone(fixtures[cmd]);
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `;
}

const wizard = (page: Page) => page.getByRole("dialog", { name: "MCPane 首次设置" });
const tour = (page: Page) => page.getByRole("dialog", { name: "MCPane 使用引导" });
const next = (page: Page) => page.getByRole("button", { name: "下一步" }).click();

/** 全新 profile：什么都不预置，向导自己出来。 */
async function goto(page: Page, extra: Record<string, unknown> = {}) {
  await page.addInitScript(mock(extra));
  await page.goto("/");
  await expect(wizard(page)).toBeVisible();
}

test("首启先出向导，导览不抢在它前面", async ({ page }) => {
  await goto(page);
  await expect(page.getByRole("heading", { name: "欢迎使用 MCPane" })).toBeVisible();
  // 总览导览也会自动开，此刻不该抢在向导前面（两者的 dialog 可访问名不同）。
  await expect(tour(page)).toBeHidden();
});

test("没装 cc-switch：那一步整个不出现，直接到选路", async ({ page }) => {
  await goto(page);
  await expect(page.getByLabel("第 1 步，共 2 步")).toBeVisible();
  await next(page);
  // 没得搬的人不该被塞一屏「没找到」加一行看不懂的路径。
  await expect(page.getByText("未检测到 cc-switch 配置")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: "从哪里开始？" })).toBeVisible();
});

test("装了 cc-switch：报条数、导入、就地给结果", async ({ page }) => {
  await goto(page, { ccswitch_detect: FOUND, ccswitch_import: IMPORTED });
  await next(page);

  await expect(page.getByText("检测到 3 个供应商、1 个 MCP 服务，可以导入")).toBeVisible();
  await expect(page.getByText("Claude Code：3 个")).toBeVisible();
  await page.getByRole("button", { name: "导入", exact: true }).click();

  await expect(page.getByText("已导入 2 个供应商、1 个 MCP 服务")).toBeVisible();
  // 导完就不该还能再点一次。
  await expect(page.getByRole("button", { name: "导入", exact: true })).toHaveCount(0);
});

/**
 * 分流是这次改动的重点：选完直接落到那一页，并接上那一页自己的导览——
 * 而不是从六站的总览讲起。两条路各测一遍，漏一条等于分流只做了一半。
 */
const ROUTES = [
  { pick: "配置 AI 供应商", heading: "供应商", tourFirst: "一个 AI 工具配一份" },
  { pick: "管理 MCP 服务", heading: "MCP 服务器", tourFirst: "三种添加方式" },
] as const;

for (const { pick, heading, tourFirst } of ROUTES) {
  test(`分流：选「${pick}」直达该页并接上它的导览`, async ({ page }) => {
    await goto(page);
    await next(page);
    await page.getByRole("button", { name: new RegExp(pick) }).click();

    await expect(wizard(page)).toBeHidden();
    await expect(page.getByRole("heading", { name: heading, exact: true })).toBeVisible();
    // 页面导览随即开讲，且不是总览那六站。
    await expect(page.getByRole("heading", { name: tourFirst })).toBeVisible();
  });
}

test("选「先浏览整体界面」才走总览导览", async ({ page }) => {
  await goto(page);
  await next(page);
  await page.getByRole("button", { name: "先浏览整体界面" }).click();

  await expect(wizard(page)).toBeHidden();
  await expect(tour(page)).toBeVisible();
  // 这句是总览导览自己的开场，与向导那句「欢迎使用 MCPane」不是同一处。
  await expect(page.getByRole("heading", { name: "一处配好，处处能用" })).toBeVisible();
});

test("走完就不再出现：刷新回来是常态界面", async ({ page }) => {
  await goto(page);
  await next(page);
  await page.getByRole("button", { name: /配置 AI 供应商/ }).click();
  await expect(wizard(page)).toBeHidden();

  await page.reload();
  await expect(wizard(page)).toBeHidden();
});

test("跳过也算走过，且不再弹", async ({ page }) => {
  await goto(page);
  await page.getByRole("button", { name: "跳过" }).click();
  await expect(wizard(page)).toBeHidden();
  await page.reload();
  await expect(wizard(page)).toBeHidden();
});

test("ESC 随时退出", async ({ page }) => {
  await goto(page);
  await page.keyboard.press("Escape");
  await expect(wizard(page)).toBeHidden();
});

test("升级上来的老用户不会被向导拦一道", async ({ page }) => {
  await page.addInitScript(`
    try {
      localStorage.setItem(
        ${JSON.stringify(TOURS_KEY)},
        JSON.stringify({ v: 1, tours: { overview: "*" }, autoStart: true }),
      );
    } catch {}
    ${mock()}
  `);
  await page.goto("/");
  await expect(wizard(page)).toBeHidden();
});

/**
 * 向导只跑一次，跳过了就再也遇不到迁移入口——所以设置页必须常驻同一块面板。
 * 这条守的是"入口还在"，面板自身的行为由 WelcomeWizard.test.tsx 覆盖。
 */
test("设置页「数据」里常驻同一个迁移入口", async ({ page }) => {
  await goto(page, { ccswitch_detect: FOUND, ccswitch_import: IMPORTED });
  await page.getByRole("button", { name: "跳过" }).click();
  await expect(wizard(page)).toBeHidden();

  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("radio", { name: "数据", exact: true }).click();
  await expect(page.getByText("从 cc-switch 导入")).toBeVisible();
  await expect(page.getByText("检测到 3 个供应商、1 个 MCP 服务，可以导入")).toBeVisible();
  await expect(page.getByRole("button", { name: "导入", exact: true })).toBeVisible();
});

test("a11y：向导三步 axe 无 serious/critical 违规", async ({ page }) => {
  await goto(page, { ccswitch_detect: FOUND, ccswitch_import: IMPORTED });
  for (let i = 0; i < 3; i++) {
    await expect(wizard(page)).toBeVisible();
    const result = await new AxeBuilder({ page }).include('[role="dialog"]').analyze();
    const severe = result.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(severe, `第 ${i + 1} 步存在违规: ${JSON.stringify(severe, null, 2)}`).toEqual([]);
    // 第二步往下走的那颗在未导入时叫「暂不导入，下一步」（见 MigrateStep 的注释）。
    if (i === 0) await next(page);
    if (i === 1) await page.getByRole("button", { name: "暂不导入，下一步" }).click();
  }
});

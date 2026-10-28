import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, test } from "@playwright/test";

/**
 * 九套导览：首启的总览，加八套进页面才触发的。
 *
 * 唯一直接用 @playwright/test 而不走 support.ts 的 spec：那个 fixture 关掉了自动触发，
 * 而这里要测的恰恰是导览自己什么时候出现、什么时候不再出现。
 * 设置 E2E_SHOT_DIR 环境变量可同时输出界面截图。
 */

/** 与 lib/onboarding.ts 的 STORAGE_KEY / SCHEMA 保持一致。 */
const TOURS_KEY = "mcpane:tours-v1";

const FIXTURES = {
  workspaces_list: [{ id: "ws_default", name: "默认工作区" }],
  servers_list: [
    {
      id: "srv_ctx7",
      name: "context7",
      slug: "context7",
      transport: "http",
      command: null,
      args: [],
      url: "https://mcp.context7.com/mcp",
      enabled: true,
    },
  ],
  hub_status: {
    name: "mcpane-hub",
    serverVersion: "0.1.0",
    protocolVersion: "2026-07-28",
    legacyProtocolVersion: "2025-11-25",
    endpoint: "http://127.0.0.1:24860/mcp",
    uptimeSecs: 42,
    connections: [],
  },
  hub_diagnostics: { catalogTools: 12, downstreams: {} },
  catalog_list: { tools: [] },
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
  mcp_clients_status: [
    {
      id: "cursor",
      display: "Cursor",
      installed: true,
      connected: false,
      tokenEnv: "MCPANE_CURSOR_TOKEN",
      configPath: "/Users/dev/.cursor/mcp.json",
    },
  ],
  // 供应商页会被导览带到，页面自己要能正常渲染——不 mock 的话它满屏报错，断言仍然过
  // （只看气泡），但截出来的图不是用户会看到的样子。
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
  // 设置页「数据」分区那张迁移卡会探一次；这些用例不测迁移，给个"没装"就够。
  ccswitch_detect: { searched: "/Users/dev/.cc-switch", found: null },
};

const MOCK = `
  const fixtures = ${JSON.stringify(FIXTURES)};
  window.__TAURI_INTERNALS__ = {
    transformCallback: (cb) => cb,
    invoke: async (cmd) => {
      if (cmd in fixtures) return structuredClone(fixtures[cmd]);
      throw new Error("e2e 未 mock 的命令: " + cmd);
    },
  };
`;

/**
 * 预置进度：`{overview: "*"}` 表示总览已走完，页面导览才会开始触发。
 * 一律带上 `wizard: true`——首启向导排在导览前面，不标记它就会挡在最前面
 *（向导本身由 welcome.spec.ts 负责）。
 */
function seed(progress: Record<string, string>) {
  return `
    try {
      localStorage.setItem(
        ${JSON.stringify(TOURS_KEY)},
        JSON.stringify({ v: 1, tours: ${JSON.stringify(progress)}, autoStart: true, wizard: true }),
      );
    } catch {}
  `;
}

const dialog = (page: Page) => page.getByRole("dialog", { name: "MCPane 使用引导" });

/** 打开应用；全新 profile 下总览导览自动开始。 */
async function gotoTour(page: Page) {
  await page.addInitScript(`${seed({})}\n${MOCK}`);
  await page.goto("/");
  await expect(dialog(page)).toBeVisible();
}

/** 打开应用，总览已走完——页面导览由此才会自动触发。 */
async function gotoWithOverviewDone(page: Page) {
  await page.addInitScript(`${seed({ overview: "*" })}\n${MOCK}`);
  await page.goto("/");
}

/** 九套全部看过：验证"讲完之后"的常态界面时用，免得导览自己弹出来挡路。 */
async function gotoWithAllToursDone(page: Page) {
  const done = Object.fromEntries(
    [
      "overview",
      "servers",
      "providers",
      "clients",
      "projects",
      "workspaces",
      "usage",
      "logs",
      "settings",
    ].map((id) => [id, "*"]),
  );
  await page.addInitScript(`${seed(done)}\n${MOCK}`);
  await page.goto("/");
}

async function shot(page: Page, name: string) {
  const dir = process.env.E2E_SHOT_DIR;
  if (!dir) return;
  // 等两帧再拍。紧跟断言拍到的是换站那一瞬：遮罩的洞看着还停在上一站的位置，
  // 像是有残留没清掉——1×1 像素解码测过，稳定态是对的，纯属截图时机。
  // 只影响截图，断言不走这里。
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r))),
  );
  await page.screenshot({ path: `${dir}/${name}.png` });
}

const next = (page: Page) => page.getByRole("button", { name: "下一步" }).click();
/** 聚光灯挖洞的那个框；没有目标的站（开场/收尾）不渲染它。 */
const spotlight = (page: Page) => page.locator(".tour-ring");

/** 走完当前这一套：一路点下一步，直到最后一站的「开始使用」。 */
async function finishTour(page: Page, stops: number) {
  for (let i = 0; i < stops - 1; i++) await next(page);
  await page.getByRole("button", { name: "开始使用" }).click();
  await expect(dialog(page)).toBeHidden();
}

test("总览导览：首启自动开始，六站走完不再自动弹", async ({ page }) => {
  await gotoTour(page);

  // 开场：无目标，整屏压暗 + 居中说明 + 那张说明产品是什么的收束图。
  await expect(page.getByRole("heading", { name: "一处配好，处处能用" })).toBeVisible();
  await expect(page.locator(".tour-scrim")).toBeVisible();
  await expect(page.locator(".tour-diagram")).toBeVisible();
  await expect(spotlight(page)).toHaveCount(0);
  await shot(page, "overview-1-intro");

  await next(page);
  await expect(page.getByRole("heading", { name: "本页展示运行状态" })).toBeVisible();
  await expect(spotlight(page)).toBeVisible();

  await next(page);
  await expect(page.getByRole("heading", { name: "左侧导航的七个区域" })).toBeVisible();
  await shot(page, "overview-3-nav");

  await next(page);
  await expect(page.getByRole("heading", { name: "工作区相互独立" })).toBeVisible();

  await next(page);
  await expect(page.getByRole("heading", { name: "绿色表示运行正常" })).toBeVisible();

  // 收尾：不是流水账，而是能接着做的三步。
  await next(page);
  await expect(page.getByRole("heading", { name: "完成以下三步即可开始使用" })).toBeVisible();
  await expect(page.locator(".tour-bubble").getByRole("listitem")).toHaveCount(3);
  await shot(page, "overview-6-outro");

  await page.getByRole("button", { name: "开始使用" }).click();
  await expect(dialog(page)).toBeHidden();
  await page.reload();
  await expect(dialog(page)).toBeHidden();
});

/**
 * 拆开之后最关键的一条：进某一页，那一页的导览要自己出来。侧栏七页在这里循环验证，
 * 不在侧栏的工作区页紧随其后单测——漏掉一套等于那块功能又回到"自己摸索"。
 */
const PAGE_TOURS = [
  { nav: "MCP 服务器", first: "三种添加方式", stops: 3, shot: "servers" },
  { nav: "供应商", first: "一个 AI 工具配一份", stops: 4, shot: "providers" },
  { nav: "客户端", first: "客户端状态一览", stops: 3, shot: "clients" },
  { nav: "工作区", first: "一个工作区一套工具", stops: 3, shot: "projects" },
  { nav: "用量", first: "选择时间范围", stops: 3, shot: "usage" },
  { nav: "日志", first: "操作记录与变更历史", stops: 3, shot: "logs" },
  { nav: "设置", first: "设置分四组", stops: 1, shot: "settings" },
] as const;

for (const { nav, first, stops, shot: name } of PAGE_TOURS) {
  test(`页面导览：首次进入「${nav}」自动开始，走完不再弹`, async ({ page }) => {
    await gotoWithOverviewDone(page);
    await page.getByRole("button", { name: nav, exact: true }).click();

    await expect(page.getByRole("heading", { name: first })).toBeVisible();
    await expect(page.locator(".tour-bubble")).toBeVisible();
    await shot(page, `tour-${name}`);

    await finishTour(page, stops);

    // 再进一次不该重来。先去别处再回来，确保走的是"进入页面"这条路径。
    await page.getByRole("button", { name: "概览", exact: true }).click();
    await page.getByRole("button", { name: nav, exact: true }).click();
    await expect(dialog(page)).toBeHidden();
  });
}

/**
 * 工作区页不在侧栏，从顶部切换器的「管理」进来。单独测一条：它的进入路径与其余七套
 * 不同，套用上面那个循环会漏掉——而这一页管的恰恰是工具级停用与别名，最需要讲。
 */
test("页面导览：从工作区切换器进管理页，导览自动开始", async ({ page }) => {
  await gotoWithOverviewDone(page);
  await page.getByRole("button", { name: "切换工作区" }).click();
  await page.getByRole("menuitem", { name: "管理工作区…" }).click();

  await expect(page.getByRole("heading", { name: "一个工作区一套服务" })).toBeVisible();
  await shot(page, "tour-workspaces");
  await finishTour(page, 3);
});

test("总览没走完时，页面导览不插播", async ({ page }) => {
  await gotoTour(page);
  await expect(page.getByRole("heading", { name: "一处配好，处处能用" })).toBeVisible();
  // 跳过总览——跳过也算看过，但没走完。
  await page.getByRole("button", { name: "跳过" }).click();
  await expect(dialog(page)).toBeHidden();

  await page.getByRole("button", { name: "日志", exact: true }).click();
  await expect(dialog(page)).toBeHidden();
});

/**
 * 重看引导**只有设置页一个入口**。
 *
 * 页头那颗指南针按钮与设置页那张九套导览的管理卡都不许回来：留着它们，引导就从
 * "顺手带一程"变成了一个要用户去配置的功能。缺的只是"想再看一遍时有路可走"，
 * 所以补的也只有一个动作按钮。
 */
test("重看引导只有设置页一个入口", async ({ page }) => {
  // 九套都标记看过：这一条查的是"讲完之后界面上还剩什么"，不该被自动弹出的导览挡路。
  await gotoWithAllToursDone(page);

  // 按具体文案断言不够——文案一改，负向断言就永远为真却谁也不知道。所以主力是这条
  // 正则：凡是名字里带"引导/导览"的可点控件，一个都不许有。
  const anyTourControl = page.getByRole("button", { name: /引导|导览/ });
  const anyTourSwitch = page.getByRole("switch", { name: /引导|导览/ });

  // 页头那颗指南针按钮不该在——九页逐个确认，漏掉哪一页都等于入口还在。
  for (const nav of ["概览", "MCP 服务器", "供应商", "客户端", "工作区", "用量", "日志", "设置"]) {
    await page.getByRole("button", { name: nav, exact: true }).click();
    await expect(page.getByRole("heading").first()).toBeVisible();
    await expect(anyTourControl).toHaveCount(0);
  }
  await page.getByRole("button", { name: "切换工作区" }).click();
  await page.getByRole("menuitem", { name: "管理工作区…" }).click();
  await expect(page.getByRole("heading", { name: "工作区设置" }).first()).toBeVisible();
  await expect(anyTourControl).toHaveCount(0);

  // 设置页：恰好一个「重新运行」，且「不再自动显示引导」那个开关没有回来。
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "设置", exact: true })).toBeVisible();
  await expect(page.getByText("使用引导")).toHaveCount(1);
  await expect(page.getByRole("button", { name: "重新运行" })).toHaveCount(1);
  await expect(anyTourSwitch).toHaveCount(0);
});

/** 点重新运行就从头来：向导先出来，而不是直接跳回某一套导览。 */
test("设置页重新运行：向导从头再来一遍", async ({ page }) => {
  await gotoWithAllToursDone(page);
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("button", { name: "重新运行" }).click();

  const wizard = page.getByRole("dialog", { name: "MCPane 首次设置" });
  await expect(wizard).toBeVisible();
  await expect(page.getByRole("heading", { name: "欢迎使用 MCPane" })).toBeVisible();
});

/**
 * 重跑之后页面导览也要真的再讲一遍。
 *
 * 清进度是不够的：Provider 里还有一份"本次挂载已自动开过哪几套"的集合，不一并
 * 清掉，重跑就只剩一个向导，八套页面导览一套都不会再出来——而且刷新之前都不会。
 */
test("重新运行之后，页面导览也会再讲一遍", async ({ page }) => {
  // 必须让日志那一套在**本次挂载内真的自动开过一次**，那份集合才会被填上——
  // 用"九套都看过"的起点，导览根本不会触发，也就验不到这件事。
  await gotoWithOverviewDone(page);
  await page.getByRole("button", { name: "日志", exact: true }).click();
  await expect(page.getByRole("heading", { name: "操作记录与变更历史" })).toBeVisible();
  await finishTour(page, 3);

  // 中途不刷新：整条路径都在同一次挂载内，考的就是那份内存集合有没有跟着清。
  await page.getByRole("button", { name: "设置", exact: true }).click();
  // 设置页的导览是第一次进，会自动开；不等它收掉，遮罩会和下一次点击抢。
  await expect(dialog(page)).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog(page)).toBeHidden();
  await page.getByRole("button", { name: "重新运行" }).click();
  await page.getByRole("button", { name: "跳过" }).click();

  await page.getByRole("button", { name: "日志", exact: true }).click();
  await expect(page.getByRole("heading", { name: "操作记录与变更历史" })).toBeVisible();
  await expect(dialog(page)).toBeVisible();
});

test("中途走人算看过：刷新回来不再弹", async ({ page }) => {
  await gotoWithOverviewDone(page);
  await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
  await expect(page.getByRole("heading", { name: "三种添加方式" })).toBeVisible();
  await next(page);
  await expect(page.getByRole("heading", { name: "接管已有的 MCP 服务" })).toBeVisible();

  // 直接刷新走人。三站的东西，下次更想要的是"别再弹"，而不是"从第三站接着讲"。
  await page.reload();
  await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
  await expect(dialog(page)).toBeHidden();
});

test("随时能退出：ESC 收工，且不再自动开始", async ({ page }) => {
  await gotoTour(page);
  await next(page);
  await page.keyboard.press("Escape");
  await expect(dialog(page)).toBeHidden();

  await page.reload();
  await expect(dialog(page)).toBeHidden();
});

/**
 * 这条守的是一次真实事故：遮罩最初靠高亮框上一圈 9999px 的 box-shadow 铺开，
 * Chromium 画得出来，WKWebView（Tauri 在 macOS 上的引擎）computed style 一模一样
 * 却一片都不画——装成 app 才发现整屏没压暗。所以断言"遮罩元素自己就铺满视口"，
 * 任何"靠影子铺开"的写法都过不了这一关；洞则必须挖在这块遮罩上。
 *
 * **这是 webkit 项目里唯一走完整挖洞路径的用例**，改动它之前先想清楚：拆成八套之后
 * 总览的头一站是没有目标的，只测那一站等于把这道防线关掉了。
 */
test("遮罩自己就铺满视口，洞挖在它身上", async ({ page }) => {
  await gotoTour(page);
  const vp = page.viewportSize();
  if (!vp) throw new Error("拿不到视口尺寸");

  const scrim = await page.locator(".tour-scrim").boundingBox();
  if (!scrim) throw new Error("遮罩没渲染");
  expect(scrim.width).toBeGreaterThanOrEqual(vp.width - 1);
  expect(scrim.height).toBeGreaterThanOrEqual(vp.height - 1);

  // 走到有目标的站：遮罩仍然铺满，洞的坐标与描边框一致（两者共用插值坐标）。
  await next(page);
  await expect(spotlight(page)).toBeVisible();
  const stillFull = await page.locator(".tour-scrim").boundingBox();
  if (!stillFull) throw new Error("遮罩没渲染");
  expect(stillFull.width).toBeGreaterThanOrEqual(vp.width - 1);

  const ring = await spotlight(page).boundingBox();
  const hole = await page.locator(".tour-scrim mask rect[rx]").boundingBox();
  if (!ring || !hole) throw new Error("测不到洞或描边");
  expect(Math.abs(hole.x - ring.x)).toBeLessThan(2);
  expect(Math.abs(hole.y - ring.y)).toBeLessThan(2);
  expect(Math.abs(hole.width - ring.width)).toBeLessThan(2);
  expect(Math.abs(hole.height - ring.height)).toBeLessThan(2);
});

test("聚光灯对准的是真实控件", async ({ page }) => {
  await gotoWithOverviewDone(page);
  await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
  await expect(page.getByRole("heading", { name: "三种添加方式" })).toBeVisible();

  const addBtn = page.locator('[data-tour="servers-add"]');
  const addBox = await addBtn.boundingBox();
  const ringBox = await spotlight(page).boundingBox();
  if (!addBox || !ringBox) throw new Error("测不到位置");
  // 高亮框外扩 8px 包住目标，中心应当与目标重合（容 2px 抖动）。
  expect(Math.abs(ringBox.x + ringBox.width / 2 - (addBox.x + addBox.width / 2))).toBeLessThan(2);
  expect(Math.abs(ringBox.y + ringBox.height / 2 - (addBox.y + addBox.height / 2))).toBeLessThan(2);
});

/**
 * 空列表的网格容器宽度满格、高度为零。只判 `width === 0 && height === 0` 会放它过关，
 * 于是画出一条 16px 高的缝。这一条守的就是那个判定。
 */
test("列表为空时不挖出一条缝，而是退回居中说明", async ({ page }) => {
  await page.addInitScript(`
    ${seed({ overview: "*" })}
    const fixtures = ${JSON.stringify({ ...FIXTURES, servers_list: [] })};
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd) => {
        if (cmd in fixtures) return structuredClone(fixtures[cmd]);
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
  await expect(page.getByRole("heading", { name: "三种添加方式" })).toBeVisible();
  await next(page);
  await next(page);

  // 第三站指的是列表首项，此刻一张卡都没有：该降级成居中说明，而不是在空容器上
  // 挖一条 16px 高的缝。一个洞都不画，正是这条要守的结果。
  await expect(page.getByRole("heading", { name: "一个服务一行" })).toBeVisible();
  await expect(spotlight(page)).toHaveCount(0);
});

test("逐行入场跑完，每一行都真的显现出来", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await gotoTour(page);

  const lines = page.locator(".tour-bubble > .tour-line");
  expect(await lines.count()).toBeGreaterThan(3);

  // 不用固定 waitForTimeout：按「最后一行延迟 200ms + 时长 260ms」算，600ms
  // 本地绰绰有余，但 CI 上（Linux WebKit、runner 负载高）动画起步本身就会被
  // 推迟，断言会抓到一条还在播的递降阶梯（实测 0.84/0.66/0.45/0.23/0.02）。
  // 改为轮询终态：某行若真的永远卡在半透明，这里会一直不满足直到超时，仍然会红。
  await expect
    .poll(
      () =>
        lines.evaluateAll((els) =>
          els.map((el) => Number(getComputedStyle(el).opacity)).filter((o) => o < 1),
        ),
      { message: "逐行入场结束后不该还有半透明的行", timeout: 5000 },
    )
    .toEqual([]);
});

test("reduce 下逐行入场整个停掉，内容不会卡在透明", async ({ page }) => {
  // 项目全局就是 reduce（见 playwright.config.ts），这里不必再 emulate。
  await gotoTour(page);
  const opacities = await page
    .locator(".tour-bubble > .tour-line")
    .evaluateAll((els) => els.map((el) => Number(getComputedStyle(el).opacity)));
  expect(opacities.length).toBeGreaterThan(3);
  expect(opacities.filter((o) => o < 1)).toEqual([]);
});

test("a11y：总览各站 axe 无 serious/critical 违规", async ({ page }) => {
  await gotoTour(page);
  for (let i = 0; i < 6; i++) {
    // 每站都要等气泡真的挂上再扫：切站有动画，机器忙时 axe 会赶在气泡挂载前
    // 跑起来，直接报 "No elements found for include"（暗色那个用例同理）。
    await expect(page.locator(".tour-bubble")).toBeVisible();
    // 限定气泡内（与其他弹层用例同惯例）：背后那一页由 app.spec 的整页扫描负责。
    const result = await new AxeBuilder({ page }).include(".tour-bubble").analyze();
    const severe = result.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(severe, `第 ${i + 1} 站存在违规: ${JSON.stringify(severe, null, 2)}`).toEqual([]);
    if (i < 5) await next(page);
  }
});

test("a11y：暗色下页面导览无 serious/critical 违规", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await gotoWithOverviewDone(page);
  await page.getByRole("button", { name: "供应商", exact: true }).click();
  // 自动触发有 500ms 去抖，不等它出来 axe 会扫不到 .tour-bubble 直接报错。
  await expect(page.getByRole("heading", { name: "一个 AI 工具配一份" })).toBeVisible();
  for (let i = 0; i < 4; i++) {
    await expect(page.locator(".tour-bubble")).toBeVisible();
    const result = await new AxeBuilder({ page }).include(".tour-bubble").analyze();
    const severe = result.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(severe, `第 ${i + 1} 站（暗色）存在违规: ${JSON.stringify(severe, null, 2)}`).toEqual(
      [],
    );
    if (i < 3) await next(page);
  }
  await shot(page, "tour-dark");
});

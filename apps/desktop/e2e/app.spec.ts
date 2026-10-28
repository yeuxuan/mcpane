import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "./support";

const PAGES: Array<{ nav: string; heading: string }> = [
  { nav: "概览", heading: "概览" },
  { nav: "MCP 服务器", heading: "MCP 服务器" },
  { nav: "供应商", heading: "供应商" },
  { nav: "客户端", heading: "客户端" },
  { nav: "工作区", heading: "工作区" },
  { nav: "用量", heading: "用量统计" },
  { nav: "日志", heading: "日志" },
  { nav: "设置", heading: "设置" },
];

test("应用壳加载：品牌、导航、Hub 状态兜底", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "MCPane" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "主导航" })).toBeVisible();
  // 无 Tauri runtime：Hub 状态必须以"未运行"兜底而非崩溃。
  await expect(page.getByText("MCP Hub 未运行", { exact: true })).toBeVisible();
});

test("侧栏页面全部可导航且渲染标题（IPC 失败不崩溃）", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(String(error)));

  await page.goto("/");
  for (const { nav, heading } of PAGES) {
    await page.getByRole("button", { name: nav, exact: true }).click();
    await expect(page.getByRole("heading", { name: heading, exact: true })).toBeVisible();
  }
  expect(errors, `页面级 JS 错误: ${errors.join("; ")}`).toHaveLength(0);
});

test("键盘导航：Tab 经切换器达侧栏并以 Enter 切换页面", async ({ page }) => {
  await page.goto("/");
  // 顶右工作区切换器是首个 Tab 焦点，随后是命令面板入口，再进入侧栏主导航
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "切换工作区" })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "快速操作" })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(page.getByRole("button", { name: "概览" })).toBeFocused();
  await page.keyboard.press("Tab");
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: "MCP 服务器", exact: true })).toBeVisible();
});

test("a11y：axe 扫描核心页面无 serious/critical 违规", async ({ page }) => {
  await page.goto("/");
  for (const { nav } of PAGES) {
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

  /*
   * 设置页的四个分区里，上面的循环只扫得到默认停留的那一个。「数据」分区的
   * 保留窗口条、备份列表、云同步表单都要点进去才渲染——不单独走一趟等于没扫。
   */
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

  /*
   * 供应商编辑器单独扫一遍：它是页内面板，上面的循环只走到列表页就停了，
   * 而这一页的控件密度（预设网格、三段表单、折叠段）比任何列表页都高——
   * 底色/文字色配得不对，只有在这里才撞得出来。
   */
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

/**
 * 上面那趟扫的全是「没有 Tauri runtime」的失败态：概览页的 Hub 卡永远停在离线分支，
 * 在线态那块端点铭牌 axe 一次也没看过——而它是全页字号最大、唯一坐在凹槽底上的元素，
 * 恰恰最容易配出不达标的文字色（faint 落在 --color-slot 上只有 4.47:1，就是这么栽的）。
 * 注入 mock 把卡片翻到在线态再扫，浅色暗色各一遍：凹槽在两套主题里是两个不同的值。
 */
const hubOnlineMock = (claudeVersion: string | null = "2.1.207") => `
  const fixtures = {
    workspaces_list: [{ id: "ws_default", name: "默认工作区" }],
    servers_list: [],
    hub_status: {
      name: "mcpane-hub", serverVersion: "0.1.0", protocolVersion: "2026-07-28", legacyProtocolVersion: "2025-11-25",
      endpoint: "http://127.0.0.1:24860/mcp", uptimeSecs: 5023, connections: [],
    },
    hub_diagnostics: { catalogTools: 51, downstreams: {}, workspaces: {} },
    detect_clients: {
      claude: { installed: true, version: ${JSON.stringify(claudeVersion)}, connected: true, configPath: "", tokenEnv: "" },
      codex: { installed: false, version: null, connected: false, configPath: "", tokenEnv: "" },
    },
    hub_process_state: { state: "managed", pid: 1 },
  };
  window.__TAURI_INTERNALS__ = {
    transformCallback: (cb) => cb,
    invoke: async (cmd) => {
      if (cmd in fixtures) return structuredClone(fixtures[cmd]);
      throw new Error("e2e 未 mock 的命令: " + cmd);
    },
  };
`;

test("a11y：Hub 在线时的概览页在浅色/暗色下均无 serious/critical 违规", async ({ page }) => {
  await page.addInitScript(hubOnlineMock());
  for (const colorScheme of ["light", "dark"] as const) {
    // 主题偏好默认 system，故直接改系统偏好即可翻主题，无需写 localStorage。
    await page.emulateMedia({ colorScheme });
    await page.goto("/");
    await expect(page.getByText("http://127.0.0.1:24860/mcp")).toBeVisible();
    const results = await new AxeBuilder({ page }).analyze();
    const severe = results.violations.filter((violation) =>
      ["serious", "critical"].includes(violation.impact ?? ""),
    );
    expect(
      severe.flatMap((violation) =>
        violation.nodes.map((node) => `概览/在线/${colorScheme}: ${violation.id} :: ${node.html}`),
      ),
    ).toHaveLength(0);
  }
});

// 装在非常规位置的客户端跑不出 `--version`，概览页此前把占位符渲染成
// 「已安装 v?」——那个问号看着像故障。现在客户端卡只报连没连，版本归客户端页。
test("概览页：客户端卡只报连接状态，不渲染版本占位", async ({ page }) => {
  await page.addInitScript(hubOnlineMock(null));
  await page.goto("/");
  await expect(page.getByRole("button", { name: "管理" }).first()).toBeVisible();
  await expect(page.getByText("Claude Code").first()).toBeVisible();
  await expect(page.getByText(/v\?/)).toHaveCount(0);
});

test("语言切换：英文后导航变为 English 标签并持久化", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "设置" }).click();
  await page.getByRole("radio", { name: "English" }).click();
  await expect(page.getByRole("button", { name: "Dashboard" })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("button", { name: "Dashboard" })).toBeVisible();
  // 还原
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("radio", { name: "简体中文" }).click();
  await expect(page.getByRole("button", { name: "概览" })).toBeVisible();
});

/**
 * i18n 的真守卫：切到英文后，**页面正文**里不该再有汉字。
 *
 * 上面那条只断言了导航标签——这正是 6 个页面零 `t()` 调用却长期没人发现的原因：
 * 切到 English，导航变了、正文纹丝不动，而测试全绿。一个已发布的功能半残着，
 * 比没有这个功能更难看。
 *
 * 断言粒度是「整页无汉字」而非逐句比对：逐句会变成 change-detector，
 * 而这里要守的红线只有一条——别再漏掉未走 i18n 的文案。
 */
test("语言切换：英文后各页正文不再残留中文", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "设置" }).click();
  await page.getByRole("radio", { name: "English" }).click();

  const NAV_EN = [
    "Dashboard",
    "Clients",
    "Providers",
    "MCP Servers",
    "Workspaces",
    "Usage",
    "Logs",
    "Settings",
  ];
  const leaked: string[] = [];
  // 「工作区设置」明细页不在侧栏（经页头切换器进入），此前被这条守卫漏扫——
  // 整页残留中文都不会红。经真实入口导航后并入同一轮扫描。
  const openWorkspaces = async () => {
    await page.getByRole("button", { name: "Switch workspace" }).click();
    await page.getByRole("menuitem", { name: "Manage workspaces…" }).click();
  };
  for (const nav of [...NAV_EN, "Workspace Settings"]) {
    if (nav === "Workspace Settings") {
      await openWorkspaces();
    } else {
      await page.getByRole("button", { name: nav, exact: true }).click();
    }
    // 语言切换器上的「简体中文」是 endonym，必须保持中文——译成
    // "Simplified Chinese" 反而让中文用户找不到自己那项。故排除该组。
    const text = await page.locator("main").evaluate((main) => {
      const clone = main.cloneNode(true) as HTMLElement;
      for (const group of clone.querySelectorAll('fieldset[aria-label="Language"]')) {
        group.remove();
      }
      return clone.innerText;
    });
    const hits = text.match(/[一-鿿]+/g);
    if (hits) leaked.push(`${nav}: ${[...new Set(hits)].slice(0, 8).join(" / ")}`);
  }

  // 设置页有四个分区，上面那一趟只看得到默认停留的「通用」——另外三组（含
  // 数据分区的保留窗口、备份、云同步）不点进去就永远扫不到。
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  for (const section of ["General", "Environment", "Data", "About"]) {
    await page.getByRole("radio", { name: section, exact: true }).click();
    const text = await page.locator("main").evaluate((main) => {
      const clone = main.cloneNode(true) as HTMLElement;
      for (const group of clone.querySelectorAll('fieldset[aria-label="Language"]')) {
        group.remove();
      }
      return clone.innerText;
    });
    const hits = text.match(/[一-鿿]+/g);
    if (hits) leaked.push(`Settings/${section}: ${[...new Set(hits)].slice(0, 8).join(" / ")}`);
  }

  expect(leaked, "切到英文后仍有中文残留（未走 i18n 的文案）").toEqual([]);
});

/**
 * 弹窗层的同一条红线。上面那条只扫页面正文——弹窗要点开才渲染，扫不到，
 * 而弹窗恰恰是文案最密集的地方（连接弹窗一个就有 70 多处）。
 */
test("语言切换：英文后弹窗内也不再残留中文", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "设置" }).click();
  await page.getByRole("radio", { name: "English" }).click();

  const leaked: string[] = [];
  /**
   * @param label 报错时标明这段文案来自哪个界面
   * @param scope 被扫区域：弹窗传 dialog，页内面板（供应商编辑器）传 main
   */
  const scan = async (label: string, scope: "dialog" | "main" = "dialog") => {
    // 供应商预设名（阿里云百炼 / 火山 …）是品牌专有名词，不翻译——它同时是
    // 用户配置里的标识。与语言切换器的 endonym 同类，故跳过那一片。
    const target = scope === "dialog" ? page.getByRole("dialog") : page.getByRole("main");
    const text = await target.evaluate((node) => {
      const clone = node.cloneNode(true) as HTMLElement;
      for (const grid of clone.querySelectorAll("[data-preset-names]")) grid.remove();
      return clone.innerText;
    });
    const hits = text.match(/[一-鿿]+/g);
    if (hits) leaked.push(`${label}: ${[...new Set(hits)].slice(0, 8).join(" / ")}`);
  };

  // 添加 MCP 服务器：四个页签逐个扫（文案最多的一处）。
  await page.getByRole("button", { name: "MCP Servers", exact: true }).click();
  await page.getByRole("button", { name: /Add MCP server/i }).click();
  for (const tab of ["Official presets", "Import JSON", "Create manually", "Remote server"]) {
    await page.getByRole("tab", { name: tab }).click();
    await scan(`ServerSetupModal/${tab}`);
  }
  await page.keyboard.press("Escape");

  // 供应商编辑器：页内面板（不是弹窗），扫主内容区，退出走「返回列表」。
  // 入口用硬断言而非 isVisible 软跳过：按钮改名/渲染失败时软跳过会让
  // 测试静默全绿而实际一格没扫——那是最坏的「看起来有守卫」。
  await page.getByRole("button", { name: "Providers", exact: true }).click();
  const addProvider = page.getByRole("button", { name: /Add provider/i }).first();
  await expect(addProvider, "Providers 页找不到新增入口，弹窗扫描失去覆盖").toBeVisible();
  await addProvider.click();
  await scan("ProviderEditor", "main");
  await page.getByRole("button", { name: /Back to list/i }).click();

  // 使用说明：文案最密的一处，且含 platform.ts 生成的 shell 命令注释。
  await page.getByRole("button", { name: "Clients", exact: true }).click();
  // 使用说明收进了行尾「…」菜单（Radix DropdownMenu，Portal 到 body）。
  const more = page.getByRole("button", { name: /More actions/i }).first();
  await expect(more, "Clients 页找不到行尾菜单，弹窗扫描失去覆盖").toBeVisible();
  await more.click();
  const howTo = page.getByRole("menuitem", { name: /How to use/i });
  await expect(howTo, "行尾菜单里找不到使用说明，弹窗扫描失去覆盖").toBeVisible();
  await howTo.click();
  await scan("HowToUseModal");
  await page.keyboard.press("Escape");

  expect(leaked, "切到英文后弹窗里仍有中文残留").toEqual([]);
});

/**
 * 外壳级回归：内容区里的绝对定位后代不得把窗口撑出滚动条。
 *
 * 日志表每行的结果列带一个 `sr-only`（Tailwind 的 `position: absolute`）。main 当初
 * 不是定位祖先，它们就以初始包含块（视口）为准落位——不随 main 滚动，却照样计入
 * html 的 scrollHeight。500 行时 html 被撑到一万七千多像素，窗口冒出一条极短的
 * 滚动条，一滚就把整个外壳（侧栏一起）推出视口。
 *
 * 断言放在 html 上而不是「找 sr-only」：守的是「窗口永不滚动」这条外壳不变量，
 * 将来换成别的绝对定位元素同样兜得住。
 */
test("外壳：日志长表不把窗口撑出滚动条", async ({ page }) => {
  const now = Math.floor(Date.now() / 1000);
  await page.addInitScript(`
    try { localStorage.setItem("mcpane.page", "logs"); } catch {}
    const entries = Array.from({ length: 500 }, (_, i) => ({
      id: "log_" + i, ts: ${now} - i * 60, requestId: "req_" + i,
      clientConnectionId: null, sessionId: null, workspaceId: "ws_default",
      serverId: null, exposedName: "web_search_exa", method: "tools/call",
      durationMs: 1200, success: true, errorCategory: null,
      requestSize: null, responseSize: null, retryCount: 0,
    }));
    const fixtures = {
      logs_recent: entries,
      workspaces_list: [{ id: "ws_default", name: "默认工作区" }],
      servers_list: [],
    };
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd) => {
        if (cmd in fixtures) return structuredClone(fixtures[cmd]);
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  // 等表格真渲染出来：空表撑不出问题，断言会假绿。
  await expect(page.getByText("web_search_exa").first()).toBeVisible();

  const doc = await page.evaluate(() => ({
    scrollHeight: document.documentElement.scrollHeight,
    clientHeight: document.documentElement.clientHeight,
    mainScrollHeight: document.querySelector("main")?.scrollHeight ?? 0,
  }));
  expect(doc.scrollHeight, "html 被撑高：内容区里有绝对定位元素落到了视口坐标系").toBe(
    doc.clientHeight,
  );
  // 反面：滚动本该发生在 main 内部，长表必须真的溢出它——否则上一条是空断言。
  expect(doc.mainScrollHeight).toBeGreaterThan(doc.clientHeight);
});

import { expect, expectNoSevereA11yViolations, type Page, test } from "./support";

/**
 * 设置页「数据」分区（mock Tauri IPC）。
 *
 * 必须 mock：不注入 `window.__TAURI_INTERNALS__` 时 `data_overview` 直接失败，
 * 卡片走的是"读不到统计"那条分支——保留窗口条、带数量的清理按钮、备份列表
 * 一个都不渲染。app.spec.ts 里那趟 axe/i18n 扫描扫的正是这个空壳，
 * 真正有内容的形态得在这里扫。
 */

const DAY = 86400;
const NOW = Math.floor(Date.now() / 1000);

const BASE = {
  // 落地页（Dashboard）先于设置页挂载，缺这几项它会抛错，整个 App 白屏。
  detect_clients: {
    claude: {
      installed: true,
      version: "2.1.207",
      connected: true,
      configPath: "~/.claude.json",
      tokenEnv: "MCPANE_CLAUDE_TOKEN",
    },
    codex: {
      installed: false,
      version: null,
      connected: false,
      configPath: "~/.codex/config.toml",
      tokenEnv: "MCPANE_CODEX_TOKEN",
    },
  },
  hub_status: { name: "mcpane-hub", version: "0.1.0", running: false, connections: [] },
  hub_enabled_get: true,
  hub_process_state: { state: "managed" },
  autostart_get: false,
  auto_update_check_get: true,
  provider_clients_installed: {},
  workspaces_list: [{ id: "default", name: "Default", isDefault: true }],
  servers_list: [],
  mcp_clients_status: [],
};

const SYNC_OFF = {
  provider: "off",
  autoSync: false,
  webdav: { baseUrl: "", username: "", remoteRoot: "", hasPassword: false },
  s3: {
    region: "",
    bucket: "",
    accessKeyId: "",
    endpoint: "",
    remoteRoot: "",
    hasSecretKey: false,
  },
  status: {
    lastSyncAt: null,
    lastSnapshotId: null,
    lastDeviceName: null,
    lastError: null,
    lastErrorSource: null,
  },
};

/** 有货的一屏：日志有超期数据可清，用量全在保留期内。 */
const POPULATED = {
  ...BASE,
  data_overview: {
    dbBytes: 2_517_842,
    logs: { rows: 1284, oldestTs: NOW - 46 * DAY, prunable: 312, retainDays: 30 },
    usage: { rows: 4193, oldestTs: NOW - 61 * DAY, prunable: 0, retainDays: 90 },
  },
  backup_list: [
    { fileName: `mcpane-backup-${NOW - 3600}.db`, sizeBytes: 2_431_002, createdAt: NOW - 3600 },
    { fileName: `mcpane-backup-${NOW - DAY}.db`, sizeBytes: 2_402_110, createdAt: NOW - DAY },
  ],
  sync_settings_get: {
    ...SYNC_OFF,
    provider: "webdav",
    autoSync: true,
    webdav: {
      baseUrl: "https://dav.example.com/dav/mcpane",
      username: "user",
      remoteRoot: "mcpane-sync",
      hasPassword: true,
    },
    status: { ...SYNC_OFF.status, lastSyncAt: NOW - 7200, lastDeviceName: "this-mac" },
  },
};

/** 全新安装：库是空的，一份备份都没有，同步没配。 */
const FRESH = {
  ...BASE,
  data_overview: {
    dbBytes: 96_000,
    logs: { rows: 0, oldestTs: null, prunable: 0, retainDays: 30 },
    usage: { rows: 0, oldestTs: null, prunable: 0, retainDays: 90 },
  },
  backup_list: [],
  sync_settings_get: SYNC_OFF,
};

async function gotoData(page: Page, fixtures: Record<string, unknown>) {
  await page.addInitScript(`
    const fixtures = ${JSON.stringify(fixtures)};
    // 设置页监听 hub-process-state；事件插件的 unlisten 走这个内部对象。
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    window.__ipcCalls = {};
    // 落库调用要能被用例回看：「选关闭有没有真的存进去」只能从这里断言。
    window.__syncSet = [];
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd, args) => {
        window.__ipcCalls[cmd] = (window.__ipcCalls[cmd] ?? 0) + 1;
        if (cmd === "sync_settings_set") {
          window.__syncSet.push(structuredClone(args.input));
          // 落库失败路径：fixtures 里挂 __failSyncSet 即可让这条命令报错。
          if (fixtures.__failSyncSet) throw new Error("写库失败");
          return structuredClone(fixtures.sync_settings_get);
        }
        if (cmd in fixtures) return structuredClone(fixtures[cmd]);
        if (String(cmd).startsWith("plugin:")) return 0;
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("radio", { name: "数据", exact: true }).click();
  await expect(page.getByText("本机数据")).toBeVisible();

  /*
   * 等两个查询真的回来、并且 React 把这批数据渲染完，用例才能开始断言。
   *
   * 这一步不能省：数据到达之前，同步表单用的是 RHF 的 defaultValues（provider
   * 恰好是 "off"），那一瞬间的界面和"正确结果"长得一模一样——Playwright 的
   * 断言只要成立过一次就通过，于是会在这个窗口里绿掉，随后数据到达把界面改坏，
   * 测试却已经跑完了。provider 回空串那个 bug 起初就是这样从守卫底下溜过去的。
   */
  await page.waitForFunction(() => {
    const calls = (window as unknown as { __ipcCalls?: Record<string, number> }).__ipcCalls ?? {};
    return (calls.data_overview ?? 0) > 0 && (calls.sync_settings_get ?? 0) > 0;
  });
  // 查询已 resolve ≠ 已上屏：留一帧余量给 React 提交这次渲染。
  await page.waitForTimeout(250);
}

test("保留窗口：条数与保留期来自后端，清理按钮写明会删多少", async ({ page }) => {
  await gotoData(page, POPULATED);

  // 天数是后端回传的（启动自动清理同一常量），不是前端写死的 7/90。
  await expect(page.getByText("1284 条 · 保留最近 30 天")).toBeVisible();
  await expect(page.getByText("4193 条 · 保留最近 90 天")).toBeVisible();

  // 点下去会删掉多少，写在按钮上——这是这次重设计的核心：动作不再是盲的。
  await expect(page.getByRole("button", { name: "清理 312 条" })).toBeEnabled();
  // 全部都在保留期内时不给一个点了没反应的按钮，而是标灰说明原因。
  await expect(page.getByRole("button", { name: "无需清理" })).toBeDisabled();
});

test("空库：不谎报 0 条可清，给出「还没有记录」与备份空态", async ({ page }) => {
  await gotoData(page, FRESH);

  await expect(page.getByText("还没有记录")).toHaveCount(2);
  await expect(page.getByRole("button", { name: "无需清理" })).toHaveCount(2);
  // 没有备份时此前整块列表消失，用户看不出备份是会攒着的——空态要在。
  await expect(page.getByText("还没有备份")).toBeVisible();
  await expect(page.getByText("应用每 24 小时自动备份一次，只保留最近 10 份")).toBeVisible();
});

test("云同步：抬头说明报出最近同步时间，自动上传是开关而非裸复选框", async ({ page }) => {
  await gotoData(page, POPULATED);

  await expect(page.getByText(/最近同步 \d/)).toBeVisible();
  await expect(
    page.getByRole("switch", { name: "数据变更后自动上传（不会自动下载）" }),
  ).toBeChecked();
});

/**
 * 选「关闭」时这张卡必须是一张完整的单行设置卡（图标 + 标题 + 说明 + 右侧选择器），
 * 与「语言」「外观」同构。此前分段器单独占一行、下面空无一物，看着像内容没加载出来。
 */
test("云同步关闭时：卡片收成一行，不留悬空的分段器与空白", async ({ page }) => {
  await gotoData(page, FRESH);

  await expect(page.getByRole("radio", { name: "关闭" })).toBeChecked();
  // 关着时讲功能是什么，不显示"最近同步/还没有同步过"这类只在开着时才有意义的状态。
  await expect(page.getByText(/将配置数据同步到 WebDAV/)).toBeVisible();
  await expect(page.getByText("还没有同步过")).toHaveCount(0);
  // 配置区整体不渲染——分隔线也不画，否则会拉出一条通往空白的线。
  await expect(page.getByPlaceholder("https://dav.jianguoyun.com/dav/mcpane")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "从云端恢复" })).toHaveCount(0);

  // 选中 WebDAV 后配置区就地展开，抬头说明换成同步状态。
  await page.getByRole("radio", { name: "WebDAV" }).click();
  await expect(page.getByPlaceholder("https://dav.jianguoyun.com/dav/mcpane")).toBeVisible();
  await expect(page.getByText("还没有同步过")).toBeVisible();
});

/**
 * 回归：后端 `SyncSettings` 是 `#[derive(Default)]`，全新安装时 provider 是**空串**
 * 而不是 "off"。空串既不等于 "off"（配置区于是当作已启用展开）、又不匹配分段器的
 * 任何一项（于是一项都不高亮）——界面变成"展开着、没有字段、没有选中项"，
 * 用户完全读不出当前是开是关。后端已在出口收敛，前端再兜一道。
 */
test("provider 是未知值（空串）时退化成关闭，而不是无选中项的半开卡片", async ({ page }) => {
  await gotoData(page, {
    ...FRESH,
    sync_settings_get: { ...SYNC_OFF, provider: "" },
  });

  await expect(page.getByRole("radio", { name: "关闭" })).toBeChecked();
  await expect(page.getByRole("button", { name: "从云端恢复" })).toHaveCount(0);
});

/**
 * 回归：保存按钮只在选了 WebDAV/S3 时才渲染，所以选「关闭」若不就地落库就永远
 * 存不进去——界面看着已经关了（配置区都收起来了），库里还是 WebDAV，自动同步
 * 照跑，下次打开设置又变回去。
 */
test("选「关闭」就地落库，不靠一个此时并不存在的保存按钮", async ({ page }) => {
  await gotoData(page, POPULATED);
  await expect(page.getByRole("radio", { name: "WebDAV" })).toBeChecked();

  await page.getByRole("radio", { name: "关闭" }).click();

  await expect(page.getByText("已保存")).toBeVisible();
  const saved = await page.evaluate(
    () => (window as unknown as { __syncSet?: unknown[] }).__syncSet ?? [],
  );
  expect(saved).toHaveLength(1);
  expect((saved[0] as { provider: string }).provider).toBe("off");
  // 原有配置原样带上，切回来时不用重填。
  expect((saved[0] as { webdav: { baseUrl: string } }).webdav.baseUrl).toBe(
    "https://dav.example.com/dav/mcpane",
  );
});

test("选「关闭」落库失败时，选择器拨回真实值而不是停在「关闭」", async ({ page }) => {
  await gotoData(page, { ...POPULATED, __failSyncSet: true });

  await page.getByRole("radio", { name: "关闭" }).click();

  // 报错要给，但选中项也必须回到库里的真实值——否则界面说关了、实际还开着。
  await expect(page.getByText(/操作失败/)).toBeVisible();
  await expect(page.getByRole("radio", { name: "WebDAV" })).toBeChecked();
  await expect(page.getByRole("button", { name: "从云端恢复" })).toBeVisible();
});

/**
 * 通用守卫：分段器是全应用共用的单选控件，而它在 value 落到 options 之外时
 * 只是"什么都不高亮"——没有报错、没有兜底、扫一眼看不出坏了。所以这里直接
 * 断言这一页的每个分段器都恰有一项被选中。
 */
test("数据分区里每个分段器都恰有一项选中", async ({ page }) => {
  await gotoData(page, POPULATED);

  const groups = page.getByRole("main").locator("fieldset");
  const count = await groups.count();
  expect(count).toBeGreaterThan(0);
  for (let i = 0; i < count; i += 1) {
    const group = groups.nth(i);
    const label = await group.getAttribute("aria-label");
    await expect(group.locator("input[type=radio]:checked"), `分段器「${label}」`).toHaveCount(1);
  }
});

test("a11y：数据分区（有内容）axe 无 serious/critical 违规", async ({ page }) => {
  await gotoData(page, POPULATED);
  await expectNoSevereA11yViolations(page, "设置/数据");
});

/**
 * 删除备份的确认框。
 *
 * 这个框是后加的——此前删除备份一点即删，而同一张卡里的「恢复」反倒有确认，
 * 保护级别正好装反（恢复挑错了还能再恢复一次，删掉的备份文件找不回来）。
 * 既然是新加的一层拦截，它的可达性就得有守卫盯着，别只验证了「拦得住」。
 */
for (const scheme of ["light", "dark"] as const) {
  test(`a11y：删除备份确认框在${scheme === "dark" ? "暗色" : "浅色"}下无 serious/critical 违规`, async ({
    page,
  }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await gotoData(page, POPULATED);
    await page
      .getByRole("button", { name: /删除 .* 的备份/ })
      .first()
      .click();
    const confirm = page.getByRole("dialog", { name: "删除备份" });
    await expect(confirm).toBeVisible();
    // 确认文案必须点明不可逆，否则这层拦截只是多一次点击。
    await expect(confirm.getByText("删除后无法找回", { exact: false })).toBeVisible();
    await expectNoSevereA11yViolations(page, `${scheme}/删除备份确认`, '[role="dialog"]');
  });
}

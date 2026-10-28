import { expect, expectNoSevereA11yViolations, type Page, test } from "./support";

/**
 * 客户端页（mock Tauri IPC）：卡片三态、连接对比弹窗、凭证下沉、使用说明。
 * 通过注入 window.__TAURI_INTERNALS__ 模拟后端，覆盖浏览器 E2E 无法触达的成功路径。
 * 设置 E2E_SHOT_DIR 环境变量可同时输出界面截图。
 */

// 模拟真实的长配置：mcpane 之外还有大段无关内容，用于验证折叠渲染。
const CODEX_BEFORE = `notify = ["/Applications/Some.app/Contents/MacOS/Notifier", "turn-ended"]
model = "gpt-x"
model_reasoning_effort = "high"
service_tier = "default"

[mcp_servers.context7]
url = "https://mcp.context7.com/mcp"
startup_timeout_sec = 120
enabled = true

[mcp_servers.other_tool]
command = "npx"
args = ["-y", "@some/cli", "connect"]
enabled = true
`;
// env 默认模式：只写环境变量引用，Token 不落盘（ADR-015）。
const CODEX_AFTER_ENV = `${CODEX_BEFORE}
[mcp_servers.mcpane]
url = "http://127.0.0.1:24860/mcp"
bearer_token_env_var = "MCPANE_CODEX_TOKEN"
enabled = true
required = false
`;
// inline 可选模式：预览用占位串，apply 时替换为真实 Token。
const CODEX_AFTER_INLINE = `${CODEX_BEFORE}
[mcp_servers.mcpane]
url = "http://127.0.0.1:24860/mcp"
enabled = true
required = false

[mcp_servers.mcpane.http_headers]
Authorization = "Bearer <连接时自动发放>"
`;

/** 以字符串内嵌到 addInitScript，保持纯可序列化数据。 */
const FIXTURES = {
  workspaces_list: [
    { id: "ws_default", name: "默认工作区" },
    { id: "ws_web", name: "Web 项目" },
  ],
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
    {
      id: "vscode",
      display: "VS Code",
      installed: true,
      connected: true,
      tokenEnv: "MCPANE_VSCODE_TOKEN",
      configPath: "/Users/dev/Library/Application Support/Code/User/mcp.json",
    },
    {
      id: "cline",
      display: "Cline",
      installed: false,
      connected: false,
      tokenEnv: null,
      configPath: "/Users/dev/.../cline_mcp_settings.json",
    },
    {
      id: "devin",
      display: "Devin",
      installed: true,
      connected: false,
      tokenEnv: "MCPANE_DEVIN_TOKEN",
      configPath: "/Users/dev/.codeium/windsurf/mcp_config.json",
    },
    {
      // Trae：国内客户端里唯一不支持环境变量展开的（tokenEnv 为 null）。
      id: "trae",
      display: "Trae",
      installed: true,
      connected: false,
      tokenEnv: null,
      configPath: "/Users/dev/Library/Application Support/Trae CN/User/mcp.json",
    },
    {
      id: "qwen-code",
      display: "Qwen Code",
      installed: true,
      connected: false,
      tokenEnv: "MCPANE_QWEN_TOKEN",
      configPath: "/Users/dev/.qwen/settings.json",
    },
    {
      // ZCode：唯一一家 Server 表挂在嵌套 mcp.servers 下的，同样不支持 env 展开。
      id: "zcode",
      display: "ZCode",
      installed: true,
      connected: false,
      tokenEnv: null,
      configPath: "/Users/dev/.zcode/cli/config.json",
    },
  ],
  hub_status: {
    name: "mcpane-hub",
    serverVersion: "0.1.0",
    protocolVersion: "2026-07-28",
    legacyProtocolVersion: "2025-11-25",
    endpoint: "http://127.0.0.1:24860/mcp",
    uptimeSecs: 42,
    connections: [
      {
        clientConnectionId: "conn_dev_claude",
        clientType: "claude",
        tokenPrefix: "1f3e4025",
        enabled: true,
        defaultWorkspaceId: "ws_web",
        toolExposureMode: "auto",
      },
      {
        clientConnectionId: "conn_dev_codex",
        clientType: "codex",
        tokenPrefix: "1c308558",
        enabled: true,
        defaultWorkspaceId: null,
        toolExposureMode: "gateway",
      },
    ],
  },
  // env 默认模式的预览/写入结果（一键：Token 由 envsync 自动持久化，ADR-016）。
  codex_connect_preview: {
    targetPath: "/Users/dev/.codex/config.toml",
    unifiedDiff: "",
    beforeText: CODEX_BEFORE,
    afterText: CODEX_AFTER_ENV,
    credentialMode: "env",
    envVar: "MCPANE_CODEX_TOKEN",
  },
  codex_connect_apply: {
    targetPath: "/Users/dev/.codex/config.toml",
    unifiedDiff: "",
    snapshotId: "snap_20260711_001",
    credentialMode: "env",
    tokenPrefix: "9a1b2c3d",
    nextSteps: [
      "Token 已自动发放并保存到 /Users/dev/Library/Application Support/MCPane/env.sh（仅本机当前用户可读）",
      "已写入 /Users/dev/.zshrc，新打开的终端自动生效；已打开的终端需重新打开",
      "GUI 应用环境已同步，之后启动的应用即可读取",
      "在新打开的终端中重启 Codex 即可使用，无需其他手动步骤",
    ],
  },
  // inline 可选模式（mock 按 args.mode === "inline" 分发）。
  codex_connect_preview_inline: {
    targetPath: "/Users/dev/.codex/config.toml",
    unifiedDiff: "",
    beforeText: CODEX_BEFORE,
    afterText: CODEX_AFTER_INLINE,
    credentialMode: "inline",
    envVar: null,
  },
  codex_connect_apply_inline: {
    targetPath: "/Users/dev/.codex/config.toml",
    unifiedDiff: "",
    snapshotId: "snap_20260711_001",
    credentialMode: "inline",
    tokenPrefix: "9a1b2c3d",
    nextSteps: ["Token 已签发并写入配置，重启 Codex 即可使用，无需其他步骤"],
  },
  // 删除凭证：永久移除，区别于撤销（可逆开关）。
  connection_delete: { ok: true },
  // 轮换联动：env 形态由 envsync 重写环境变量存储，不弹一次性明文。
  connection_rotate_sync: {
    tokenPrefix: "77aa88bb",
    token: null,
    synced: null,
    envSynced: {
      credentialStore: "/Users/dev/Library/Application Support/MCPane/env.sh",
      rcFiles: ["/Users/dev/.zshrc"],
      guiSession: "synced_now",
      persistenceAgent: "unchanged",
      warnings: [],
    },
  },
};

/** client_config_snippet 按客户端返回（mock 里按 args.client 分发）。 */
const SNIPPETS: Record<string, string> = {
  claude: `{\n  "mcpServers": {\n    "mcpane": {\n      "type": "http",\n      "url": "http://127.0.0.1:24860/mcp",\n      "headers": { "Authorization": "Bearer \${MCPANE_CLAUDE_TOKEN}" }\n    }\n  }\n}`,
  cline: `{\n  "mcpServers": {\n    "mcpane": {\n      "type": "streamableHttp",\n      "url": "http://127.0.0.1:24860/mcp",\n      "headers": { "Authorization": "Bearer <在卡片「凭证」里发放的 Token>" }\n    }\n  }\n}`,
  zcode: `{\n  "mcp": {\n    "servers": {\n      "mcpane": {\n        "type": "http",\n        "url": "http://127.0.0.1:24860/mcp",\n        "headers": { "Authorization": "Bearer <在卡片「凭证」里发放的 Token>" }\n      }\n    }\n  }\n}`,
};

/** `overrides` 按命令名覆盖 fixture（整条替换，不深合并）。 */
async function gotoClients(page: Page, overrides: Record<string, unknown> = {}) {
  await page.addInitScript(`
    const fixtures = ${JSON.stringify({ ...FIXTURES, ...overrides })};
    const snippets = ${JSON.stringify(SNIPPETS)};
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd, args) => {
        if (cmd === "client_config_snippet") return snippets[args.client] ?? "{}";
        // 凭据模式按 mode 分发（env 默认 / inline 可选）。
        if (cmd === "codex_connect_preview" && args && args.mode === "inline")
          return structuredClone(fixtures.codex_connect_preview_inline);
        if (cmd === "codex_connect_apply" && args && args.mode === "inline")
          return structuredClone(fixtures.codex_connect_apply_inline);
        // 暴露模式写进 fixture：onSettled 会重取 hub_status，mock 不记住变更
        // 的话乐观高亮会被旧值冲回去（ADR-020）。
        if (cmd === "connection_set_tool_exposure") {
          const target = fixtures.hub_status.connections.find(
            (c) => c.clientConnectionId === args.clientConnectionId,
          );
          if (target) target.toolExposureMode = args.mode;
          return null;
        }
        if (cmd in fixtures) return structuredClone(fixtures[cmd]);
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  await page.getByRole("button", { name: "客户端", exact: true }).click();
  await expect(page.getByRole("heading", { name: "客户端", exact: true })).toBeVisible();
}

async function shot(page: Page, name: string) {
  const dir = process.env.E2E_SHOT_DIR;
  if (dir) await page.screenshot({ path: `${dir}/${name}.png`, fullPage: true });
}

/** 行尾「…」菜单（Radix DropdownMenu，Portal 到 body）：先开菜单，再按 menuitem 名点。 */
async function clickRowMenuItem(page: Page, client: string, item: string) {
  await page
    .locator(`[data-client=${client}]`)
    .getByRole("button", { name: /^更多操作/ })
    .click();
  await page.getByRole("menuitem", { name: item, exact: true }).click();
}

test("按连接状态分组：已连接 / 可连接 / 未检测到", async ({ page }) => {
  await gotoClients(page);

  const group = (name: string) => page.getByRole("region", { name: new RegExp(`^${name}`) });

  // 已连接组：只列版本与配置路径，不再有主动作按钮；「重新连接」「断开」在行尾菜单
  const claude = group("已连接").locator("[data-client=claude]");
  await expect(claude.getByText("v2.1.207")).toBeVisible();
  await expect(claude.getByRole("button", { name: "重新连接" })).toHaveCount(0);
  await claude.getByRole("button", { name: /^更多操作/ }).click();
  await expect(page.getByRole("menuitem", { name: "重新连接" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "断开" })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);

  // 可连接组：主按钮可点
  const codex = group("可连接").locator("[data-client=codex]");
  await expect(codex.getByText("v0.133.0")).toBeVisible();
  await expect(codex.getByRole("button", { name: "连接", exact: true })).toBeEnabled();

  // Cline 未安装：落在「未检测到」组，不挂点不动的连接按钮
  const cline = group("未检测到").locator("[data-client=cline]");
  await expect(cline).toBeVisible();
  await expect(cline.getByRole("button", { name: "连接", exact: true })).toHaveCount(0);

  await shot(page, "clients-rows");
});

// 已安装的排前面，未安装的沉底：fixture 里 cline 未装且夹在中间，应被排到最后。
test("排序：未检测到的一组沉底", async ({ page }) => {
  await gotoClients(page);

  const ids = await page
    .locator("[data-client]")
    .evaluateAll((els) => els.map((el) => el.getAttribute("data-client")));
  expect(ids[ids.length - 1]).toBe("cline");
  expect(ids.slice(0, -1)).not.toContain("cline");
});

// 版本探测可能落空（客户端装在非常规位置，`--version` 无从跑起），
// 此时版本栏留空（装没装由分组说明），把占位符渲染成 "v?" 会被当成故障。
test("装了但版本探测落空：不渲染问号占位", async ({ page }) => {
  await gotoClients(page, {
    detect_clients: {
      claude: { ...FIXTURES.detect_clients.claude, version: null },
      codex: { ...FIXTURES.detect_clients.codex, version: null },
    },
  });

  for (const id of ["claude", "codex"]) {
    const card = page.locator(`[data-client=${id}]`);
    await expect(card).toBeVisible();
    await expect(card.getByText(/v\?|v\d/)).toHaveCount(0);
  }
});

test("连接流程：env 默认一键写入 → inline 可切换", async ({ page }) => {
  await gotoClients(page);

  await page
    .locator("[data-client=codex]")
    .getByRole("button", { name: "连接", exact: true })
    .click();

  const dialog = page.getByRole("dialog", { name: "连接预览 · Codex" });
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText("当前配置")).toBeVisible();
  await expect(dialog.getByText("写入后")).toBeVisible();
  // 折叠渲染：长配置的未变更头部收起，变更块（mcpane 条目）打开即见
  await expect(dialog.getByText(/展开 \d+ 行未变更/)).toBeVisible();
  await expect(dialog.getByText("[mcp_servers.mcpane]")).toHaveCount(1);
  await expect(dialog.getByText("bearer_token_env_var", { exact: false }).first()).toBeVisible();
  const envCard = dialog.getByRole("radio", { name: "环境变量" });
  const inlineCard = dialog.getByRole("radio", { name: "直接写入 Token" });
  await expect(envCard).toBeChecked();
  await expect(dialog.getByText("写入时自动设置", { exact: true })).toBeVisible();
  await shot(page, "clients-compare-modal");

  // 展开折叠区后完整上文可见
  await dialog.getByText(/展开 \d+ 行未变更/).click();
  await expect(dialog.getByText('model = "gpt-x"').first()).toBeVisible();

  // 切到 inline 直写模式：预览换为占位串形态
  await inlineCard.click();
  await expect(inlineCard).toBeChecked();
  await expect(dialog.getByText("<连接时自动发放>", { exact: false }).first()).toBeVisible();

  // 切回 env 默认，一键确认写入
  await envCard.click();
  await expect(envCard).toBeChecked();
  await expect(dialog.getByText("bearer_token_env_var", { exact: false }).first()).toBeVisible();
  await dialog.getByRole("button", { name: "确认写入" }).click();

  const done = page.getByRole("dialog", { name: "Codex · 已连接" });
  await expect(done.getByText("配置已写入")).toBeVisible();
  await expect(done.getByText("snap_20260711_001", { exact: false })).toBeVisible();
  // env 一键结果：Token 前缀可见、凭据自动持久化、唯一手动步骤是重启
  await expect(done.getByText("9a1b2c3d", { exact: false })).toBeVisible();
  await expect(done.getByText("env.sh", { exact: false }).first()).toBeVisible();
  await expect(done.getByText("无需其他手动步骤", { exact: false })).toBeVisible();
  await expect(done.getByText("export ", { exact: false })).toHaveCount(0);
  await shot(page, "clients-apply-outcome");

  await done.getByRole("button", { name: "完成" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("凭证下沉：按客户端过滤凭证列表并回显工作区绑定", async ({ page }) => {
  await gotoClients(page);

  await clickRowMenuItem(page, "claude", "凭证");
  const dialog = page.getByRole("dialog", { name: "凭证 · Claude Code" });
  await expect(dialog.getByText("conn_dev_claude")).toBeVisible();
  // 从行菜单打开：菜单关闭会把焦点还给背景里的触发器，弹窗得把它拉回来，往回 Tab 也不能
  // 走进背景（真机曾一路走到侧栏导航，按 Space 就切走了页面）。
  const focusInDialog = () => dialog.evaluate((el) => el.contains(document.activeElement));
  await expect.poll(focusInDialog).toBe(true);
  for (let i = 0; i < 4; i++) {
    await page.keyboard.press("Shift+Tab");
    expect(await focusInDialog()).toBe(true);
  }
  // 其它客户端的凭证不串场
  await expect(dialog.getByText("conn_dev_codex")).toHaveCount(0);
  // 工作区绑定回显（存量 bug 修复：不再恒显默认）。
  // 下拉已从原生 select 收敛到 Radix Select，选中项以触发器上的文本呈现而非 value，
  // 故断言的是回显的工作区名——守的仍是同一条线：绑了 ws_web 就不能显示成「默认工作区」。
  const wsSelect = dialog.getByRole("combobox", { name: "绑到哪个工作区（对新开的对话生效）" });
  await expect(wsSelect).toHaveText("Web 项目");
  // 展开的下拉先吃 ESC：只收起下拉，凭证弹窗留着（Modal 曾不看 defaultPrevented，一按全关）。
  await wsSelect.click();
  await expect(page.getByRole("listbox")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("listbox")).toHaveCount(0);
  await expect(dialog).toBeVisible();
  // 工具暴露模式（ADR-020）：回显 fixture 的 auto，切到网关走乐观更新即时高亮。
  const exposure = dialog.getByRole("group", { name: "调整 conn_dev_claude 的工具暴露模式" });
  await expect(exposure.getByRole("radio", { name: "自动" })).toBeChecked();
  await exposure.getByRole("radio", { name: "网关" }).click();
  await expect(exposure.getByRole("radio", { name: "网关" })).toBeChecked();
  await shot(page, "clients-credentials-modal");

  // 轮换联动同步：env 形态由 envsync 重写环境变量存储，不弹一次性明文
  await dialog.getByRole("button", { name: "为 conn_dev_claude 签发新凭证" }).click();
  await expect(dialog.getByText("已同步", { exact: true })).toBeVisible();
  await expect(dialog.getByText("77aa88bb", { exact: false })).toBeVisible();
  await expect(dialog.getByText("env.sh", { exact: false })).toBeVisible();
  await expect(dialog.getByText("只显示这一次")).toHaveCount(0);

  // 删除凭证：需先确认（区别于撤销的一键开关，删除不可逆）
  await dialog.getByRole("button", { name: "删除 conn_dev_claude 的凭证" }).click();
  const confirm = page.getByRole("dialog", { name: "删除凭证" });
  await expect(confirm.getByText("conn_dev_claude", { exact: false })).toBeVisible();
  // 取消不触发删除，凭证弹窗仍在
  await confirm.getByRole("button", { name: "取消" }).click();
  await expect(page.getByRole("dialog", { name: "删除凭证" })).toHaveCount(0);
  await expect(dialog.getByText("conn_dev_claude")).toBeVisible();

  await dialog.getByRole("button", { name: "删除 conn_dev_claude 的凭证" }).click();
  await page
    .getByRole("dialog", { name: "删除凭证" })
    .getByRole("button", { name: "删除" })
    .click();
  await expect(page.getByRole("dialog", { name: "删除凭证" })).toHaveCount(0);

  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("使用说明：一键叙事 + 手动配置备用节", async ({ page }) => {
  await gotoClients(page);

  await clickRowMenuItem(page, "claude", "使用说明");
  const claudeDialog = page.getByRole("dialog", { name: "使用说明 · Claude Code" });
  // §1 一键连接：两种凭据方式都自动，callout 点名环境变量与直写模式
  await expect(claudeDialog.getByText("一键连接")).toBeVisible();
  await expect(
    claudeDialog.getByText("MCPANE_CLAUDE_TOKEN", { exact: false }).first(),
  ).toBeVisible();
  await expect(claudeDialog.getByText("直接写入 Token", { exact: false }).first()).toBeVisible();
  await expect(claudeDialog.getByText("重启并验证")).toBeVisible();
  await expect(claudeDialog.getByText("claude mcp list", { exact: false })).toBeVisible();
  // 手动配置降级为备用节：平台切换 export/setx + 官方 CLI 命令 + 复制按钮
  await expect(claudeDialog.getByText("手动配置（备用）")).toBeVisible();
  await claudeDialog.getByRole("button", { name: "macOS" }).click();
  await expect(claudeDialog.getByText('export MCPANE_CLAUDE_TOKEN="<Token>"')).toBeVisible();
  await claudeDialog.getByRole("button", { name: "Windows" }).click();
  await expect(claudeDialog.getByText('setx MCPANE_CLAUDE_TOKEN "<Token>"')).toBeVisible();
  await expect(
    claudeDialog.getByText("claude mcp add --scope user --transport http mcpane", {
      exact: false,
    }),
  ).toBeVisible();
  await expect(claudeDialog.getByRole("button", { name: "复制" }).first()).toBeVisible();
  await shot(page, "clients-howto-modal");
  await page.keyboard.press("Escape");

  // Cline 无 CLI 且不支持环境变量：固定直写叙事 + 备用节展示配置片段
  await clickRowMenuItem(page, "cline", "使用说明");
  const clineDialog = page.getByRole("dialog", { name: "使用说明 · Cline" });
  await expect(clineDialog.getByText("不支持环境变量展开", { exact: false }).first()).toBeVisible();
  await expect(clineDialog.getByText('"url": "http://127.0.0.1:24860/mcp"').first()).toBeVisible();
  await page.keyboard.press("Escape");

  // ZCode 的 Server 表挂在嵌套的 mcp.servers 下——片段里写成顶层 mcpServers
  // 就是让用户往一个 ZCode 不读的位置贴，这条断言盯的就是这个。
  await clickRowMenuItem(page, "zcode", "使用说明");
  const zcodeDialog = page.getByRole("dialog", { name: "使用说明 · ZCode" });
  await expect(zcodeDialog.getByText('"servers"').first()).toBeVisible();
  await expect(zcodeDialog.getByText('"mcpServers"')).toHaveCount(0);
  await page.keyboard.press("Escape");
});

/**
 * 客户端页的四个弹窗，浅色与暗色各扫一遍。
 *
 * 此前只有「连接」这一个弹窗被扫过，且只在浅色下——凭证、访问控制、使用说明
 * 三个从未进过 axe。弹窗恰恰是最容易漏的一层：页面级扫描遍历导航就结束了，
 * 弹窗要点开才在 DOM 里；而它们又集中用了 soft 底 + 描边的提示条（一次性
 * Token、已同步回执），那套配色在暗色下整体换值，浅色扫过不代表暗色成立。
 */
const CLIENT_DIALOGS = [
  { button: "连接", client: "codex", label: "连接", menu: false },
  { button: "凭证", client: "claude", label: "凭证", menu: true },
  { button: "访问控制", client: "claude", label: "访问控制", menu: true },
  { button: "使用说明", client: "claude", label: "使用说明", menu: true },
] as const;

for (const scheme of ["light", "dark"] as const) {
  test(`a11y：客户端页各弹窗在${scheme === "dark" ? "暗色" : "浅色"}下无 serious/critical 违规`, async ({
    page,
  }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await gotoClients(page);

    for (const dialog of CLIENT_DIALOGS) {
      if (dialog.menu) {
        await clickRowMenuItem(page, dialog.client, dialog.button);
      } else {
        await page
          .locator(`[data-client=${dialog.client}]`)
          .getByRole("button", { name: dialog.button, exact: true })
          .click();
      }
      await expect(page.getByRole("dialog")).toBeVisible();
      // 背景内容被遮罩刻意压暗，对比度不适用；只扫描弹窗本身。
      await expectNoSevereA11yViolations(page, `${scheme}/${dialog.label}`, '[role="dialog"]');
      await page.keyboard.press("Escape");
      await expect(page.getByRole("dialog")).toHaveCount(0);
    }
  });
}

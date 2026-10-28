import { expect, expectNoSevereA11yViolations, type Page, test } from "./support";

/** `${NAME}` 占位符；写成普通字符串字面量会被 lint 当成忘了用模板字符串。 */
const placeholder = (name: string) => `\${${name}}`;

/**
 * 服务器页（mock Tauri IPC）：连接弹窗三 tab、手动创建含环境变量、
 * 编辑预填带 id 提交、JSON / 文件导入（预览与占位符补填）、导出。
 * mock 侧记录 server_upsert / 导入 / 导出的入参供断言。
 */

const SERVERS = [
  {
    id: "srv_ctx7",
    slug: "context7",
    name: "Context7",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@upstash/context7-mcp"],
    env: { API_KEY: "secret" },
    cwd: null,
    url: null,
    bearerEnv: null,
    runtimePolicy: "shared",
    enabled: true,
  },
  {
    id: "srv_remote",
    slug: "remote-mcp",
    name: "Remote MCP",
    transport: "http",
    command: null,
    args: [],
    env: {},
    cwd: null,
    url: "https://example.com/mcp",
    bearerEnv: "MY_MCP_TOKEN",
    runtimePolicy: "shared",
    enabled: false,
  },
];

async function gotoServers(page: Page) {
  await page.addInitScript(`
    const servers = ${JSON.stringify(SERVERS)};
    window.__E2E_UPSERTS__ = [];
    window.__E2E_IMPORTS__ = [];
    window.__E2E_EXPORTS__ = [];
    // 导入 mock 的最小解析：与后端同口径（command / url 二选一，slug 撞了算冲突）。
    const entriesOf = (json) => {
      const root = JSON.parse(json);
      return Object.entries(root.mcpServers ?? root);
    };
    const defOf = (name, entry) => ({
      id: "",
      slug: name.toLowerCase(),
      name,
      transport: entry.url ? "http" : "stdio",
      command: entry.command ?? null,
      args: entry.args ?? [],
      env: entry.env ?? {},
      cwd: null,
      url: entry.url ?? null,
      bearer: null,
      bearerEnv: null,
      headers: entry.headers ?? {},
      runtimePolicy: "shared",
      enabled: entry.enabled !== false,
    });
    window.__E2E_ACCESS__ = [];
    // 行内客户端可见性：Claude Code 一条连接全放行，Codex 一条连接拒绝了 Context7。
    const denied = { conn_claude: [], conn_codex: ["srv_ctx7"] };
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd, args) => {
        if (cmd === "servers_list") return structuredClone(servers);
        if (cmd === "hub_status") {
          return {
            name: "mcpane-hub",
            serverVersion: "0.0.1",
            protocolVersion: "2026-07-28",
            legacyProtocolVersion: "2025-11-25",
            endpoint: "http://127.0.0.1:24860/mcp",
            uptimeSecs: 1,
            connections: [
              { clientConnectionId: "conn_claude", clientType: "claude", tokenPrefix: "aaaa", enabled: true, defaultWorkspaceId: null, toolExposureMode: "auto" },
              { clientConnectionId: "conn_codex", clientType: "codex", tokenPrefix: "bbbb", enabled: true, defaultWorkspaceId: null, toolExposureMode: "auto" },
            ],
          };
        }
        if (cmd === "mcp_clients_status") return [];
        if (cmd === "connection_denied_servers") return structuredClone(denied[args.clientConnectionId] ?? []);
        if (cmd === "connection_server_access_set") {
          window.__E2E_ACCESS__.push(structuredClone(args));
          const list = denied[args.clientConnectionId] ?? [];
          denied[args.clientConnectionId] = args.allowed
            ? list.filter((id) => id !== args.serverId)
            : [...list, args.serverId];
          return null;
        }
        if (cmd === "server_upsert") {
          const input = structuredClone(args.input);
          window.__E2E_UPSERTS__.push(input);
          const existing = servers.find((s) => s.id === input.id);
          if (existing) {
            Object.assign(existing, input, { id: existing.id, slug: existing.slug });
          } else {
            servers.push({
              ...input,
              id: "srv_new",
              slug: input.name.toLowerCase(),
              runtimePolicy: "shared",
              enabled: true,
            });
          }
          return null;
        }
        if (cmd === "server_set_enabled") {
          const target = servers.find((s) => s.id === args.id);
          if (target) target.enabled = args.enabled;
          return null;
        }
        if (cmd === "server_delete") {
          const index = servers.findIndex((s) => s.id === args.id);
          if (index >= 0) servers.splice(index, 1);
          return null;
        }
        if (cmd === "servers_import_preview") {
          const entries = [];
          const invalid = [];
          for (const [name, entry] of entriesOf(args.json)) {
            if (!entry.command && !entry.url) {
              invalid.push(name);
              continue;
            }
            const owner = servers.find((s) => s.slug === name.toLowerCase());
            const found = JSON.stringify(entry).match(/[$][{][A-Za-z_][A-Za-z0-9_]*[}]/g) ?? [];
            entries.push({
              server: defOf(name, entry),
              conflict: owner ? owner.name : null,
              variables: [...new Set(found.map((m) => m.slice(2, -1)))],
            });
          }
          return { entries, invalid };
        }
        if (cmd === "servers_import_json") {
          window.__E2E_IMPORTS__.push(structuredClone(args));
          const map = Object.fromEntries(entriesOf(args.json));
          for (const name of args.selected) {
            servers.push({ ...defOf(name, map[name]), id: "srv_" + name });
          }
          return { imported: args.selected, skipped: [], warnings: [], incomplete: [] };
        }
        if (cmd === "servers_export") {
          window.__E2E_EXPORTS__.push(structuredClone(args.ids));
          const picked = servers.filter((s) => args.ids.includes(s.id));
          const entry = (s) => (s.command ? { command: s.command } : { type: "http", url: s.url });
          return {
            json: JSON.stringify(
              { mcpServers: Object.fromEntries(picked.map((s) => [s.name, entry(s)])) },
              null,
              2,
            ),
            // Context7 的 env 里有 API_KEY，后端会把它换成占位符。
            redactions: picked.some((s) => s.id === "srv_ctx7")
              ? [{ server: "Context7", field: "env", variable: "API_KEY" }]
              : [],
          };
        }
        if (cmd === "servers_export_save") return "/Users/me/Downloads/mcpane-servers.json";
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
  await expect(page.getByRole("heading", { name: "MCP 服务器" })).toBeVisible();
}

async function lastUpsert(page: Page) {
  return page.evaluate(() =>
    (window as unknown as { __E2E_UPSERTS__: unknown[] }).__E2E_UPSERTS__.at(-1),
  );
}

test("手动创建：tab 切换、slug 预览、环境变量行、提交 payload", async ({ page }) => {
  await gotoServers(page);

  await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
  const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });
  await expect(dialog).toBeVisible();
  // 默认落在官方预设：多数人要装的就是那几个官方 Server，最省事的排最前
  await expect(dialog.getByRole("tab", { name: "官方预设" })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  await dialog.getByRole("tab", { name: "手动创建" }).click();
  await dialog.getByPlaceholder("如 memory").fill("My Server!");
  // slug 预览与后端规则一致
  await expect(dialog.getByText("工具前缀：my-server__")).toBeVisible();
  await dialog.getByPlaceholder("npx").fill("npx");
  await dialog.getByPlaceholder("-y @modelcontextprotocol/server-memory").fill("-y demo-mcp");

  await dialog.getByRole("button", { name: "添加环境变量" }).click();
  await dialog.getByPlaceholder("名称，如 API_KEY").fill("TOKEN");
  await dialog.getByPlaceholder("值").fill("abc123");

  await dialog.getByRole("button", { name: "创建 MCP 服务器" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByText("My Server!", { exact: true })).toBeVisible();

  expect(await lastUpsert(page)).toMatchObject({
    id: null,
    name: "My Server!",
    transport: "stdio",
    command: "npx",
    args: ["-y", "demo-mcp"],
    env: { TOKEN: "abc123" },
  });
});

test("编辑：预填 env/命令，保存带原 id 提交", async ({ page }) => {
  await gotoServers(page);

  await page.getByRole("button", { name: "更多操作 · Context7" }).click();
  await page.getByRole("menuitem", { name: "编辑" }).click();
  const dialog = page.getByRole("dialog", { name: "编辑 MCP 服务器 · Context7" });
  await expect(dialog).toBeVisible();
  // 编辑模式双 tab（常规设置/工具）+ 表单/JSON 模式切换，默认落在常规设置的表单模式
  await expect(dialog.getByRole("tab", { name: "常规设置" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(dialog.getByRole("tab", { name: "表单" })).toHaveAttribute("aria-selected", "true");
  await expect(dialog.getByPlaceholder("npx")).toHaveValue("npx");
  await expect(dialog.getByPlaceholder("名称，如 API_KEY")).toHaveValue("API_KEY");
  await expect(dialog.getByPlaceholder("值")).toHaveValue("secret");
  // slug 固定显示既有值
  await expect(dialog.getByText("工具前缀：context7__")).toBeVisible();

  await dialog.getByPlaceholder("如 memory").fill("Context7 Pro");
  await dialog.getByRole("button", { name: "保存修改" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByText("Context7 Pro")).toBeVisible();

  expect(await lastUpsert(page)).toMatchObject({ id: "srv_ctx7", name: "Context7 Pro" });
});

test("启停：Switch 触发 server_set_enabled，状态翻转并保持", async ({ page }) => {
  await gotoServers(page);

  // Context7 初始启用 → aria-label 是「停用」动作。
  const toggle = page.getByRole("switch", { name: "停用 Context7" });
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await toggle.click();

  // 乐观翻转 + 失效重取（mock 已落盘 enabled=false）后，动作变回「启用」。
  const toggledOff = page.getByRole("switch", { name: "启用 Context7" });
  await expect(toggledOff).toHaveAttribute("aria-checked", "false");
});

test("删除：确认对话框拦一道，确认后卡片消失", async ({ page }) => {
  await gotoServers(page);

  await page.getByRole("button", { name: "更多操作 · Remote MCP" }).click();
  await page.getByRole("menuitem", { name: "删除" }).click();
  // 不做一点即删：先弹确认。取消/确认前卡片仍在。
  const confirm = page.getByRole("dialog", { name: "删除服务器" });
  await expect(confirm).toBeVisible();
  await expect(page.getByText("Remote MCP", { exact: true })).toBeVisible();

  await confirm.getByRole("button", { name: "删除", exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByText("Remote MCP", { exact: true })).toHaveCount(0);
  // 无关卡片不受牵连。
  await expect(page.getByText("Context7", { exact: true })).toBeVisible();
});

test("远程服务器：URL + Bearer 环境变量创建 http 服务器", async ({ page }) => {
  await gotoServers(page);

  await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
  const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });
  await dialog.getByRole("tab", { name: "远程服务器" }).click();
  await dialog.getByPlaceholder("如 memory").fill("Exa");
  await dialog.getByPlaceholder("https://example.com/mcp").fill("https://mcp.exa.ai/mcp");
  // Bearer 环境变量收在折叠区，先展开
  await dialog.getByText("高级：从环境变量读取 Token").click();
  await dialog.getByPlaceholder("MY_MCP_TOKEN").fill("EXA_TOKEN");
  await dialog.getByRole("button", { name: "创建 MCP 服务器" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);

  expect(await lastUpsert(page)).toMatchObject({
    id: null,
    transport: "http",
    url: "https://mcp.exa.ai/mcp",
    bearerEnv: "EXA_TOKEN",
    command: null,
    env: {},
  });
});

async function lastImport(page: Page) {
  return page.evaluate(() =>
    (window as unknown as { __E2E_IMPORTS__: unknown[] }).__E2E_IMPORTS__.at(-1),
  );
}

async function lastExport(page: Page) {
  return page.evaluate(() =>
    (window as unknown as { __E2E_EXPORTS__: unknown[] }).__E2E_EXPORTS__.at(-1),
  );
}

test("导入 JSON：粘贴配置 → 逐条预览 → 自动关窗 → 列表出现新服务器与结果摘要", async ({ page }) => {
  await gotoServers(page);

  await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
  const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });
  await dialog.getByRole("tab", { name: "导入 JSON" }).click();
  // 空输入禁用导入按钮
  await expect(dialog.getByRole("button", { name: "导入", exact: true })).toBeDisabled();

  // 导入框是 CodeMirror（contenteditable），非原生 textarea
  await dialog
    .locator(".cm-content")
    .fill(
      '{ "mcpServers": { "demo-server": { "command": "npx", "args": ["-y", "demo"] }, "context7": { "command": "npx" } } }',
    );
  // 前缀已被占用的条目不可勾选，并说出被谁占了。
  await expect(dialog.getByRole("checkbox", { name: "导入 demo-server" })).toBeChecked();
  await expect(dialog.getByRole("checkbox", { name: "导入 context7" })).toBeDisabled();
  await expect(dialog.getByText("与「Context7」工具前缀相同，跳过")).toBeVisible();

  await dialog.getByRole("button", { name: "导入 1 个服务器" }).click();
  // 导入成功即自动关窗，结果摘要落在列表页内联提示位
  await expect(dialog).not.toBeVisible();
  await expect(page.getByText("已导入 1 个")).toBeVisible();
  await expect(page.getByText("demo-server", { exact: true })).toBeVisible();
  expect(await lastImport(page)).toMatchObject({ selected: ["demo-server"], values: {} });
});

test("页头导入：选择导出文件，占位符就地补填后随导入一起提交", async ({ page }) => {
  await gotoServers(page);

  await page.getByRole("button", { name: "导入", exact: true }).click();
  await page.getByRole("menuitem", { name: "从文件或 JSON 导入…" }).click();
  const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });
  await expect(dialog.getByRole("tab", { name: "导入 JSON" })).toHaveAttribute(
    "aria-selected",
    "true",
  );

  const file = {
    mcpServers: {
      github: {
        type: "http",
        url: "https://api.githubcopilot.com/mcp/",
        headers: { Authorization: `Bearer ${placeholder("AUTHORIZATION")}` },
      },
    },
  };
  await dialog.locator('input[type="file"]').setInputFiles({
    name: "mcpane-servers.json",
    mimeType: "application/json",
    buffer: Buffer.from(JSON.stringify(file)),
  });

  await expect(dialog.getByText("待填写 1 项")).toBeVisible();
  await dialog.getByLabel("github 的 AUTHORIZATION").fill("ghp_e2e");
  await expect(dialog.getByText("待填写 1 项")).toBeHidden();
  await dialog.getByRole("button", { name: "导入 1 个服务器" }).click();

  await expect(dialog).not.toBeVisible();
  expect(await lastImport(page)).toEqual({
    json: JSON.stringify(file),
    selected: ["github"],
    values: { github: { AUTHORIZATION: "ghp_e2e" } },
  });
});

test("导出：页头默认全选并列出凭据替换明细，保存后显示路径；行尾菜单只勾选该行", async ({
  page,
}) => {
  await gotoServers(page);

  await page.getByRole("button", { name: "导出", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "导出 MCP 服务器" });
  await expect(dialog.getByText("已选 2 / 2")).toBeVisible();
  await expect(dialog.getByText("已将 1 处凭据替换为占位符")).toBeVisible();
  await expect(dialog.getByText(placeholder("API_KEY"))).toBeVisible();
  expect(await lastExport(page)).toEqual(["srv_ctx7", "srv_remote"]);

  await dialog.getByRole("button", { name: "保存为文件" }).click();
  await expect(dialog.getByText("已保存到 /Users/me/Downloads/mcpane-servers.json")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(dialog).not.toBeVisible();

  await page.getByRole("button", { name: "更多操作 · Remote MCP" }).click();
  await page.getByRole("menuitem", { name: "导出…" }).click();
  await expect(dialog.getByText("已选 1 / 2")).toBeVisible();
  await expect(dialog.getByRole("checkbox", { name: "导出 Remote MCP" })).toBeChecked();
  await expect(dialog.getByRole("checkbox", { name: "导出 Context7" })).not.toBeChecked();
  await expect(dialog.getByText(/未发现需要替换的凭据/)).toBeVisible();
  expect(await lastExport(page)).toEqual(["srv_remote"]);
});

/**
 * 连接弹窗与删除确认框，浅色暗色各一遍。
 *
 * 连接弹窗此前只在浅色下扫过；确认框则一次也没进过 axe——它是全应用最短的一个
 * 弹窗（一句话 + 两个按钮），但用的恰恰是 danger 那套配色，暗色下 danger-soft
 * 是深红棕，图标与正文压在上面，正是最容易掉出 AA 的组合。
 */
for (const scheme of ["light", "dark"] as const) {
  const themeLabel = scheme === "dark" ? "暗色" : "浅色";

  test(`a11y：连接弹窗在${themeLabel}下无 serious/critical 违规`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await gotoServers(page);
    await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await expectNoSevereA11yViolations(page, `${scheme}/连接弹窗`, '[role="dialog"]');
  });

  test(`a11y：删除确认框在${themeLabel}下无 serious/critical 违规`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await gotoServers(page);
    await page.getByRole("button", { name: "更多操作 · Remote MCP" }).click();
    await page.getByRole("menuitem", { name: "删除" }).click();
    await expect(page.getByRole("dialog", { name: "删除服务器" })).toBeVisible();
    await expectNoSevereA11yViolations(page, `${scheme}/删除确认`, '[role="dialog"]');
  });

  test(`a11y：导出弹窗在${themeLabel}下无 serious/critical 违规`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await gotoServers(page);
    await page.getByRole("button", { name: "导出", exact: true }).click();
    await expect(page.getByText("已将 1 处凭据替换为占位符")).toBeVisible();
    await expectNoSevereA11yViolations(page, `${scheme}/导出弹窗`, '[role="dialog"]');
  });

  test(`a11y：导入预览在${themeLabel}下无 serious/critical 违规`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await gotoServers(page);
    await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
    const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });
    await dialog.getByRole("tab", { name: "导入 JSON" }).click();
    // 冲突、待填写、导入后停用、无效条目四种例外态一次铺开。
    await dialog.locator(".cm-content").fill(
      JSON.stringify({
        context7: { command: "npx" },
        remote: { url: "https://x.dev/mcp", headers: { "X-API-Key": placeholder("X_API_KEY") } },
        paused: { command: "npx", enabled: false },
        broken: {},
      }),
    );
    await expect(dialog.getByText("待填写 1 项")).toBeVisible();
    await expectNoSevereA11yViolations(page, `${scheme}/导入预览`, '[role="dialog"]');
  });
}

/**
 * 官方预设：新建 Server 的主路径。
 *
 * 预设只负责填表——填完仍落在手动表单里，用户可以看清自己装的是什么、
 * 先测再存。这条覆盖「选预设 → 表单被填好 → 切到对应 transport」。
 */
test("官方预设：选一个即填好命令与参数", async ({ page }) => {
  await gotoServers(page);
  await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
  const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });

  await dialog.getByRole("button", { name: "使用 sequential-thinking" }).click();

  // 自动切到 stdio 页签，且命令/参数已填好。
  await expect(dialog.getByRole("tab", { name: "手动创建" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(dialog.getByPlaceholder("如 memory")).toHaveValue("sequential-thinking");
  await expect(dialog.getByPlaceholder("npx")).toHaveValue("npx");
  await expect(dialog.getByPlaceholder("-y @modelcontextprotocol/server-memory")).toHaveValue(
    "-y @modelcontextprotocol/server-sequential-thinking",
  );
});

/** 需要参数的预设先收参数；填完的值要落到参数末尾。 */
test("官方预设：需要参数的先收参数再填表", async ({ page }) => {
  await gotoServers(page);
  await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
  const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });

  await dialog.getByRole("button", { name: "使用 filesystem" }).click();
  // 没填目录之前不能填入。
  await expect(dialog.getByRole("button", { name: /填入配置/ })).toBeDisabled();

  await dialog.getByPlaceholder("/Users/you/projects").fill("/tmp/demo");
  await dialog.getByRole("button", { name: /填入配置/ }).click();

  await expect(dialog.getByPlaceholder("-y @modelcontextprotocol/server-memory")).toHaveValue(
    "-y @modelcontextprotocol/server-filesystem /tmp/demo",
  );
});

/** GitHub 走远端 HTTP：填出来应落在「远程服务器」页签而非命令行。 */
test("官方预设：GitHub 落到远程服务器页签", async ({ page }) => {
  await gotoServers(page);
  await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
  const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });

  await dialog.getByRole("button", { name: "使用 github" }).click();
  await dialog.getByLabel(/GitHub Personal Access Token/).fill("ghp_demo");
  await dialog.getByRole("button", { name: /填入配置/ }).click();

  await expect(dialog.getByRole("tab", { name: "远程服务器" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(dialog.getByPlaceholder(/https/).first()).toHaveValue(
    "https://api.githubcopilot.com/mcp/",
  );
});

/**
 * 预设 = 从这个预设重新开始，先前在别的页签填的东西不能串过来。
 *
 * 具体后果很实在：先在「手动创建」填过工作目录，再回来选 filesystem，
 * Server 会跑在那个残留目录下——而 serverInput() 只按 tab 过滤，
 * 过滤不掉同 transport 的残留。
 */
test("官方预设：不串入先前页签填的残留值", async ({ page }) => {
  await gotoServers(page);
  await page.getByRole("button", { name: "添加 MCP 服务器" }).click();
  const dialog = page.getByRole("dialog", { name: "添加 MCP 服务器" });

  // 先在手动页签留下工作目录与一行环境变量。
  await dialog.getByRole("tab", { name: "手动创建" }).click();
  await dialog.getByPlaceholder("/path/to/project").fill("/tmp/stale-cwd");
  await dialog.getByRole("button", { name: "添加环境变量" }).click();
  await dialog.getByPlaceholder("名称，如 API_KEY").fill("STALE");

  // 回预设选一个无参数的。
  await dialog.getByRole("tab", { name: "官方预设" }).click();
  await dialog.getByRole("button", { name: "使用 sequential-thinking" }).click();

  await expect(dialog.getByPlaceholder("/path/to/project")).toHaveValue("");
  await expect(dialog.getByPlaceholder("名称，如 API_KEY")).toHaveCount(0);
});

test("行内客户端可见性：灰图标点一下恢复可见，计数条随之变化", async ({ page }) => {
  await gotoServers(page);

  await expect(page.getByText("Claude Code 可见 2/2")).toBeVisible();
  await expect(page.getByText("Codex 可见 1/2")).toBeVisible();

  const hidden = page.getByRole("switch", { name: "「Context7」已对 Codex 隐藏，点击恢复" });
  await expect(hidden).toHaveAttribute("aria-checked", "false");
  await hidden.click();

  await expect(
    page.getByRole("switch", { name: "「Context7」对 Codex 可见，点击隐藏" }),
  ).toHaveAttribute("aria-checked", "true");
  await expect(page.getByText("Codex 可见 2/2")).toBeVisible();
  const calls = await page.evaluate(
    () => (window as unknown as { __E2E_ACCESS__: unknown[] }).__E2E_ACCESS__,
  );
  expect(calls).toEqual([
    { clientConnectionId: "conn_codex", serverId: "srv_ctx7", allowed: true },
  ]);

  // 编辑/删除收在行尾「…」：按钮常驻可见（不靠悬停），菜单里两项齐全
  const row = page.locator("[data-tour=servers-card]");
  const more = row.getByRole("button", { name: "更多操作 · Context7" });
  await expect(more).toBeVisible();
  await more.click();
  await expect(page.getByRole("menuitem", { name: "编辑" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "删除" })).toBeVisible();
});

import { expect, type Page, test } from "./support";

/**
 * README 宣传截图（mock Tauri IPC）：概览 / 服务器 / 客户端三页，浅色暗色各一张。
 * 仅在设置 E2E_SHOT_DIR 时运行：
 *   E2E_SHOT_DIR=../../docs/screenshots pnpm exec playwright test e2e/screenshots.spec.ts
 * mock 数据按真实使用规模构造——空荡荡的界面截出来没有说服力。
 */

const SHOT_DIR = process.env.E2E_SHOT_DIR;

const SERVERS = [
  ["srv_ctx7", "context7", "Context7", "stdio", "npx", ["-y", "@upstash/context7-mcp"], null, true],
  ["srv_pw", "playwright", "Playwright", "stdio", "npx", ["@playwright/mcp@latest"], null, true],
  ["srv_gh", "github", "GitHub", "http", null, [], "https://api.githubcopilot.com/mcp/", true],
  [
    "srv_fs",
    "filesystem",
    "Filesystem",
    "stdio",
    "npx",
    ["-y", "@modelcontextprotocol/server-filesystem", "~/Projects"],
    null,
    true,
  ],
  ["srv_fc", "firecrawl", "Firecrawl", "stdio", "npx", ["-y", "firecrawl-mcp"], null, true],
  ["srv_sentry", "sentry", "Sentry", "http", null, [], "https://mcp.sentry.dev/mcp", false],
].map(([id, slug, name, transport, command, args, url, enabled]) => ({
  id,
  slug,
  name,
  transport,
  command,
  args,
  env: {},
  cwd: null,
  url,
  bearer: null,
  bearerEnv: url ? `${String(slug).toUpperCase()}_TOKEN` : null,
  headers: {},
  runtimePolicy: "shared",
  enabled,
}));

/** 每个启用的 Server 造一批工具，让目录/计数看起来是真的在跑。 */
const TOOLS = SERVERS.filter((s) => s.enabled).flatMap((s, i) =>
  Array.from({ length: [14, 21, 36, 11, 5][i] ?? 6 }, (_, k) => ({
    serverId: s.id,
    serverSlug: s.slug,
    originalName: `tool_${k}`,
    exposedName: `${s.slug}__tool_${k}`,
    description: null,
    schemaHash: `h${i}_${k}`,
    // 截图取"健康的下游"：不给注解的话每行都挂着「无注解」告警标，
    // README 截图会把一个正常配置拍成满屏黄标。
    annotations: { readOnlyHint: true, destructiveHint: false },
  })),
);

/** 概览页首卡：每个客户端的当前供应商。字段只填界面读得到的那几个。 */
const PROVIDERS = [
  ["official-claude", "claude", "official", "Claude 官方订阅", null, 0],
  ["relay_ds", "claude", "relay", "DeepSeek", "https://api.deepseek.com/anthropic", 1],
  ["relay_kimi", "claude", "relay", "Kimi For Coding", "https://api.kimi.com/coding/", 2],
  ["official-codex", "codex", "official", "Codex 官方订阅", null, 0],
  ["relay_or", "codex", "relay", "OpenRouter", "https://openrouter.ai/api/v1", 1],
  ["official-claude-desktop", "claude-desktop", "official", "Claude 官方订阅", null, 0],
  ["relay_ds_desk", "claude-desktop", "relay", "DeepSeek", "https://api.deepseek.com/anthropic", 1],
].map(([id, app, category, name, baseUrl, sortOrder]) => ({
  id,
  app,
  category,
  name,
  baseUrl,
  sortOrder,
}));

const FIXTURES = {
  provider_clients_installed: { claude: true, codex: true, "claude-desktop": true, gemini: false },
  providers_list: PROVIDERS,
  provider_current: {
    claude: "official-claude",
    codex: "official-codex",
    "claude-desktop": "relay_ds_desk",
  },
  provider_routing_get: {
    enabled: false,
    failoverQueue: [],
    tuning: {
      firstByteTimeoutSecs: 60,
      idleTimeoutSecs: 120,
      failureThreshold: 4,
      cooldownSecs: 60,
    },
  },
  router_status: [],
  router_clear_requests: null,
  desktop_proxy_status: { running: false, port: null, hasUpstream: false, listenPort: 24871 },
  usage_by_provider: [
    { key: "claude", displayName: null, costUsd: 4.82, priced: true },
    { key: "codex", displayName: null, costUsd: 1.37, priced: true },
  ],
  workspaces_list: [
    { id: "ws_default", name: "默认工作区" },
    { id: "ws_web", name: "Web 项目" },
  ],
  servers_list: SERVERS,
  hub_status: {
    name: "mcpane-hub",
    serverVersion: "0.2.0",
    protocolVersion: "2026-07-28",
    legacyProtocolVersion: "2025-11-25",
    endpoint: "http://127.0.0.1:24860/mcp",
    uptimeSecs: 3 * 86400 + 7 * 3600 + 41 * 60,
    connections: [
      {
        clientConnectionId: "conn_claude",
        clientType: "claude",
        tokenPrefix: "1f3e4025",
        enabled: true,
        defaultWorkspaceId: null,
        toolExposureMode: "auto",
      },
      {
        clientConnectionId: "conn_codex",
        clientType: "codex",
        tokenPrefix: "9a01cc72",
        enabled: true,
        defaultWorkspaceId: null,
        toolExposureMode: "auto",
      },
      {
        clientConnectionId: "conn_gemini",
        clientType: "gemini-cli",
        tokenPrefix: "5b7de913",
        enabled: true,
        defaultWorkspaceId: "ws_web",
        toolExposureMode: "gateway",
      },
    ],
  },
  hub_diagnostics: {
    catalogTools: TOOLS.length,
    downstreams: Object.fromEntries(
      SERVERS.filter((s) => s.enabled).map((s) => [s.id, { status: "ready", crashCount: 0 }]),
    ),
    workspaces: {},
  },
  hub_process_state: { state: "managed", pid: 8412 },
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
      connected: true,
      configPath: "/Users/dev/.codex/config.toml",
      tokenEnv: "MCPANE_CODEX_TOKEN",
    },
  },
  // 与 src-tauri/src/mcp_clients.rs 的 CLIENTS 表逐条对应（id / 展示名 / 配置路径
  // 都照实写）——README 截图取自这里，编出来的客户端等于对外宣传不存在的功能。
  custom_mcp_clients_list: [],
  mcp_clients_status: [
    [
      "claude-desktop",
      "Claude Desktop",
      true,
      true,
      null,
      "~/Library/Application Support/Claude/claude_desktop_config.json",
    ],
    ["cursor", "Cursor", true, true, "MCPANE_CURSOR_TOKEN", "~/.cursor/mcp.json"],
    [
      "vscode",
      "VS Code",
      true,
      false,
      "MCPANE_VSCODE_TOKEN",
      "~/Library/Application Support/Code/User/mcp.json",
    ],
    [
      "cline",
      "Cline",
      true,
      false,
      null,
      "~/Library/Application Support/Code/User/globalStorage/saoudrizwan.claude-dev/settings/cline_mcp_settings.json",
    ],
    ["devin", "Devin", false, false, "MCPANE_DEVIN_TOKEN", "~/.codeium/windsurf/mcp_config.json"],
    ["gemini-cli", "Gemini CLI", true, true, "MCPANE_GEMINI_TOKEN", "~/.gemini/settings.json"],
    ["antigravity", "Antigravity", true, true, null, "~/.gemini/config/mcp_config.json"],
    ["trae", "Trae", true, false, null, "~/Library/Application Support/Trae CN/User/mcp.json"],
    ["qoder", "Qoder", true, true, "MCPANE_QODER_TOKEN", "~/.qoder/settings.json"],
    ["qwen-code", "Qwen Code", false, false, "MCPANE_QWEN_TOKEN", "~/.qwen/settings.json"],
    ["codebuddy", "CodeBuddy", true, false, "MCPANE_CODEBUDDY_TOKEN", "~/.codebuddy/.mcp.json"],
    ["zcode", "ZCode", true, true, null, "~/.zcode/cli/config.json"],
  ].map(([id, display, installed, connected, tokenEnv, configPath]) => ({
    id,
    display,
    installed,
    connected,
    tokenEnv,
    configPath,
    custom: false,
  })),
};

async function boot(page: Page) {
  // README 统一用亮色截图
  await page.emulateMedia({ colorScheme: "light" });
  await page.addInitScript(`
    const fixtures = ${JSON.stringify(FIXTURES)};
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd, args) => {
        if (cmd in fixtures) return structuredClone(fixtures[cmd]);
        if (cmd === "catalog_list")
          return { id: "cat_1", workspaceId: "ws_default", tools: ${JSON.stringify(TOOLS)}, prompts: [], resources: [] };
        // 服务器页行内可见性：让截图里有一格灰的，看得出这列是开关不是装饰。
        if (cmd === "connection_denied_servers")
          return args?.clientConnectionId === "conn_codex" ? ["srv_pw"] : [];
        if (cmd.startsWith("plugin:")) return null;
        throw new Error("screenshot 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  await expect(page.getByText("http://127.0.0.1:24860/mcp").first()).toBeVisible();
}

async function shot(page: Page, name: string) {
  await page.mouse.move(0, 0);
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${SHOT_DIR}/${name}.png` });
}

test.use({ viewport: { width: 1440, height: 900 }, deviceScaleFactor: 2 });

test("README 截图", async ({ page }) => {
  test.skip(!SHOT_DIR, "设置 E2E_SHOT_DIR 才产出截图");
  await boot(page);
  await shot(page, "dashboard");

  await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
  await expect(page.getByText("Context7", { exact: true }).first()).toBeVisible();
  await shot(page, "servers");

  await page.getByRole("button", { name: "客户端", exact: true }).click();
  await expect(page.getByText("Claude Desktop", { exact: true }).first()).toBeVisible();
  // 八家客户端排三行，900 高视口装不下且滚动发生在 main 内部（fullPage 无效）——
  // 临时拉高视口把最后一行完整装进来
  await page.setViewportSize({ width: 1440, height: 1020 });
  await shot(page, "clients");
});

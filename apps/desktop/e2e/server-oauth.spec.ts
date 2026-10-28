import { expect, type Page, test } from "./support";

/**
 * 远程 MCP 的 OAuth 登录入口（ADR-024，mock Tauri IPC）：
 * 需要登录的卡片显示「需要登录」与「登录」按钮；等待浏览器授权期间可取消；
 * 登录成功后状态翻为运行中，行尾菜单出现「退出登录」；失败卡片说具体原因。
 */

const SERVERS = [
  {
    id: "srv_stdio",
    slug: "uvx-thing",
    name: "UVX Thing",
    transport: "stdio",
    command: "uvx",
    args: ["mcp-server-time"],
    env: {},
    cwd: null,
    url: null,
    bearerEnv: null,
    runtimePolicy: "shared",
    enabled: true,
  },
  {
    id: "srv_notion",
    slug: "notion",
    name: "Notion",
    transport: "http",
    command: null,
    args: [],
    env: {},
    cwd: null,
    url: "https://mcp.notion.com/mcp",
    bearerEnv: null,
    runtimePolicy: "shared",
    enabled: true,
  },
];

async function gotoServers(page: Page) {
  await page.addInitScript(`
    const servers = ${JSON.stringify(SERVERS)};
    // 可由测试驱动的后端状态：下游诊断 + 登录调用的挂起点。
    window.__E2E_CALLS__ = [];
    window.__E2E_STATE__ = {
      notion: { status: "needs-auth", signedIn: false, lastErrorKind: "needs_auth", lastError: "需要登录授权（OAuth）" },
    };
    let pendingLogin = null;
    window.__E2E_FINISH_LOGIN__ = (error) => {
      if (!pendingLogin) return;
      if (error) pendingLogin.reject(error);
      else pendingLogin.resolve({ ok: true });
      pendingLogin = null;
    };
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd, args) => {
        if (cmd === "servers_list") return structuredClone(servers);
        if (cmd === "hub_status") {
          return { name: "mcpane-hub", serverVersion: "0.0.1", protocolVersion: "2026-07-28",
            legacyProtocolVersion: "2025-11-25", endpoint: "http://127.0.0.1:24860/mcp",
            uptimeSecs: 1, connections: [] };
        }
        if (cmd === "hub_diagnostics") {
          return {
            downstreams: {
              srv_stdio: { status: "failed", crashCount: 0, lastErrorKind: "command_not_found",
                lastError: "No such file or directory (os error 2)", signedIn: null },
              srv_notion: { crashCount: 0, ...window.__E2E_STATE__.notion },
            },
            catalogRevision: "r", catalogTools: 0, workspaces: {},
          };
        }
        if (cmd === "mcp_clients_status") return [];
        if (cmd === "server_oauth_login") {
          window.__E2E_CALLS__.push(["login", args.id]);
          return new Promise((resolve, reject) => { pendingLogin = { resolve, reject }; });
        }
        if (cmd === "server_oauth_cancel") {
          window.__E2E_CALLS__.push(["cancel", args.id]);
          window.__E2E_FINISH_LOGIN__("登录已取消");
          return null;
        }
        if (cmd === "server_oauth_logout") {
          window.__E2E_CALLS__.push(["logout", args.id]);
          window.__E2E_STATE__.notion = { status: "needs-auth", signedIn: false, lastErrorKind: "needs_auth", lastError: "需要登录授权（OAuth）" };
          return null;
        }
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
  await expect(page.getByRole("heading", { name: "MCP 服务器" })).toBeVisible();
}

const calls = (page: Page) =>
  page.evaluate(() => (window as unknown as { __E2E_CALLS__: unknown[] }).__E2E_CALLS__);

test("需要登录的卡片显示徽标与登录按钮；失败卡片说具体原因而不是笼统的失败", async ({ page }) => {
  await gotoServers(page);
  const notion = page.locator("div.group", { hasText: "Notion" });
  await expect(notion.getByText("需要登录")).toBeVisible();
  await expect(notion.getByRole("button", { name: "登录" })).toBeVisible();

  const uvx = page.locator("div.group", { hasText: "UVX Thing" });
  await expect(uvx.getByText("无法启动：找不到命令")).toBeVisible();
  await expect(uvx.getByText("连接失败")).toHaveCount(0);
  await expect(uvx.getByRole("button", { name: "登录" })).toHaveCount(0);
});

test("登录等待期间可取消：回到登录按钮，且不弹错误条", async ({ page }) => {
  await gotoServers(page);
  const notion = page.locator("div.group", { hasText: "Notion" });
  await notion.getByRole("button", { name: "登录" }).click();
  await expect(notion.getByRole("button", { name: "等待浏览器授权…" })).toBeDisabled();
  expect(await calls(page)).toEqual([["login", "srv_notion"]]);

  await notion.getByRole("button", { name: "取消" }).click();
  await expect(notion.getByRole("button", { name: "登录" })).toBeVisible();
  expect(await calls(page)).toEqual([
    ["login", "srv_notion"],
    ["cancel", "srv_notion"],
  ]);
  await expect(page.getByText("登录已取消")).toHaveCount(0);
});

test("登录成功后翻为运行中，行尾菜单出现退出登录；退出后回到需要登录", async ({ page }) => {
  await gotoServers(page);
  const notion = page.locator("div.group", { hasText: "Notion" });
  await notion.getByRole("button", { name: "登录" }).click();
  // 后端完成授权：Hub 已拿到令牌并重连。
  await page.evaluate(() => {
    const w = window as unknown as {
      __E2E_STATE__: { notion: unknown };
      __E2E_FINISH_LOGIN__: () => void;
    };
    w.__E2E_STATE__.notion = {
      status: "ready",
      signedIn: true,
      lastErrorKind: null,
      lastError: null,
    };
    w.__E2E_FINISH_LOGIN__();
  });
  await expect(notion.getByText("运行中")).toBeVisible();
  await expect(notion.getByRole("button", { name: "登录" })).toHaveCount(0);

  await notion.getByRole("button", { name: /更多操作/ }).click();
  await page.getByRole("menuitem", { name: "退出登录" }).click();
  await expect(notion.getByText("需要登录")).toBeVisible();
  expect(await calls(page)).toContainEqual(["logout", "srv_notion"]);
});

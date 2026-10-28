import { expect, type Page, test } from "./support";

/**
 * 按地址查找 MCP 服务器（ADR-025，mock Tauri IPC）：
 * 粘贴地址 → 出现候选 → 「使用」只预填表单、不保存；过期响应不覆盖新结果；失败给可读原因。
 */

const CONTEXT7_NPM = {
  id: "io.github.upstash/context7#1",
  source: "registry",
  name: "context7",
  title: null,
  description: "Up-to-date docs for any library",
  namespace: "io.github.upstash",
  repositoryUrl: "https://github.com/upstash/context7",
  websiteUrl: null,
  exact: true,
  install: {
    kind: "stdio",
    runtime: "node",
    command: "npx",
    args: ["-y", "@upstash/context7-mcp@4.1.1"],
    env: [
      { name: "CONTEXT7_API_KEY", value: "", secret: true, required: false, description: null },
    ],
  },
  todo: [],
};

const CONTEXT7_REMOTE = {
  ...CONTEXT7_NPM,
  id: "io.github.upstash/context7#0",
  install: { kind: "remote", transport: "http", url: "https://mcp.context7.com/mcp", headers: [] },
};

const NUGET = {
  ...CONTEXT7_NPM,
  id: "io.github.fieldcure/filesystem#0",
  name: "filesystem",
  namespace: "io.github.fieldcure",
  exact: false,
  install: { kind: "unsupported", reason: "暂不支持 NuGet 类型的包" },
};

async function gotoAddDialog(page: Page) {
  await page.addInitScript(`
    window.__E2E_CALLS__ = [];
    const exact = ${JSON.stringify([CONTEXT7_REMOTE, CONTEXT7_NPM])};
    const similar = ${JSON.stringify([NUGET])};
    const stale = { ...exact[1], id: "stale#0", name: "stale-one", exact: true };
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd, args) => {
        if (cmd === "servers_list") return [];
        if (cmd === "hub_status") {
          return { name: "mcpane-hub", serverVersion: "0.0.1", protocolVersion: "2026-07-28",
            legacyProtocolVersion: "2025-11-25", endpoint: "http://127.0.0.1:24860/mcp",
            uptimeSecs: 1, connections: [] };
        }
        if (cmd === "hub_diagnostics") {
          return { downstreams: {}, catalogRevision: "r", catalogTools: 0, workspaces: {} };
        }
        if (cmd === "mcp_clients_status") return [];
        if (cmd === "server_discover") {
          window.__E2E_CALLS__.push(["discover", args.query]);
          if (args.query.includes("boom")) {
            throw "无法访问官方 MCP Registry，请检查网络或在设置里配置代理";
          }
          if (args.query.includes("slow")) {
            await new Promise((r) => setTimeout(r, 900));
            return { candidates: [stale], notices: [] };
          }
          if (args.query.includes("unlisted")) {
            return { candidates: [], notices: ["repositoryNotListed"] };
          }
          return { candidates: [...exact, ...similar], notices: [] };
        }
        window.__E2E_CALLS__.push([cmd]);
        if (cmd === "server_upsert" || cmd === "server_test") {
          throw new Error("按地址查找不应直接保存或测试: " + cmd);
        }
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  await page.getByRole("button", { name: "MCP 服务器", exact: true }).click();
  await page.getByRole("button", { name: "添加 MCP 服务器" }).first().click();
  return page.getByRole("dialog", { name: "添加 MCP 服务器" });
}

const calls = (page: Page) =>
  page.evaluate(() => (window as unknown as { __E2E_CALLS__: unknown[][] }).__E2E_CALLS__);

test("粘贴地址：精确匹配置顶，「使用」只预填表单、不保存", async ({ page }) => {
  const dialog = await gotoAddDialog(page);
  const panel = dialog.getByRole("region", { name: "按地址查找" });
  await dialog.getByRole("searchbox", { name: "按地址查找" }).fill("github.com/upstash/context7");
  await dialog.getByRole("button", { name: "查找", exact: true }).click();

  await expect(dialog.getByText("地址一致")).toHaveCount(2);
  await expect(dialog.getByText("相近结果")).toBeVisible();
  await expect(panel.getByText("需要 Node.js")).toBeVisible();
  await expect(dialog.getByText("可选环境变量（未预填）：CONTEXT7_API_KEY")).toBeVisible();
  await expect(dialog.getByText("npx -y @upstash/context7-mcp@4.1.1")).toBeVisible();
  await expect(dialog.getByText("结果来自公共 Registry")).toBeVisible();

  await dialog.getByRole("button", { name: /^使用 context7（npx / }).click();
  await expect(dialog.getByRole("tab", { name: "手动创建" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(dialog.getByPlaceholder("如 memory")).toHaveValue("context7");
  await expect(dialog.getByPlaceholder("npx")).toHaveValue("npx");
  await expect(dialog.getByPlaceholder("-y @modelcontextprotocol/server-memory")).toHaveValue(
    "-y @upstash/context7-mcp@4.1.1",
  );
  // 可选且无值的环境变量不预填成空行。
  await expect(dialog.getByPlaceholder("名称，如 API_KEY")).toHaveCount(0);
  // 全程没有保存 / 测试调用。
  const names = (await calls(page)).map((c) => c[0]);
  expect(names).toContain("discover");
  expect(names).not.toContain("server_upsert");
  expect(names).not.toContain("server_test");
});

test("远程候选落到远程服务器页签", async ({ page }) => {
  const dialog = await gotoAddDialog(page);
  await dialog.getByRole("searchbox", { name: "按地址查找" }).fill("context7");
  await dialog.getByRole("searchbox", { name: "按地址查找" }).press("Enter");
  await dialog
    .getByRole("button", { name: /^使用 context7（https:\/\/mcp\.context7\.com\/mcp）/ })
    .click();
  await expect(dialog.getByRole("tab", { name: "远程服务器" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await expect(dialog.getByPlaceholder("https://example.com/mcp")).toHaveValue(
    "https://mcp.context7.com/mcp",
  );
});

test("暂不支持的候选说明原因且没有「使用」按钮", async ({ page }) => {
  const dialog = await gotoAddDialog(page);
  await dialog.getByRole("searchbox", { name: "按地址查找" }).fill("filesystem");
  await dialog.getByRole("searchbox", { name: "按地址查找" }).press("Enter");
  await expect(dialog.getByText("暂不支持 NuGet 类型的包")).toBeVisible();
  await expect(dialog.getByRole("button", { name: /^使用 filesystem（/ })).toHaveCount(0);
});

test("过期响应不覆盖新结果", async ({ page }) => {
  const dialog = await gotoAddDialog(page);
  const box = dialog.getByRole("searchbox", { name: "按地址查找" });
  await box.fill("slow");
  await box.press("Enter");
  await box.fill("context7");
  await box.press("Enter");
  await expect(dialog.getByText("相近结果")).toBeVisible();
  // 慢的那次 900ms 后才返回：等它返回后仍不该出现。
  await page.waitForTimeout(1300);
  await expect(dialog.getByText("stale-one")).toHaveCount(0);
});

test("仓库未登记：给出提示而不是瞎猜命令", async ({ page }) => {
  const dialog = await gotoAddDialog(page);
  const box = dialog.getByRole("searchbox", { name: "按地址查找" });
  await box.fill("github.com/someone/unlisted");
  await box.press("Enter");
  await expect(dialog.getByText("官方 Registry 里没有这个仓库")).toBeVisible();
  await expect(dialog.getByRole("button", { name: /^使用 .+（/ })).toHaveCount(0);
});

test("查询失败：显示可读原因", async ({ page }) => {
  const dialog = await gotoAddDialog(page);
  const box = dialog.getByRole("searchbox", { name: "按地址查找" });
  await box.fill("boom");
  await box.press("Enter");
  await expect(dialog.getByRole("alert")).toContainText("无法访问官方 MCP Registry");
});

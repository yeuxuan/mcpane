import { expect, type Page, test } from "./support";

/**
 * 按模型设置价格（ADR-026，mock Tauri IPC）：
 * 无定价的模型填价后变为有成本并带「自定义」标记；有官方价的模型可按倍率套用；
 * 「恢复默认」清除覆盖。口径写明「按模型、不区分供应商」。
 */

const OFFICIAL = { input: 3, output: 15, cacheRead: 0.3, cacheCreation: 3.75 };

async function gotoUsage(page: Page) {
  await page.addInitScript(`
    const official = ${JSON.stringify(OFFICIAL)};
    // 后端状态：每个模型的官方价与当前自定义价。
    const state = {
      "claude-sonnet-5": { official, custom: null },
      "mystery-model-9000": { official: null, custom: null },
    };
    window.__E2E_CALLS__ = [];
    const row = (key) => {
      const s = state[key];
      const priced = !!(s.official || s.custom);
      const p = s.custom || s.official;
      return {
        key, displayName: key === "claude-sonnet-5" ? "Claude Sonnet 5" : null,
        events: 3, inputTokens: 1000000, outputTokens: 1000000, cacheReadTokens: 0,
        cacheCreationTokens: 0, costUsd: p ? p.input + p.output : 0,
        priced, overridden: !!s.custom,
      };
    };
    window.__TAURI_INTERNALS__ = {
      transformCallback: (cb) => cb,
      invoke: async (cmd, args) => {
        switch (cmd) {
          case "usage_sync": return { scanned: 0, inserted: 0, errors: [] };
          case "usage_summary":
            return { events: 6, inputTokens: 2000000, outputTokens: 2000000, cacheReadTokens: 0,
              cacheCreationTokens: 0, costUsd: 18, unpricedEvents: state["mystery-model-9000"].custom ? 0 : 3 };
          case "usage_trend": return [];
          case "usage_by_model": return Object.keys(state).map(row);
          case "usage_by_project": return [];
          case "usage_by_provider": return [];
          case "usage_price_info": return structuredClone(state[args.modelId]);
          case "usage_price_override_set":
            window.__E2E_CALLS__.push(["set", args.modelId, args.prices]);
            state[args.modelId].custom = args.prices;
            return null;
          case "usage_price_override_clear":
            window.__E2E_CALLS__.push(["clear", args.modelId]);
            state[args.modelId].custom = null;
            return null;
          case "providers_list": return [];
          case "provider_current": return {};
          case "servers_list": return [];
          case "workspaces_list": return [{ id: "ws_default", name: "默认工作区" }];
          case "hub_status":
            return { name: "mcpane-hub", serverVersion: "0.0.1", protocolVersion: "2026-07-28",
              legacyProtocolVersion: "2025-11-25", endpoint: "http://127.0.0.1:24860/mcp",
              uptimeSecs: 1, connections: [] };
          case "hub_diagnostics":
            return { downstreams: {}, catalogRevision: "r", catalogTools: 0, workspaces: {} };
          case "mcp_clients_status": return [];
        }
        throw new Error("e2e 未 mock 的命令: " + cmd);
      },
    };
  `);
  await page.goto("/");
  await page.getByRole("button", { name: "用量", exact: true }).click();
  await expect(page.getByRole("heading", { name: "用量" })).toBeVisible();
}

const calls = (page: Page) =>
  page.evaluate(() => (window as unknown as { __E2E_CALLS__: unknown[][] }).__E2E_CALLS__);

test("无定价的模型：填价后有成本、带「自定义」标记，并声明按模型生效", async ({ page }) => {
  await gotoUsage(page);
  await expect(page.getByText("无定价")).toBeVisible();

  await page.getByRole("button", { name: "设置 mystery-model-9000 的价格" }).click();
  const dialog = page.getByRole("dialog", { name: /设置价格/ });
  await expect(dialog.getByText("按模型生效，不区分供应商")).toBeVisible();
  await expect(dialog.getByText("价目表里没有这个模型")).toBeVisible();
  // 没有官方价就没有倍率快捷方式。
  await expect(dialog.getByText("按官方价的倍率")).toHaveCount(0);

  const save = dialog.getByRole("button", { name: "保存" });
  await expect(save).toBeDisabled();
  await dialog.getByRole("textbox", { name: "输入" }).fill("1.5");
  await dialog.getByRole("textbox", { name: "输出" }).fill("2.5");
  await expect(save).toBeEnabled();
  await save.click();

  await expect(dialog).toHaveCount(0);
  expect(await calls(page)).toEqual([
    ["set", "mystery-model-9000", { input: 1.5, output: 2.5, cacheRead: 0, cacheCreation: 0 }],
  ]);
  // 列表刷新：无定价提示消失、成本出现、出现「自定义」。
  await expect(page.getByText("无定价")).toHaveCount(0);
  await expect(page.getByText("自定义")).toBeVisible();
  await expect(page.getByText("$4.00")).toBeVisible();
});

test("有官方价：按倍率套用，填出四项且可保存；恢复默认清除覆盖", async ({ page }) => {
  await gotoUsage(page);
  await page.getByRole("button", { name: "设置 claude-sonnet-5 的价格" }).click();
  const dialog = page.getByRole("dialog", { name: /设置价格/ });
  // 打开时默认填官方价。
  await expect(dialog.getByRole("textbox", { name: "输入" })).toHaveValue("3");
  await expect(dialog.getByText("官方价 15")).toBeVisible();
  // 没有覆盖时不显示「恢复默认」。
  await expect(dialog.getByRole("button", { name: "恢复默认" })).toHaveCount(0);

  await dialog.getByPlaceholder("0.6").fill("0.6");
  await dialog.getByRole("button", { name: "套用" }).click();
  await expect(dialog.getByRole("textbox", { name: "输入" })).toHaveValue("1.8");
  await expect(dialog.getByRole("textbox", { name: "输出" })).toHaveValue("9");
  await expect(dialog.getByRole("textbox", { name: "缓存读取" })).toHaveValue("0.18");
  await expect(dialog.getByRole("textbox", { name: "缓存写入" })).toHaveValue("2.25");
  await dialog.getByRole("button", { name: "保存" }).click();
  await expect(dialog).toHaveCount(0);
  expect(await calls(page)).toEqual([
    ["set", "claude-sonnet-5", { input: 1.8, output: 9, cacheRead: 0.18, cacheCreation: 2.25 }],
  ]);

  // 再打开：显示自定义值，并出现「恢复默认」。
  await page.getByRole("button", { name: "设置 claude-sonnet-5 的价格" }).click();
  await expect(dialog.getByRole("textbox", { name: "输入" })).toHaveValue("1.8");
  await dialog.getByRole("button", { name: "恢复默认" }).click();
  await expect(dialog).toHaveCount(0);
  expect((await calls(page)).at(-1)).toEqual(["clear", "claude-sonnet-5"]);
  await expect(page.getByText("自定义")).toHaveCount(0);
});

test("非法输入：提示并禁用保存，不发请求", async ({ page }) => {
  await gotoUsage(page);
  await page.getByRole("button", { name: "设置 mystery-model-9000 的价格" }).click();
  const dialog = page.getByRole("dialog", { name: /设置价格/ });
  await dialog.getByRole("textbox", { name: "输入" }).fill("1");
  await dialog.getByRole("textbox", { name: "输出" }).fill("-3");
  await expect(dialog.getByText("请填 0 到 1000000 之间的数字")).toBeVisible();
  await expect(dialog.getByRole("button", { name: "保存" })).toBeDisabled();
  await dialog.getByRole("textbox", { name: "输出" }).fill("1000001");
  await expect(dialog.getByRole("button", { name: "保存" })).toBeDisabled();
  expect(await calls(page)).toEqual([]);
});

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import type { ServerDef, ServerExport } from "../lib/ipc";
import { ServerExportModal } from "./ServerExportModal";

/** `${NAME}` 占位符；写成普通字符串字面量会被 lint 当成忘了用模板字符串。 */
const placeholder = (name: string) => `\${${name}}`;

function serverDef(overrides: Partial<ServerDef>): ServerDef {
  return {
    id: "srv",
    slug: "demo",
    name: "demo",
    transport: "stdio",
    command: "npx",
    args: [],
    env: {},
    cwd: null,
    url: null,
    bearer: null,
    bearerEnv: null,
    headers: {},
    runtimePolicy: "shared",
    enabled: true,
    ...overrides,
  };
}

const SERVERS = [
  serverDef({ id: "srv_memory", name: "memory" }),
  serverDef({ id: "srv_github", name: "GitHub", env: { GITHUB_TOKEN: "ghp_x" } }),
  serverDef({
    id: "srv_remote",
    name: "Remote",
    transport: "http",
    command: null,
    url: "https://x.dev/mcp",
  }),
];

/** 按 id 拼一份「后端」导出：GitHub 带一处替换，其余没有。 */
function fakeExport(ids: string[]): ServerExport {
  const names = SERVERS.filter((s) => ids.includes(s.id)).map((s) => s.name);
  return {
    json: JSON.stringify({ mcpServers: Object.fromEntries(names.map((n) => [n, {}])) }),
    redactions: ids.includes("srv_github")
      ? [{ server: "GitHub", field: "env", variable: "GITHUB_TOKEN" }]
      : [],
  };
}

const calls: { cmd: string; args: { ids: string[] } }[] = [];
// 工厂而非现成的 Promise：提前造好的 reject 在被 await 之前就会报「未处理」。
let saveResult: () => Promise<string>;

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args: { ids: string[] }) => {
    calls.push({ cmd, args });
    if (cmd === "servers_export") return Promise.resolve(fakeExport(args.ids));
    if (cmd === "servers_export_save") return saveResult();
    return Promise.resolve(null);
  },
}));

function renderModal(initialIds: string[]) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={client}>
      <ServerExportModal servers={SERVERS} initialIds={initialIds} onClose={() => {}} />
    </QueryClientProvider>,
  );
}

const lastExportIds = () => calls.filter((c) => c.cmd === "servers_export").at(-1)?.args.ids;

beforeEach(() => {
  calls.length = 0;
  saveResult = () => Promise.resolve("/Users/me/Downloads/mcpane-servers.json");
});

test("行尾菜单打开时只勾选那一行，替换明细逐条列出变量名与位置", async () => {
  renderModal(["srv_github"]);

  expect(await screen.findByText("已将 1 处凭据替换为占位符")).toBeInTheDocument();
  expect(lastExportIds()).toEqual(["srv_github"]);
  expect(screen.getByRole("checkbox", { name: "导出 GitHub" })).toBeChecked();
  expect(screen.getByRole("checkbox", { name: "导出 memory" })).not.toBeChecked();
  expect(screen.getByText("已选 1 / 3")).toBeInTheDocument();
  expect(screen.getByText("环境变量")).toBeInTheDocument();
  expect(screen.getByText(placeholder("GITHUB_TOKEN"))).toBeInTheDocument();
});

test("全选后按列表顺序导出，复制的是后端生成的那份文本", async () => {
  const user = userEvent.setup();
  renderModal(["srv_remote"]);
  await screen.findByText(/未发现需要替换的凭据/);

  await user.click(screen.getByRole("checkbox", { name: "全选" }));
  await waitFor(() => expect(lastExportIds()).toEqual(["srv_memory", "srv_github", "srv_remote"]));
  await screen.findByText("已将 1 处凭据替换为占位符");

  await user.click(screen.getByRole("button", { name: "复制 JSON" }));
  expect(await navigator.clipboard.readText()).toBe(
    fakeExport(["srv_memory", "srv_github", "srv_remote"]).json,
  );
  expect(screen.getByRole("button", { name: "已复制" })).toBeInTheDocument();
});

test("保存为文件后显示落盘路径；失败说清原因", async () => {
  const user = userEvent.setup();
  renderModal(["srv_memory"]);
  await screen.findByText(/未发现需要替换的凭据/);

  await user.click(screen.getByRole("button", { name: "保存为文件" }));
  expect(
    await screen.findByText("已保存到 /Users/me/Downloads/mcpane-servers.json"),
  ).toBeInTheDocument();
  expect(calls.find((c) => c.cmd === "servers_export_save")?.args).toEqual({ ids: ["srv_memory"] });

  saveResult = () => Promise.reject("找不到「下载」文件夹：no such dir");
  await user.click(screen.getByRole("button", { name: "保存为文件" }));
  expect(
    await screen.findByText("保存失败：找不到「下载」文件夹：no such dir"),
  ).toBeInTheDocument();
});

test("一个都不选时两个出口都禁用，并说明原因", async () => {
  const user = userEvent.setup();
  renderModal(["srv_memory"]);
  await screen.findByText(/未发现需要替换的凭据/);

  await user.click(screen.getByRole("checkbox", { name: "导出 memory" }));
  expect(screen.getByText("至少选择一个服务器")).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "复制 JSON" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "保存为文件" })).toBeDisabled();
});

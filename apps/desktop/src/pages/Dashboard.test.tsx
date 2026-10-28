import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import type { PageKey } from "../components/Shell";
import "../lib/i18n";
import { Dashboard } from "./Dashboard";

// 显式 unknown：离线用例要 mock 出 hub_process_state 这类不在默认分支里的返回形状，
// 让 TS 从 switch 反推联合类型的话，任何新形状都会被判为不可赋值。
const defaultInvoke = async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case "hub_status":
      return {
        name: "mcpane-hub",
        serverVersion: "0.0.1",
        protocolVersion: "2026-07-28",
        legacyProtocolVersion: "2025-11-25",
        endpoint: "http://127.0.0.1:24860/mcp",
        uptimeSecs: 5023,
        connections: [
          {
            clientConnectionId: "c1",
            clientType: "claude",
            tokenPrefix: "tk",
            enabled: true,
            defaultWorkspaceId: null,
            toolExposureMode: "auto",
          },
        ],
      };
    case "hub_diagnostics":
      return {
        catalogTools: 51,
        catalogRevision: "rev1",
        downstreams: { s1: { slug: "demo", status: "ready", crashCount: 0 } },
        workspaces: {},
      };
    case "detect_clients":
      return {
        claude: {
          installed: true,
          version: "2.1.207",
          connected: true,
          configPath: "",
          tokenEnv: "",
        },
        codex: { installed: false, version: null, connected: false, configPath: "", tokenEnv: "" },
      };
    case "servers_list":
      return [
        {
          id: "s1",
          slug: "demo",
          name: "Demo Server",
          transport: "stdio",
          command: "npx",
          args: [],
          env: {},
          cwd: null,
          url: null,
          bearer: null,
          bearerEnv: null,
          headers: {},
          runtimePolicy: "",
          enabled: true,
        },
      ];
    default:
      return null;
  }
};

const invokeMock = vi.fn(defaultInvoke);

afterEach(() => {
  invokeMock.mockImplementation(defaultInvoke);
  invokeMock.mockClear();
  localStorage.clear();
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async () => () => {},
}));

function renderDashboard(onNavigate?: (page: PageKey) => void) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <Dashboard onNavigate={onNavigate} />
    </QueryClientProvider>,
  );
}

// 回归用例：运行时长必须格式化（5023s → 1 小时 23 分），不能渲染原始秒数。
test("运行时长按两级单位格式化显示", async () => {
  renderDashboard();
  expect(await screen.findByText("已运行 1 小时 23 分")).toBeInTheDocument();
  expect(screen.queryByText("5023s")).not.toBeInTheDocument();
});

// 协议/版本是查阅一次的常量，归设置 → 关于（见 Settings.test.tsx）；首页不留它们。
test("首页不再展示协议与版本常量", async () => {
  renderDashboard();
  await screen.findByText("已运行 1 小时 23 分");
  expect(screen.queryByText("MCP 2026-07-28 · 兼容 2025-11-25 及更早")).not.toBeInTheDocument();
  expect(screen.queryByText("v0.0.1")).not.toBeInTheDocument();
});

// 端点是这张卡唯一要被拿走的信息：铭牌整块即复制按钮，点哪都写进剪贴板。
test("端点铭牌整块可点复制", async () => {
  const writeText = vi.fn(async () => {});
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
  renderDashboard();
  const plate = await screen.findByRole("button", { name: /AI 工具连接地址/ });
  expect(plate).toHaveTextContent("http://127.0.0.1:24860/mcp");
  await userEvent.click(plate);
  expect(writeText).toHaveBeenCalledWith("http://127.0.0.1:24860/mcp");
  expect(await screen.findByText("已复制")).toBeInTheDocument();
  vi.unstubAllGlobals();
});

test("实况轨按数据流向展示三站读数", async () => {
  renderDashboard();
  expect(await screen.findByText("个服务正常")).toBeInTheDocument();
  expect(screen.getByText("个工具可用")).toBeInTheDocument();
  expect(screen.getByText("个客户端在线")).toBeInTheDocument();
  expect(await screen.findByText("51")).toBeInTheDocument();
  // 下游 1/1 就绪 + 客户端 1/1 活跃
  expect(await screen.findAllByText("1/1")).toHaveLength(2);
});

// Hub 不可达时铭牌不给地址：复制一个连不上的端点只会让用户把客户端配错。
// 「已在设置里关掉」是唯一有一键出路的离线原因，那一站才给跳转。
test("Hub 关闭时铭牌不展示端点，改给设置入口", async () => {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "hub_status") throw new Error("hub unreachable");
    if (cmd === "hub_process_state") return { state: "disabled" };
    return defaultInvoke(cmd);
  });
  const onNavigate = vi.fn();
  renderDashboard(onNavigate);
  expect(await screen.findByText("MCP Hub 已在设置中关闭")).toBeInTheDocument();
  expect(screen.queryByText("http://127.0.0.1:24860/mcp")).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "打开设置" }));
  expect(onNavigate).toHaveBeenCalledWith("settings");
});

test("下游服务卡展示各服务运行状态", async () => {
  renderDashboard();
  expect(await screen.findByText("Demo Server")).toBeInTheDocument();
  expect(await screen.findByText("运行中")).toBeInTheDocument();
});

// 回归：这两个查询都是 retry:false，失败后此前落进「加载中」分支永久定格——
// 用户看到的不是「读取失败」，而是一句平静且错误的事实陈述。
test("客户端检测失败时如实报错，不停在「检测中」", async () => {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "detect_clients") throw new Error("spawn EACCES");
    return defaultInvoke(cmd);
  });
  renderDashboard();
  expect(await screen.findByText(/检测客户端失败/)).toBeInTheDocument();
  expect(screen.queryByText("检测中…")).not.toBeInTheDocument();
});

test("服务器列表读取失败时如实报错，不显示成「尚未启用任何 MCP 服务」", async () => {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "servers_list") throw new Error("database is locked");
    return defaultInvoke(cmd);
  });
  renderDashboard();
  expect(await screen.findByText(/读取服务器列表失败/)).toBeInTheDocument();
  expect(screen.queryByText(/尚未启用任何 MCP 服务/)).not.toBeInTheDocument();
});

// 占用者身份探测（lsof + ps）早就查得到，此前只用于内部判断是不是自家僵尸 hub，
// 从不外传——界面只说「被其他程序占用」，用户接下来该找谁全靠自己猜。
test("端口被占用时指名占用它的进程与 PID", async () => {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "hub_status") throw new Error("hub unreachable");
    if (cmd === "hub_process_state") {
      return { state: "failed", reason: "portOccupied", occupant: { name: "node", pid: 4321 } };
    }
    return defaultInvoke(cmd);
  });
  renderDashboard();
  expect(await screen.findByText("端口 24860 被 node（PID 4321）占用")).toBeInTheDocument();
});

// ---------- 当前供应商卡 ----------

const profile = (id: string, app: string, name: string, baseUrl: string | null, sortOrder = 0) => ({
  id,
  app,
  name,
  baseUrl,
  category: baseUrl ? "relay" : "official",
  sortOrder,
});

function withProviders(extra?: (cmd: string, args?: unknown) => unknown) {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const hit = extra?.(cmd, args);
    if (hit !== undefined) return hit;
    switch (cmd) {
      case "provider_clients_installed":
        return { claude: true, codex: true, opencode: true, gemini: false };
      case "providers_list":
        return [
          profile("official-claude", "claude", "Claude 官方", null),
          profile("ds", "claude", "DeepSeek", "https://api.deepseek.com/anthropic", 1),
          profile("official-codex", "codex", "Codex 官方", null),
          profile("oc", "opencode", "OpenCode 某家", "https://x.example"),
        ];
      case "provider_current":
        return { claude: "official-claude", codex: "official-codex" };
      case "usage_by_provider":
        return [{ key: "claude", costUsd: 3.456, priced: true }];
      case "provider_switch":
        return { written: true, warnings: [] };
      default:
        return defaultInvoke(cmd, args);
    }
  });
}

test("当前供应商卡：列已安装的非 additive 客户端与近 24 小时花费", async () => {
  withProviders();
  renderDashboard();
  expect(await screen.findByText("Claude Code")).toBeInTheDocument();
  expect(screen.getByRole("combobox", { name: /Claude Code/ })).toHaveTextContent("Claude 官方");
  expect(screen.getByRole("combobox", { name: /Codex/ })).toHaveTextContent("Codex 官方");
  // additive（OpenCode）没有「当前生效项」，未安装的（Gemini）也不列。
  expect(screen.queryByRole("combobox", { name: /OpenCode/ })).not.toBeInTheDocument();
  expect(screen.queryByRole("combobox", { name: /Gemini/ })).not.toBeInTheDocument();
  expect(screen.getByText("近 24 小时约 $3.46")).toBeInTheDocument();
});

test("当前供应商卡：按设置里的选择显示", async () => {
  localStorage.setItem("mcpane:dashboard-apps", JSON.stringify(["claude", "gemini"]));
  withProviders((cmd) => {
    if (cmd === "provider_clients_installed")
      return { claude: true, codex: true, gemini: true, "claude-desktop": true };
    if (cmd === "providers_list")
      return [
        profile("official-claude", "claude", "Claude 官方", null),
        profile("g1", "gemini", "Gemini 某家", "https://x.example"),
        profile("official-codex", "codex", "Codex 官方", null),
      ];
    if (cmd === "provider_current") return { gemini: "g1", codex: "official-codex" };
    return undefined;
  });
  renderDashboard();
  // 只显示设置里选的两个。
  expect(await screen.findByRole("combobox", { name: /Gemini/ })).toHaveTextContent("Gemini 某家");
  expect(screen.getByRole("combobox", { name: /Claude Code/ })).toBeInTheDocument();
  expect(screen.queryByRole("combobox", { name: /Codex/ })).not.toBeInTheDocument();
  expect(screen.queryByRole("combobox", { name: /Claude Desktop/ })).not.toBeInTheDocument();
});

test("当前供应商卡不查询、不显示额度", async () => {
  withProviders();
  renderDashboard();
  expect(await screen.findByRole("combobox", { name: /Claude Code/ })).toBeInTheDocument();
  expect(screen.queryByRole("meter")).not.toBeInTheDocument();
  expect(screen.queryByText(/剩余/)).not.toBeInTheDocument();
  expect(invokeMock).not.toHaveBeenCalledWith("provider_quota", expect.anything());
});

test("下拉切换供应商调用 provider_switch", async () => {
  withProviders();
  renderDashboard();
  // jsdom 没有指针捕获与滚动：Radix Select 用键盘打开，并补这两个桩。
  Element.prototype.hasPointerCapture ??= () => false;
  Element.prototype.scrollIntoView ??= () => {};
  (await screen.findByRole("combobox", { name: /Claude Code/ })).focus();
  await userEvent.keyboard("{ArrowDown}");
  await userEvent.click(await screen.findByRole("option", { name: "DeepSeek" }));
  expect(invokeMock).toHaveBeenCalledWith("provider_switch", { app: "claude", id: "ds" });
});

test("客户端卡用真实检测数据，已连接排前", async () => {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) =>
    cmd === "mcp_clients_status"
      ? [
          {
            id: "cursor",
            display: "Cursor",
            installed: true,
            connected: true,
            tokenEnv: null,
            configPath: "",
          },
          {
            id: "devin",
            display: "Devin",
            installed: false,
            connected: false,
            tokenEnv: null,
            configPath: "",
          },
        ]
      : defaultInvoke(cmd, args),
  );
  renderDashboard();
  expect(await screen.findByText("Cursor")).toBeInTheDocument();
  expect(screen.getByText("Claude Code")).toBeInTheDocument();
  // 未安装的不列；Codex 在默认 mock 里未安装。
  expect(screen.queryByText("Devin")).not.toBeInTheDocument();
  expect(screen.queryByText("Codex")).not.toBeInTheDocument();
});

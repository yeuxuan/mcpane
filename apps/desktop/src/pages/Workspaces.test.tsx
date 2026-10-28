import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { WorkspaceProvider } from "../lib/workspace";
import { Workspaces } from "./Workspaces";

let resolveToggle: (() => void) | null = null;

const makeTool = (name: string) => ({
  serverId: "s1",
  serverSlug: "demo",
  originalName: name,
  exposedName: name,
  description: null,
  icons: null as Array<{ src: string }> | null,
  schemaHash: `h_${name}`,
});
let catalogTools = [makeTool("foo")];

const makeTargets = () => [
  {
    clientType: "gemini-cli",
    label: "Gemini CLI",
    path: "/home/u/.gemini/GEMINI.md",
    supported: true,
    delivered: false,
    stale: false,
    unsupportedReason: null,
    steps: [],
    // 落点按文档推定、未实测——UI 必须为此出一句提示。
    pathVerified: false,
    error: null,
  },
  {
    clientType: "claude",
    label: "Claude Code",
    path: "/home/u/.claude/CLAUDE.md",
    supported: true,
    delivered: false,
    stale: false,
    unsupportedReason: null,
    steps: [],
    pathVerified: true,
    error: null,
  },
  {
    clientType: "cursor",
    label: "Cursor",
    path: null,
    supported: false,
    delivered: false,
    stale: false,
    unsupportedReason: "Cursor 的全局规则存在应用内部存储、不是文件，MCPane 写不了。",
    pathVerified: true,
    steps: [
      "复制右侧的准则文本。",
      "打开 Cursor 设置，找到 Rules，粘贴进 User Rules。",
      "只想对当前项目生效的话，改为在项目根目录建 .cursor/rules/mcpane.mdc。",
    ],
    error: null,
  },
];
let guidanceTargets = makeTargets();
let usageGuidance = "";
let toolOverrides: Array<{
  serverId: string;
  originalName: string;
  enabled: boolean;
  alias: string | null;
  descriptionSuffix: string | null;
}> = [];

const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
  switch (cmd) {
    case "workspaces_list":
      return [];
    case "servers_list":
      return [{ id: "s1", name: "Demo Server", enabled: true }];
    case "catalog_list":
      return { id: "rev1", workspaceId: "ws_default", tools: catalogTools };
    case "workspace_tool_discovery":
      return false;
    case "workspace_usage_guidance":
      return usageGuidance;
    case "workspace_tool_overrides":
      return toolOverrides;
    case "guidance_targets":
      return guidanceTargets;
    case "guidance_preview":
      return usageGuidance ? `## MCP 工具使用准则\n\n${usageGuidance}\n` : "";
    case "guidance_deliver":
    case "guidance_revoke":
      return null;
    case "workspace_tool_toggle":
      // 模拟 workspace_tool_toggle 里同步 await 的 ReloadServers 卡住不返回，
      // 直到测试手动 resolveToggle() 才算「重连完成」。
      return new Promise<void>((resolve) => {
        resolveToggle = resolve;
      });
    default:
      return null;
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

function renderWorkspaces() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <WorkspaceProvider>
        <Workspaces />
      </WorkspaceProvider>
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  resolveToggle = null;
  catalogTools = [makeTool("foo")];
  guidanceTargets = makeTargets();
  usageGuidance = "";
  toolOverrides = [];
  invokeMock.mockClear();
});

// 回归用例：workspace_tool_toggle 会同步等 Hub 侧 ReloadServers 把下游服务器重连一遍，
// 这期间按钮必须持续显示 loading 并禁止重复点击，直到真正拿到响应。
test("禁用工具：ReloadServers 未返回前按钮持续转圈，返回后才收起", async () => {
  const user = userEvent.setup();
  renderWorkspaces();

  const disableButton = await screen.findByRole("button", { name: "禁用" });
  await user.click(disableButton);

  await waitFor(() => expect(disableButton).toBeDisabled());
  expect(disableButton).toHaveAttribute("aria-busy", "true");

  resolveToggle?.();
  await waitFor(() => expect(disableButton).not.toBeDisabled());
});

// 工具数达到阈值且发现模式未开时提示一键开启；不足阈值不提示（不做静默切换）。
test("全量工具超过阈值时提示开启动态发现，一键开启", async () => {
  catalogTools = Array.from({ length: 30 }, (_, i) => makeTool(`tool_${i}`));
  const user = userEvent.setup();
  renderWorkspaces();

  const enableButton = await screen.findByRole("button", { name: "开启动态发现" });
  expect(screen.getByText(/30 个工具会全量下发/)).toBeInTheDocument();

  await user.click(enableButton);
  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("workspace_tool_discovery_set", {
      workspaceId: null,
      enabled: true,
    }),
  );
});

test("工具数不足阈值时不出现开启提示", async () => {
  renderWorkspaces();
  await screen.findByRole("button", { name: "禁用" });
  expect(screen.queryByRole("button", { name: "开启动态发现" })).toBeNull();
});

test("下游申报的工具图标渲染在工具行", async () => {
  catalogTools = [{ ...makeTool("shot"), icons: [{ src: "https://example.com/shot.png" }] }];
  const { container } = renderWorkspaces();
  await screen.findByRole("button", { name: "禁用" });
  const icon = container.querySelector('img[src="https://example.com/shot.png"]');
  expect(icon).not.toBeNull();
});

// 使用准则（ADR-020 主动性轴）：编辑后保存下发，未改动时保存不可点。
test("使用准则：编辑后保存下发到对应工作区", async () => {
  const user = userEvent.setup();
  renderWorkspaces();

  const textarea = await screen.findByRole("textbox", { name: "使用准则" });
  // 查询未落定时 textarea 处于 disabled，type 会被静默忽略——先等它可用。
  await waitFor(() => expect(textarea).toBeEnabled());
  const save = screen.getByRole("button", { name: "保存" });
  expect(save).toBeDisabled();

  await user.type(textarea, "代码搜索一律先用 fast_context_search");
  expect(save).toBeEnabled();
  await user.click(save);

  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("workspace_usage_guidance_set", {
      workspaceId: null,
      guidance: "代码搜索一律先用 fast_context_search",
    }),
  );
});

// 不支持自动投递的客户端必须照样出现在清单里并说清原因——静默隐藏会让用户
// 以为 MCPane 漏了它，转头去别处找配置入口。
test("投递清单：不支持的客户端也列出，并给出手动配置的去处", async () => {
  renderWorkspaces();
  expect(await screen.findByText("Claude Code")).toBeInTheDocument();
  expect(screen.getByText("Cursor")).toBeInTheDocument();
  // 说清为什么得手动，并给出通往步骤的入口（步骤本身默认收起，见下一个用例）。
  expect(screen.getByText(/MCPane 写不了/)).toBeInTheDocument();
  expect(screen.getByRole("button", { name: /怎么加|How to add/ })).toBeInTheDocument();
  // 自动投递不了的目标不给开关，走复制 + 手动那条路。
  expect(
    screen.queryByRole("switch", { name: /投递到 Cursor|Deliver to Cursor/ }),
  ).not.toBeInTheDocument();
});

// 准则为空时投递没有意义：写进去只会留一个空壳区块。
test("准则为空时投递开关禁用，填了才放行", async () => {
  const { unmount } = renderWorkspaces();
  const off = await screen.findByRole("switch", {
    name: /投递到 Claude Code|Deliver to Claude Code/,
  });
  expect(off).toBeDisabled();
  unmount();

  usageGuidance = "优先用 Hub 的工具";
  renderWorkspaces();
  await waitFor(() =>
    expect(
      screen.getByRole("switch", { name: /投递到 Claude Code|Deliver to Claude Code/ }),
    ).toBeEnabled(),
  );
});

// 触发提示是这次改动的核心通道：保存必须把 descriptionSuffix 真的传下去。
test("触发提示：展开编辑并保存，descriptionSuffix 随请求下发", async () => {
  const user = userEvent.setup();
  renderWorkspaces();
  await user.click(await screen.findByRole("button", { name: /触发提示|Trigger hint/ }));

  const box = screen.getByRole("textbox", { name: /触发提示|Trigger hint/ });
  await user.type(box, "Use when: 需要实时网页时");
  await user.click(screen.getByRole("button", { name: /保存提示|Save hint/ }));

  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("workspace_tool_toggle", {
      input: {
        workspaceId: null,
        serverId: "s1",
        originalName: "foo",
        enabled: true,
        alias: null,
        descriptionSuffix: "Use when: 需要实时网页时",
      },
    }),
  );
  resolveToggle?.();
});

// 光说「不支持」等于把用户丢在半路——必须给能照着做的步骤。
test("手动投递：展开后给出可照做的步骤，含 User Rules 与项目文件两条路", async () => {
  const user = userEvent.setup();
  renderWorkspaces();

  const howTo = await screen.findByRole("button", { name: /怎么加|How to add/ });
  expect(screen.queryByText(/粘贴进 User Rules/)).not.toBeInTheDocument();

  await user.click(howTo);
  expect(screen.getByText(/粘贴进 User Rules/)).toBeInTheDocument();
  expect(screen.getByText(/\.cursor\/rules\/mcpane\.mdc/)).toBeInTheDocument();

  // 再点收起，步骤不再占地方。
  await user.click(screen.getByRole("button", { name: /收起|Hide/ }));
  await waitFor(() => expect(screen.queryByText(/粘贴进 User Rules/)).not.toBeInTheDocument());
});

// 工具行显示的描述里已经带上提示了，编辑框却空着的话，用户只会以为没存上、
// 再写一遍——保存过的值必须回显。
test("触发提示：已保存的值回显进编辑框", async () => {
  const user = userEvent.setup();
  toolOverrides = [
    {
      serverId: "s1",
      originalName: "foo",
      enabled: true,
      alias: null,
      descriptionSuffix: "Use when: 已经存过的提示",
    },
  ];
  renderWorkspaces();
  await user.click(await screen.findByRole("button", { name: /触发提示|Trigger hint/ }));
  expect(screen.getByRole("textbox", { name: /触发提示|Trigger hint/ })).toHaveValue(
    "Use when: 已经存过的提示",
  );
});

// 只展开、不编辑就点保存，不能把已存的内容抹成空串。
test("触发提示：未编辑直接保存，下发的仍是已存内容", async () => {
  const user = userEvent.setup();
  toolOverrides = [
    {
      serverId: "s1",
      originalName: "foo",
      enabled: true,
      alias: null,
      descriptionSuffix: "Use when: 别弄丢我",
    },
  ];
  renderWorkspaces();
  await user.click(await screen.findByRole("button", { name: /触发提示|Trigger hint/ }));
  await user.click(screen.getByRole("button", { name: /保存提示|Save hint/ }));

  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("workspace_tool_toggle", {
      input: expect.objectContaining({ descriptionSuffix: "Use when: 别弄丢我" }),
    }),
  );
  resolveToggle?.();
});

// 落点没实测过就必须说出来：用户勾完开关、以为准则生效了，实际客户端根本不读
// 那个文件——这类「看着成功」的失败最难自查。
test("投递清单：落点未实测的目标带出提示，已实测的不带", async () => {
  renderWorkspaces();
  await screen.findByText("Gemini CLI");

  const notes = screen.getAllByText(/依据官方文档推定|inferred from the official docs/);
  expect(notes).toHaveLength(1);

  // 已实测的 Claude Code 不该跟着一起出提示，否则等于人人有份、等于没说。
  expect(screen.getByText("Claude Code")).toBeInTheDocument();
});

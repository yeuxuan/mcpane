import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { WelcomeWizard } from "./WelcomeWizard";

const detect = vi.fn();
const runImport = vi.fn();

vi.mock("../lib/ipc", () => ({
  ipc: {
    ccswitchDetect: () => detect(),
    ccswitchImport: (providers: boolean, servers: boolean) => runImport(providers, servers),
  },
}));

const FOUND = {
  searched: "/Users/dev/.cc-switch",
  found: {
    path: "/Users/dev/.cc-switch/cc-switch.db",
    providerCount: 3,
    serverCount: 1,
    apps: [{ app: "claude" as const, count: 3 }],
  },
};

function renderWizard() {
  const onNavigate = vi.fn();
  const onDone = vi.fn();
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={queryClient}>
      <WelcomeWizard onNavigate={onNavigate} onDone={onDone} />
    </QueryClientProvider>,
  );
  return { onNavigate, onDone };
}

const NOT_FOUND = { searched: "/Users/dev/.cc-switch", found: null };

beforeEach(() => {
  // 默认按"装过 cc-switch"来：那才是三步齐全的形态。没装的走 NOT_FOUND。
  detect.mockReset().mockResolvedValue(FOUND);
  runImport.mockReset();
});

/** 等第一步把探测结果拿到手——步数与要不要迁移那一步都看它。 */
async function gotoStep2(user: ReturnType<typeof userEvent.setup>) {
  await screen.findByLabelText("第 1 步，共 3 步");
  await user.click(screen.getByRole("button", { name: "下一步" }));
}

test("三步走完，选「配置 AI 供应商」落到供应商页", async () => {
  const user = userEvent.setup();
  const { onNavigate, onDone } = renderWizard();

  expect(screen.getByRole("heading", { name: "欢迎使用 MCPane" })).toBeTruthy();
  await gotoStep2(user);
  expect(screen.getByRole("heading", { name: "导入 cc-switch 配置" })).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "暂不导入，下一步" }));

  await user.click(screen.getByRole("button", { name: /配置 AI 供应商/ }));
  expect(onNavigate).toHaveBeenCalledWith("providers");
  // 直奔任务的这条路要把总览标记成看过，否则关掉向导又被六站的总览接上。
  expect(onDone).toHaveBeenCalledWith(true);
});

test("选「管理 MCP 服务」落到服务器页", async () => {
  const user = userEvent.setup();
  const { onNavigate } = renderWizard();
  await gotoStep2(user);
  await user.click(screen.getByRole("button", { name: "暂不导入，下一步" }));
  await user.click(screen.getByRole("button", { name: /管理 MCP 服务/ }));
  expect(onNavigate).toHaveBeenCalledWith("mcp-servers");
});

test("选「先浏览整体界面」回概览页，总览导览留着讲", async () => {
  const user = userEvent.setup();
  const { onNavigate, onDone } = renderWizard();
  await gotoStep2(user);
  await user.click(screen.getByRole("button", { name: "暂不导入，下一步" }));
  await user.click(screen.getByRole("button", { name: "先浏览整体界面" }));
  expect(onNavigate).toHaveBeenCalledWith("dashboard");
  expect(onDone).toHaveBeenCalledWith(false);
});

test("跳过随时可用", async () => {
  const user = userEvent.setup();
  const { onDone } = renderWizard();
  await user.click(screen.getByRole("button", { name: "跳过" }));
  expect(onDone).toHaveBeenCalledWith(true);
});

/**
 * 没装过 cc-switch 的人（多数新用户）不该被塞一屏「没找到」加一行路径：
 * 那一步他既没得做，还会以为自己少装了什么。整步消失，向导变两步。
 */
test("没装 cc-switch：那一步整个不出现，向导只有两步", async () => {
  const user = userEvent.setup();
  detect.mockResolvedValue(NOT_FOUND);
  const { onNavigate } = renderWizard();

  await screen.findByLabelText("第 1 步，共 2 步");
  await user.click(screen.getByRole("button", { name: "下一步" }));

  expect(screen.queryByRole("heading", { name: "导入 cc-switch 配置" })).toBeNull();
  expect(screen.queryByText("未检测到 cc-switch 配置")).toBeNull();
  await user.click(screen.getByRole("button", { name: /配置 AI 供应商/ }));
  expect(onNavigate).toHaveBeenCalledWith("providers");
});

test("检测到就报条数，导入后就地给结果", async () => {
  const user = userEvent.setup();
  runImport.mockResolvedValue({
    providers: { imported: ["A", "B"], skipped: ["C"], failed: [] },
    servers: { imported: ["context7"], skipped: [], warnings: [], incomplete: [] },
    warnings: ["某条：Claude Desktop 的本地代理模式无法迁移"],
  });
  renderWizard();
  await gotoStep2(user);

  await screen.findByText("检测到 3 个供应商、1 个 MCP 服务，可以导入");
  // 「Claude Code 3」会被读成版本号。
  expect(screen.getByText("Claude Code：3 个")).toBeTruthy();
  await user.click(screen.getByRole("button", { name: "导入" }));

  await waitFor(() => expect(runImport).toHaveBeenCalledWith(true, true));
  await screen.findByText("已导入 2 个供应商、1 个 MCP 服务");
  expect(screen.getByText("1 条已存在，未重复导入")).toBeTruthy();
  // 搬不动的那条要给出原因，不能静默丢掉。
  expect(screen.getByText("某条：Claude Desktop 的本地代理模式无法迁移")).toBeTruthy();
  // 导完就不该还能再点一次。
  expect(screen.queryByRole("button", { name: "导入" })).toBeNull();
  // 搬完还摆着「可以搬过来」和那两个开关，与「搬好了」直接打架。
  expect(screen.queryByText("检测到 3 个供应商、1 个 MCP 服务，可以导入")).toBeNull();
  expect(screen.queryByRole("switch")).toBeNull();
  // 搬完了，往下走的那颗才回到主按钮。
  expect(screen.getByRole("button", { name: "下一步" })).toBeTruthy();
});

test("只勾一类时只搬那一类", async () => {
  const user = userEvent.setup();
  runImport.mockResolvedValue({
    providers: { imported: [], skipped: [], failed: [] },
    servers: { imported: ["context7"], skipped: [], warnings: [], incomplete: [] },
    warnings: [],
  });
  renderWizard();
  await gotoStep2(user);
  await screen.findByText("检测到 3 个供应商、1 个 MCP 服务，可以导入");

  await user.click(screen.getByRole("switch", { name: "供应商（3）" }));
  await user.click(screen.getByRole("button", { name: "导入" }));
  await waitFor(() => expect(runImport).toHaveBeenCalledWith(false, true));
});

/**
 * 探测失败 ≠ 没装。此时仍要把这一步显示出来并说明原因——直接跳过的话，
 * 一个装了 cc-switch 但库被锁住的用户会以为 MCPane 根本不支持迁移。
 */
test("检测失败时说明原因，而不是装作没装", async () => {
  const user = userEvent.setup();
  detect.mockRejectedValue(new Error("database is locked"));
  renderWizard();
  await screen.findByLabelText("第 1 步，共 3 步");
  await user.click(screen.getByRole("button", { name: "下一步" }));
  await screen.findByText(/检测失败：database is locked/);
});

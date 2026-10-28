import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { ClientAccessControlModal } from "./ClientAccessControlModal";

const LONG_ID = "conn_6b2d7fddb58d459a8c9f6129127250817";

const invokeMock = vi.fn(async (cmd: string, args?: unknown) => {
  switch (cmd) {
    case "hub_status":
      return {
        name: "mcpane-hub",
        serverVersion: "0",
        protocolVersion: "2026-07-28",
        legacyProtocolVersion: "2025-11-25",
        endpoint: "http://127.0.0.1:24860/mcp",
        uptimeSecs: 1,
        connections: [
          {
            clientConnectionId: "conn_claude_1",
            clientType: "claude",
            tokenPrefix: "abcd1234",
            enabled: true,
            defaultWorkspaceId: null,
            toolExposureMode: "auto",
          },
          // uuid 形态的真实 ID：卡片标题必须截短，否则撑破布局。
          {
            clientConnectionId: LONG_ID,
            clientType: "claude",
            tokenPrefix: "f60118f6",
            enabled: true,
            defaultWorkspaceId: null,
            toolExposureMode: "auto",
          },
          // 其他客户端的连接不得出现在本弹窗。
          {
            clientConnectionId: "conn_codex_1",
            clientType: "codex",
            tokenPrefix: "ffff0000",
            enabled: true,
            defaultWorkspaceId: null,
            toolExposureMode: "auto",
          },
        ],
      };
    case "servers_list":
      return [
        { id: "srv_a", slug: "alpha", name: "Alpha", enabled: true },
        { id: "srv_b", slug: "beta", name: "Beta", enabled: false },
      ];
    case "connection_denied_servers":
      // 只有 conn_claude_1 有拒绝项，「已限制 N 个」标签才保持唯一。
      return (args as { clientConnectionId: string }).clientConnectionId === "conn_claude_1"
        ? ["srv_b"]
        : [];
    case "connection_server_access_set":
      return accessSetImpl(args as { serverId: string; allowed: boolean });
    default:
      return null;
  }
});

let accessSetImpl: (args: { serverId: string; allowed: boolean }) => Promise<void> = async () => {};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

function renderModal() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <ClientAccessControlModal clientId="claude" title="Claude Code" onClose={() => {}} />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  accessSetImpl = async () => {};
  invokeMock.mockClear();
});

test("deny-list 回显：允许的 Server 开关为开，被拒的为关，且只列本客户端的连接", async () => {
  renderModal();

  // findByRole 等 deny-list 查询返回：返回前 Beta 的开关标签还是「拒绝」态。
  const deniedSwitch = await screen.findByRole("switch", {
    name: "允许 conn_claude_1 访问 Beta",
  });
  expect(deniedSwitch).not.toBeChecked();
  const allowedSwitch = screen.getByRole("switch", { name: "拒绝 conn_claude_1 访问 Alpha" });
  expect(allowedSwitch).toBeChecked();

  expect(screen.getByText("已限制 1 个")).toBeInTheDocument();
  expect(screen.queryByText("conn_codex_1")).not.toBeInTheDocument();
});

test("关闭开关派发 connection_server_access_set(allowed=false) 并乐观更新", async () => {
  const user = userEvent.setup();
  renderModal();

  const allowedSwitch = await screen.findByRole("switch", {
    name: "拒绝 conn_claude_1 访问 Alpha",
  });
  await user.click(allowedSwitch);

  await waitFor(() =>
    expect(invokeMock).toHaveBeenCalledWith("connection_server_access_set", {
      clientConnectionId: "conn_claude_1",
      serverId: "srv_a",
      allowed: false,
    }),
  );
});

test("控制面报错时乐观更新回滚并展示错误", async () => {
  accessSetImpl = async () => {
    throw new Error("Hub 未运行或控制面不可达");
  };
  const user = userEvent.setup();
  renderModal();

  const allowedSwitch = await screen.findByRole("switch", {
    name: "拒绝 conn_claude_1 访问 Alpha",
  });
  await user.click(allowedSwitch);

  await waitFor(() => expect(screen.getByText(/Hub 未运行或控制面不可达/)).toBeInTheDocument());
  // 回滚后开关恢复为开。
  await waitFor(() =>
    expect(screen.getByRole("switch", { name: "拒绝 conn_claude_1 访问 Alpha" })).toBeChecked(),
  );
});

test("uuid 形态的连接 ID 截短显示，完整值留在 title 上", async () => {
  renderModal();

  const short = await screen.findByText("conn_6b2d7f…0817");
  expect(short).toHaveAttribute("title", LONG_ID);
  expect(screen.queryByText(LONG_ID)).not.toBeInTheDocument();
  // 无障碍标签仍用完整 ID：截短只为版面，不该削弱可定位性。
  expect(screen.getByRole("switch", { name: `拒绝 ${LONG_ID} 访问 Alpha` })).toBeInTheDocument();
});

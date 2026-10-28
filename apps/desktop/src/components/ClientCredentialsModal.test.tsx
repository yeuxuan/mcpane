import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { WorkspaceProvider } from "../lib/workspace";
import { ClientCredentialsModal } from "./ClientCredentialsModal";

const LONG_ID = "conn_6b2d7fddb58d459a8c9f6129127250817";

const invokeMock = vi.fn(async (cmd: string) => {
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
            clientConnectionId: LONG_ID,
            clientType: "codex",
            tokenPrefix: "f60118f6",
            enabled: true,
            defaultWorkspaceId: null,
            toolExposureMode: "auto",
          },
          {
            clientConnectionId: "conn_dev_codex",
            clientType: "codex",
            tokenPrefix: "8a8b0f96",
            enabled: true,
            defaultWorkspaceId: null,
            toolExposureMode: "auto",
          },
        ],
      };
    case "workspaces_list":
      return [];
    default:
      return null;
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => invokeMock(cmd),
}));

function renderModal() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <WorkspaceProvider>
        <ClientCredentialsModal clientId="codex" title="Codex" onClose={() => {}} />
      </WorkspaceProvider>
    </QueryClientProvider>,
  );
}

beforeEach(() => invokeMock.mockClear());

test("长连接 ID 截短显示，完整值留在 title 上", async () => {
  renderModal();
  const short = await screen.findByText("conn_6b2d7f…0817");
  expect(short).toHaveAttribute("title", LONG_ID);
  expect(screen.queryByText(LONG_ID)).not.toBeInTheDocument();
  // 可读短 ID 不受影响。
  expect(await screen.findByText("conn_dev_codex")).toBeInTheDocument();
});

test("新增按钮的文案说的是创建凭证", async () => {
  renderModal();
  expect(await screen.findByRole("button", { name: "新建凭证" })).toBeInTheDocument();
});

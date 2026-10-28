import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../lib/i18n";
import type { ServerDef } from "../lib/ipc";
import { WorkspaceProvider } from "../lib/workspace";
import { ServerSetupModal } from "./ServerSetupModal";

/** 只有各用例显式覆盖的字段有意义，其余取合法默认值（同 pages/Servers.test.ts）。 */
function serverDef(overrides: Partial<ServerDef> = {}): ServerDef {
  return {
    id: "srv_1",
    slug: "demo",
    name: "Demo",
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

const invokeMock = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case "server_upsert":
      return undefined;
    case "workspaces_list":
      return [];
    case "hub_diagnostics":
      return { catalogTools: 0, catalogRevision: "rev1", downstreams: {}, workspaces: {} };
    case "catalog_list":
      return { tools: [], prompts: [], resources: [] };
    case "workspace_tool_overrides":
    case "workspace_item_overrides":
      return [];
    default:
      return undefined;
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

function renderModal(props: Partial<Parameters<typeof ServerSetupModal>[0]> = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const onClose = vi.fn();
  const utils = render(
    <QueryClientProvider client={queryClient}>
      <WorkspaceProvider>
        <ServerSetupModal onClose={onClose} {...props} />
      </WorkspaceProvider>
    </QueryClientProvider>,
  );
  return { ...utils, onClose };
}

beforeEach(() => {
  invokeMock.mockClear();
});

describe("ServerSetupModal", () => {
  it("新建模式默认落在官方预设页签，不发编辑态才需要的诊断请求", () => {
    renderModal();
    expect(screen.getByRole("tab", { name: "官方预设" })).toHaveAttribute("aria-selected", "true");
    expect(invokeMock).not.toHaveBeenCalledWith("hub_diagnostics", undefined);
    expect(invokeMock).not.toHaveBeenCalledWith("catalog_list", expect.anything());
  });

  it("手动创建页签：填完名称+命令后可保存，成功后关闭弹窗", async () => {
    const user = userEvent.setup();
    const { onClose } = renderModal();

    await user.click(screen.getByRole("tab", { name: "手动创建" }));
    const saveButton = screen.getByRole("button", { name: "创建 MCP 服务器" });
    expect(saveButton).toBeDisabled();

    await user.type(screen.getByRole("textbox", { name: /服务器名称/ }), "my-server");
    await user.type(screen.getByRole("textbox", { name: /^命令/ }), "npx");
    expect(saveButton).toBeEnabled();

    await user.click(saveButton);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(invokeMock).toHaveBeenCalledWith(
      "server_upsert",
      expect.objectContaining({
        input: expect.objectContaining({ name: "my-server", command: "npx", transport: "stdio" }),
      }),
    );
  });

  it("编辑模式：回填已有字段并按 editing 触发诊断类查询", async () => {
    const editing = serverDef({ id: "s1", name: "existing", command: "node", args: ["server.js"] });
    renderModal({ editing });
    expect(screen.getByRole("textbox", { name: /服务器名称/ })).toHaveValue("existing");
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("hub_diagnostics", undefined));
  });
});

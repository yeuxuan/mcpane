import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import "../lib/i18n";
import type { ProviderProfile } from "../lib/ipc";
import { ProviderEditor } from "./ProviderEditor";

/** 只有各用例显式覆盖的字段有意义，其余取合法默认值。 */
function providerProfile(overrides: Partial<ProviderProfile> = {}): ProviderProfile {
  return {
    schemaVersion: 2,
    id: "p1",
    app: "claude",
    category: "relay",
    name: "",
    baseUrl: null,
    model: null,
    modelHaiku: null,
    modelSonnet: null,
    modelOpus: null,
    modelFable: null,
    modelSubagent: null,
    apiKeyField: "auth-token",
    codexWireApi: "responses",
    desktopMode: "direct",
    desktopRoutes: [],
    extraSettings: null,
    commonConfigEnabled: false,
    endpointCandidates: [],
    models: [],
    upstreamApi: null,
    headers: {},
    routeBodyOverride: null,
    contextLength: null,
    codexQueryParams: {},
    codexContextWindow1m: false,
    codexAutoCompactLimit: null,
    icon: null,
    notes: null,
    websiteUrl: null,
    apiKeyRef: null,
    codexSlug: null,
    sortOrder: 0,
    ...overrides,
  };
}

const invokeMock = vi.fn(async (cmd: string, args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case "provider_live_settings":
      return {};
    case "provider_common_snippet":
      return null;
    case "provider_add":
      return { ...(args as { profile: object }).profile, id: "new-id" };
    case "provider_set_api_key":
    case "provider_update":
    case "provider_switch":
    case "open_external":
      return undefined;
    default:
      return undefined;
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

function renderEditor(props: Partial<Parameters<typeof ProviderEditor>[0]> = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const onClose = vi.fn();
  const utils = render(
    <QueryClientProvider client={queryClient}>
      <ProviderEditor app="claude" onClose={onClose} {...props} />
    </QueryClientProvider>,
  );
  return { ...utils, onClose };
}

beforeEach(() => {
  invokeMock.mockClear();
});

describe("ProviderEditor", () => {
  it("新建模式下渲染基础表单字段，保存按钮初始禁用", async () => {
    renderEditor();
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("provider_live_settings", { app: "claude" }),
    );
    expect(screen.getByRole("textbox", { name: /名称/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
  });

  it("编辑模式下用 profile 字段回填表单", async () => {
    const profile = providerProfile({
      name: "我的中转站",
      baseUrl: "https://relay.example.com",
    });
    renderEditor({ profile });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("provider_live_settings", { app: "claude" }),
    );
    expect(screen.getByRole("textbox", { name: /名称/ })).toHaveValue("我的中转站");
    expect(screen.getByRole("textbox", { name: /Base URL/ })).toHaveValue(
      "https://relay.example.com",
    );
  });

  it("填完必填项后保存：新建走 provider_add + provider_set_api_key，成功后关闭面板", async () => {
    const user = userEvent.setup();
    const { onClose } = renderEditor();
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("provider_live_settings", { app: "claude" }),
    );

    await user.type(screen.getByRole("textbox", { name: /名称/ }), "测试供应商");
    await user.type(screen.getByRole("textbox", { name: /Base URL/ }), "https://api.example.com");

    const saveButton = screen.getByRole("button", { name: "保存" });
    await waitFor(() => expect(saveButton).toBeEnabled());
    await user.click(saveButton);

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(invokeMock).toHaveBeenCalledWith(
      "provider_add",
      expect.objectContaining({ profile: expect.objectContaining({ name: "测试供应商" }) }),
    );
  });

  it("快捷开关「禁用 Artifact 工具」写入 env.CLAUDE_CODE_DISABLE_ARTIFACT=1", async () => {
    const user = userEvent.setup();
    renderEditor();
    const toggle = await screen.findByRole("checkbox", { name: "禁用 Artifact 工具" });
    await waitFor(() => expect(toggle).toBeEnabled());
    expect(toggle).toHaveAttribute("aria-checked", "false");

    await user.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "true"));
    // 开关只是编辑器文本的投影：文本里必须真有这条 env，否则勾选态是假象。
    expect(document.body.textContent).toContain("CLAUDE_CODE_DISABLE_ARTIFACT");

    await user.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute("aria-checked", "false"));
    expect(document.body.textContent).not.toContain("CLAUDE_CODE_DISABLE_ARTIFACT");
  });

  it("Chat 协议的 Claude 供应商：提示需开启本地路由、隐藏认证字段，保存时保留协议", async () => {
    const user = userEvent.setup();
    const profile = providerProfile({
      name: "Chat 站",
      baseUrl: "https://chat.example.com/v1",
      claudeApiFormat: "openai-chat",
    });
    const { onClose } = renderEditor({ profile });
    expect(await screen.findByText(/需在供应商页开启本地路由/)).toBeInTheDocument();
    expect(screen.queryByText("认证字段")).toBeNull();

    await user.type(screen.getByRole("textbox", { name: /名称/ }), "2");
    const saveButton = screen.getByRole("button", { name: "保存" });
    await waitFor(() => expect(saveButton).toBeEnabled());
    await user.click(saveButton);
    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(invokeMock).toHaveBeenCalledWith(
      "provider_update",
      expect.objectContaining({
        profile: expect.objectContaining({ claudeApiFormat: "openai-chat" }),
      }),
    );
  });

  it("路由 app 的中转站：高级段显示请求头与请求体覆盖，保存时一并写入", async () => {
    const user = userEvent.setup();
    const { onClose } = renderEditor(); // 默认 app="claude"，属于支持本地路由的 app
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("provider_live_settings", { app: "claude" }),
    );

    await user.type(screen.getByRole("textbox", { name: /名称/ }), "路由中转站");
    await user.type(screen.getByRole("textbox", { name: /Base URL/ }), "https://api.example.com");

    await user.click(screen.getByRole("button", { name: /高级选项/ }));
    expect(screen.getByText(/仅在本地路由开启时生效/)).toBeInTheDocument();

    const headers = await screen.findByRole("textbox", { name: "自定义请求头" });
    await user.type(headers, "X-Route=fast");
    const bodyOverride = screen.getByRole("textbox", { name: "请求体覆盖" });
    fireEvent.change(bodyOverride, { target: { value: '{"enable_thinking":false}' } });

    const saveButton = screen.getByRole("button", { name: "保存" });
    await waitFor(() => expect(saveButton).toBeEnabled());
    await user.click(saveButton);

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(invokeMock).toHaveBeenCalledWith(
      "provider_add",
      expect.objectContaining({
        profile: expect.objectContaining({
          headers: { "X-Route": "fast" },
          routeBodyOverride: { enable_thinking: false },
        }),
      }),
    );
  });

  it("请求体覆盖不是 JSON 对象时禁止保存并提示", async () => {
    const user = userEvent.setup();
    renderEditor();
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("provider_live_settings", { app: "claude" }),
    );
    await user.type(screen.getByRole("textbox", { name: /名称/ }), "路由中转站");
    await user.type(screen.getByRole("textbox", { name: /Base URL/ }), "https://api.example.com");
    await user.click(screen.getByRole("button", { name: /高级选项/ }));

    const bodyOverride = await screen.findByRole("textbox", { name: "请求体覆盖" });
    fireEvent.change(bodyOverride, { target: { value: "[1,2,3]" } });

    expect(await screen.findByText("必须是 JSON 对象")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
  });

  it("非路由、非 additive 的 app（Claude Desktop）不显示请求头/请求体覆盖字段", async () => {
    renderEditor({ app: "claude-desktop" });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("provider_live_settings", { app: "claude-desktop" }),
    );
    expect(screen.queryByText("自定义请求头")).toBeNull();
    expect(screen.queryByText("请求体覆盖")).toBeNull();
    expect(screen.queryByRole("button", { name: /高级选项/ })).toBeNull();
  });
});

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import type { ServerDef, ServerImportPreview, ServerImportSummary } from "../lib/ipc";
import { ServerImportAction, ServerImportPanel, useServerImport } from "./ServerImportPanel";

/** `${NAME}` 占位符；写成普通字符串字面量会被 lint 当成忘了用模板字符串。 */
const placeholder = (name: string) => `\${${name}}`;

function serverDef(overrides: Partial<ServerDef>): ServerDef {
  return {
    id: "",
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

const PREVIEW: ServerImportPreview = {
  entries: [
    { server: serverDef({ name: "memory", slug: "memory" }), conflict: null, variables: [] },
    {
      server: serverDef({
        name: "GitHub",
        slug: "github",
        transport: "http",
        command: null,
        url: "https://api.githubcopilot.com/mcp/",
        headers: { Authorization: `Bearer ${placeholder("AUTHORIZATION")}` },
      }),
      conflict: null,
      variables: ["AUTHORIZATION"],
    },
    {
      server: serverDef({ name: "context7", slug: "context7" }),
      conflict: "context7",
      variables: [],
    },
    {
      server: serverDef({ name: "Legacy", slug: "legacy", enabled: false }),
      conflict: null,
      variables: [],
    },
  ],
  invalid: ["broken"],
};

let preview: ServerImportPreview | string = PREVIEW;
let summary: ServerImportSummary;
const calls: { cmd: string; args: unknown }[] = [];

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => {
    calls.push({ cmd, args });
    if (cmd === "servers_import_preview") {
      return typeof preview === "string" ? Promise.reject(preview) : Promise.resolve(preview);
    }
    if (cmd === "servers_import_json") return Promise.resolve(summary);
    return Promise.resolve(null);
  },
}));

function Harness({ onImported }: { onImported: (message: string) => void }) {
  const importer = useServerImport(onImported);
  return (
    <>
      <ServerImportPanel importer={importer} />
      <ServerImportAction importer={importer} />
    </>
  );
}

function renderPanel() {
  const onImported = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const utils = render(
    <QueryClientProvider client={client}>
      <Harness onImported={onImported} />
    </QueryClientProvider>,
  );
  const fileInput = utils.container.querySelector<HTMLInputElement>('input[type="file"]');
  if (!fileInput) throw new Error("缺少文件选择框");
  return { onImported, fileInput };
}

const FILE_TEXT = '{ "mcpServers": { "memory": { "command": "npx" } } }';

beforeEach(() => {
  preview = PREVIEW;
  summary = { imported: ["GitHub"], skipped: [], warnings: [], incomplete: [] };
  calls.length = 0;
});

test("选择文件即出预览：冲突条目不可勾选，占位符就地补填，无效条目单独说明", async () => {
  const user = userEvent.setup();
  const { fileInput } = renderPanel();
  expect(screen.getByRole("button", { name: "导入" })).toBeDisabled();

  await user.upload(fileInput, new File([FILE_TEXT], "servers.json", { type: "application/json" }));

  // 文件原文直接送去解析，不等防抖。
  await screen.findByRole("group", { name: "待导入的服务器" });
  expect(calls.find((c) => c.cmd === "servers_import_preview")?.args).toEqual({ json: FILE_TEXT });

  expect(screen.getByRole("checkbox", { name: "导入 context7" })).toBeDisabled();
  expect(screen.getByText("已存在，跳过")).toBeInTheDocument();
  expect(screen.getByText("待填写 1 项")).toBeInTheDocument();
  // 取值框是密码框（没有 textbox 角色），按标签找。
  expect(screen.getByLabelText("GitHub 的 AUTHORIZATION")).toHaveAttribute("type", "password");
  expect(screen.getByText("导入后停用")).toBeInTheDocument();
  expect(screen.getByText("无法识别（缺少 command 或 url），已忽略：broken")).toBeInTheDocument();
  // 冲突的不计入：memory + GitHub + Legacy。
  expect(screen.getByRole("button", { name: "导入 3 个服务器" })).toBeEnabled();
});

test("只导入勾选的条目，补填的取值随条目名一起提交", async () => {
  const user = userEvent.setup();
  const { fileInput, onImported } = renderPanel();
  await user.upload(fileInput, new File([FILE_TEXT], "servers.json", { type: "application/json" }));
  await screen.findByRole("group", { name: "待导入的服务器" });

  await user.click(screen.getByRole("checkbox", { name: "导入 memory" }));
  await user.click(screen.getByRole("checkbox", { name: "导入 Legacy" }));
  const value = screen.getByLabelText("GitHub 的 AUTHORIZATION");
  await user.type(value, "ghp_secret");
  expect(screen.queryByText("待填写 1 项")).not.toBeInTheDocument();

  summary = { imported: ["GitHub"], skipped: [], warnings: [], incomplete: [] };
  await user.click(screen.getByRole("button", { name: "导入 1 个服务器" }));

  await waitFor(() => expect(onImported).toHaveBeenCalledWith("已导入 1 个"));
  expect(calls.find((c) => c.cmd === "servers_import_json")?.args).toEqual({
    json: FILE_TEXT,
    selected: ["GitHub"],
    values: { GitHub: { AUTHORIZATION: "ghp_secret" } },
  });
});

test("没填全的条目照常导入，结果里说明已按停用入库", async () => {
  const user = userEvent.setup();
  const { fileInput, onImported } = renderPanel();
  await user.upload(fileInput, new File([FILE_TEXT], "servers.json", { type: "application/json" }));
  await screen.findByRole("group", { name: "待导入的服务器" });

  summary = {
    imported: ["memory", "GitHub", "Legacy"],
    skipped: [],
    warnings: [],
    incomplete: ["GitHub"],
  };
  await user.click(screen.getByRole("button", { name: "导入 3 个服务器" }));

  await waitFor(() =>
    expect(onImported).toHaveBeenCalledWith(
      "已导入 3 个 · 其中 1 个缺少凭据，已按停用导入，编辑补填后再启用",
    ),
  );
});

test("全部被跳过时不关窗，原地说明结果", async () => {
  const user = userEvent.setup();
  const { fileInput, onImported } = renderPanel();
  await user.upload(fileInput, new File([FILE_TEXT], "servers.json", { type: "application/json" }));
  await screen.findByRole("group", { name: "待导入的服务器" });

  summary = { imported: [], skipped: ["memory", "GitHub", "Legacy"], warnings: [], incomplete: [] };
  await user.click(screen.getByRole("button", { name: "导入 3 个服务器" }));

  expect(await screen.findByText("已导入 0 个，跳过已存在 3 个")).toBeInTheDocument();
  expect(onImported).not.toHaveBeenCalled();
});

test("解析失败把后端的错因原样展示", async () => {
  preview = "JSON 解析失败: expected value at line 1 column 1";
  const user = userEvent.setup();
  const { fileInput } = renderPanel();
  await user.upload(fileInput, new File(["oops"], "servers.json", { type: "application/json" }));

  expect(
    await screen.findByText("JSON 解析失败: expected value at line 1 column 1"),
  ).toBeInTheDocument();
  expect(screen.getByRole("button", { name: "导入" })).toBeDisabled();
});

test("批内同前缀的两条随勾选互斥：取消前一条，后一条即可导入", async () => {
  preview = {
    entries: [
      { server: serverDef({ name: "memory", slug: "memory" }), conflict: null, variables: [] },
      { server: serverDef({ name: "Memory", slug: "memory" }), conflict: null, variables: [] },
    ],
    invalid: [],
  };
  const user = userEvent.setup();
  const { fileInput } = renderPanel();
  await user.upload(fileInput, new File([FILE_TEXT], "servers.json", { type: "application/json" }));
  await screen.findByRole("group", { name: "待导入的服务器" });

  const second = screen.getByRole("checkbox", { name: "导入 Memory" });
  expect(second).toBeDisabled();
  expect(screen.getByText("与「memory」工具前缀相同，跳过")).toBeInTheDocument();

  await user.click(screen.getByRole("checkbox", { name: "导入 memory" }));
  expect(second).toBeEnabled();
  expect(second).toBeChecked();
  expect(screen.getByRole("button", { name: "导入 1 个服务器" })).toBeEnabled();
});

test("超过 1 MB 的文件直接拒绝，不送去解析", async () => {
  const user = userEvent.setup();
  const { fileInput } = renderPanel();
  const big = new File(["x".repeat(1024 * 1024 + 1)], "huge.json", { type: "application/json" });
  await user.upload(fileInput, big);

  expect(await screen.findByText("huge.json 超过 1 MB，不是服务器配置文件")).toBeInTheDocument();
  expect(calls.some((c) => c.cmd === "servers_import_preview")).toBe(false);
});

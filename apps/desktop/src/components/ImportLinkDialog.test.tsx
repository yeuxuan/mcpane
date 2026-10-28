import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import type { ImportLinkOutcome, ImportLinkPreview } from "../lib/ipc";
import { ImportLinkDialog } from "./ImportLinkDialog";

type Handler = (event: { payload: unknown }) => void;

let handlers: Record<string, Handler> = {};
let pending: ImportLinkPreview | null = null;
let confirmCalls: (string | null)[] = [];
let discardCalls = 0;
let outcome: ImportLinkOutcome = { imported: ["中转 A"], skipped: [], failed: [] };
let confirmError: string | null = null;

vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: Handler) => {
    handlers[name] = cb;
    return Promise.resolve(() => {});
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: { apiKey: string | null }) => {
    if (cmd === "import_link_peek") return Promise.resolve(pending);
    if (cmd === "import_link_confirm") {
      confirmCalls.push(args?.apiKey ?? null);
      return confirmError ? Promise.reject(confirmError) : Promise.resolve(outcome);
    }
    if (cmd === "import_link_discard") {
      discardCalls += 1;
      pending = null;
      return Promise.resolve();
    }
    return Promise.resolve(null);
  },
}));

const PREVIEW: ImportLinkPreview = {
  app: "claude",
  name: "中转 A",
  baseUrl: "https://relay.example.com/api",
  host: "relay.example.com",
  model: "glm-5",
  keyHint: "••••9999",
};

function renderIt() {
  const client = new QueryClient();
  return render(
    <QueryClientProvider client={client}>
      <ImportLinkDialog />
    </QueryClientProvider>,
  );
}

beforeEach(() => {
  handlers = {};
  pending = null;
  confirmCalls = [];
  discardCalls = 0;
  outcome = { imported: ["中转 A"], skipped: [], failed: [] };
  confirmError = null;
});

test("没有待导入链接时不渲染任何东西", async () => {
  const { container } = renderIt();
  await waitFor(() => expect(handlers["import-link-request"]).toBeDefined());
  expect(container.textContent).toBe("");
});

test("冷启动已暂存的草稿在挂载时即弹出确认框，突出主机名并只显示 Key 掩码", async () => {
  pending = PREVIEW;
  renderIt();
  expect(await screen.findByRole("dialog", { name: "导入供应商" })).toBeTruthy();
  expect(screen.getByText("relay.example.com")).toBeTruthy();
  expect(screen.getByText("https://relay.example.com/api")).toBeTruthy();
  expect(screen.getByText("Claude Code")).toBeTruthy();
  expect(screen.getByText("链接已提供（••••9999）")).toBeTruthy();
  expect(screen.queryByLabelText("API Key")).toBeNull();
});

test("运行中收到事件后取预览；点导入落库并给出结果", async () => {
  renderIt();
  await waitFor(() => expect(handlers["import-link-request"]).toBeDefined());
  pending = PREVIEW;
  handlers["import-link-request"]?.({ payload: null });
  await userEvent.click(await screen.findByRole("button", { name: "导入" }));
  await waitFor(() => expect(confirmCalls).toEqual([null]));
  expect(await screen.findByText("已导入「中转 A」。到供应商页选择启用即可。")).toBeTruthy();
});

test("链接没带 Key 时可在框内补填，补填值随确认一并提交", async () => {
  pending = { ...PREVIEW, keyHint: null };
  renderIt();
  await userEvent.type(await screen.findByLabelText("API Key"), "sk-filled-by-user");
  await userEvent.click(screen.getByRole("button", { name: "导入" }));
  await waitFor(() => expect(confirmCalls).toEqual(["sk-filled-by-user"]));
});

test("取消会丢弃后端暂存的草稿", async () => {
  pending = PREVIEW;
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "取消" }));
  await waitFor(() => expect(discardCalls).toBe(1));
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("同地址已存在时提示未重复导入", async () => {
  pending = PREVIEW;
  outcome = { imported: [], skipped: ["老中转"], failed: [] };
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "导入" }));
  expect(await screen.findByText("已有相同地址的供应商「老中转」，未重复导入。")).toBeTruthy();
});

test("落库失败时留在确认框并显示原因，可重试", async () => {
  pending = PREVIEW;
  confirmError = "没有待导入的链接，可能已处理或已过期";
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "导入" }));
  expect(await screen.findByText(/没有待导入的链接/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "导入" })).toBeTruthy();
});

test("无效链接事件弹出提示，且不把它当成待导入项", async () => {
  renderIt();
  await waitFor(() => expect(handlers["import-link-error"]).toBeDefined());
  handlers["import-link-error"]?.({ payload: "参数无效：baseUrl" });
  expect(await screen.findByText("这个链接无法识别：参数无效：baseUrl")).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "完成" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(discardCalls).toBe(0);
});

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import type { RouteRequestRecord } from "../lib/ipc";
import { RouteRequestLog } from "./RouteRequestLog";

let recent: RouteRequestRecord[] = [];
let clearCalls: (string | undefined)[] = [];
let clearError: string | null = null;

vi.mock("../lib/ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/ipc")>();
  return {
    ...actual,
    ipc: {
      routerRecentRequests: () => Promise.resolve(recent),
      routerClearRequests: (app?: string) => {
        clearCalls.push(app);
        if (clearError) return Promise.reject(new Error(clearError));
        recent = [];
        return Promise.resolve();
      },
    },
  };
});

const record = (overrides: Partial<RouteRequestRecord> = {}): RouteRequestRecord => ({
  id: 1,
  at: Date.now(),
  app: "claude",
  model: "glm-5",
  stream: true,
  providerId: "a",
  providerName: "中转 A",
  attempts: [],
  status: 200,
  firstByteMs: 300,
  durationMs: 1000,
  usage: null,
  error: null,
  ...overrides,
});

function renderIt() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const Wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return render(<RouteRequestLog app="claude" />, { wrapper: Wrapper });
}

beforeEach(() => {
  recent = [];
  clearCalls = [];
  clearError = null;
});

test("展开后默认展示全部请求，切到「仅失败」只留失败/进行中的行", async () => {
  recent = [
    record({ id: 1, providerName: "中转 A", status: 200 }),
    record({ id: 2, providerName: "中转 B", status: 503 }),
    record({ id: 3, providerName: "中转 C", status: null }),
    record({ id: 4, providerName: "中转 D", status: 200, error: "解析失败" }),
  ];
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "最近请求" }));
  expect(await screen.findByText("中转 A")).toBeTruthy();
  expect(screen.getByText("中转 B")).toBeTruthy();
  expect(screen.getByText("中转 C")).toBeTruthy();
  expect(screen.getByText("中转 D")).toBeTruthy();

  await userEvent.click(screen.getByRole("radio", { name: "仅失败" }));
  expect(screen.queryByText("中转 A")).toBeNull();
  expect(screen.getByText("中转 B")).toBeTruthy();
  expect(screen.getByText("中转 C")).toBeTruthy();
  expect(screen.getByText("中转 D")).toBeTruthy();
});

test("清空记录：二次确认后调用清空命令并刷新列表", async () => {
  recent = [record({ id: 1 })];
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "最近请求" }));
  expect(await screen.findByText("中转 A")).toBeTruthy();

  await userEvent.click(screen.getByRole("button", { name: "清空记录" }));
  expect(await screen.findByText(/清空后不可恢复/)).toBeTruthy();
  await userEvent.click(screen.getAllByRole("button", { name: "清空记录" })[1] as HTMLElement);

  await waitFor(() => expect(clearCalls).toEqual(["claude"]));
  await waitFor(() =>
    expect(screen.getByText("开启本地路由后，经过路由的请求会显示在这里。")).toBeTruthy(),
  );
});

test("清空失败时如实展示错误", async () => {
  recent = [record({ id: 1 })];
  clearError = "connection refused";
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "最近请求" }));
  await userEvent.click(await screen.findByRole("button", { name: "清空记录" }));
  await userEvent.click(screen.getAllByRole("button", { name: "清空记录" })[1] as HTMLElement);
  expect(await screen.findByText("connection refused")).toBeTruthy();
});

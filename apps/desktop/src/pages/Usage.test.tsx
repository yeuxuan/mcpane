import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { Usage } from "./Usage";

const invokeMock = vi.fn(async (cmd: string, _args?: unknown): Promise<unknown> => {
  switch (cmd) {
    case "usage_sync":
      return { imported: 0, filesScanned: 0, errors: [] };
    case "usage_summary":
      return {
        events: 0,
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheCreationTokens: 0,
        costUsd: 0,
        unpricedEvents: 0,
      };
    default:
      // 其余用量查询（趋势/按模型/按项目/按客户端/配额）都是列表形态
      return [];
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

afterEach(() => invokeMock.mockClear());

function renderUsage() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <Usage />
    </QueryClientProvider>,
  );
}

test("「立即同步」不受 5 分钟自动同步节流限制", async () => {
  const user = userEvent.setup();
  // 首次挂载自动同步一次（把节流基准打到"刚刚"）。
  renderUsage();
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("usage_sync", undefined));
  const autoCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === "usage_sync").length;
  expect(autoCalls).toBe(1);

  // 节流窗口内手动点击仍要真的发起同步。
  const button = await screen.findByRole("button", { name: "立即同步" });
  await user.click(button);
  await waitFor(() =>
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "usage_sync")).toHaveLength(2),
  );
});

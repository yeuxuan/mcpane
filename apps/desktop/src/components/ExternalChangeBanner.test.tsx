import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { ExternalChangeBanner } from "./ExternalChangeBanner";

type Handler = (event: { payload: { path: string } }) => void;

let handler: Handler | undefined;
let unlistened = false;
/** 后端待读队列（模拟窗口关着时攒下的改动）。 */
let pending: string[] = [];
let takePendingCalls = 0;

vi.mock("@tauri-apps/api/event", () => ({
  listen: (_name: string, cb: Handler) => {
    handler = cb;
    return Promise.resolve(() => {
      unlistened = true;
    });
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => {
    if (cmd === "config_external_changes") {
      takePendingCalls += 1;
      const taken = pending;
      pending = []; // 取走即清空，与后端 take_pending 语义一致
      return Promise.resolve(taken);
    }
    return Promise.resolve(null);
  },
}));

/** 模拟后端 emit 一次外部改动。 */
async function emitChange(path: string) {
  await waitFor(() => expect(handler).toBeDefined());
  handler?.({ payload: { path } });
}

beforeEach(() => {
  handler = undefined;
  unlistened = false;
  pending = [];
  takePendingCalls = 0;
});

function renderBanner() {
  const queryClient = new QueryClient();
  return render(
    <QueryClientProvider client={queryClient}>
      <ExternalChangeBanner />
    </QueryClientProvider>,
  );
}

test("没有外部改动时不占位", () => {
  const { container } = renderBanner();
  expect(container).toBeEmptyDOMElement();
});

/**
 * 轻量模式下 WebView 会被销毁，那期间的 emit 没有接收方——用户去终端改配置
 * 恰恰就在窗口关着的时候。不补这一拉，这个提醒在最常见的场景下等于没做。
 */
test("挂载时拉取窗口关闭期间攒下的改动", async () => {
  pending = ["/home/me/.claude/settings.json", "/home/me/.codex/config.toml"];
  renderBanner();

  expect(await screen.findByRole("status")).toBeTruthy();
  await waitFor(() => {
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });
  expect(takePendingCalls).toBe(1);
});

test("待读队列与实时事件合并且不重复", async () => {
  pending = ["/home/me/.claude/settings.json"];
  renderBanner();
  await screen.findByRole("status");

  // 同一文件的实时事件不该产生第二行。
  await emitChange("/home/me/.claude/settings.json");
  await emitChange("/home/me/.codex/config.toml");

  await waitFor(() => {
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });
});

test("后端队列取空后不再重复提醒", async () => {
  pending = ["/home/me/.claude/settings.json"];
  const first = renderBanner();
  await first.findByRole("status");
  first.unmount();

  // 重建 WebView（轻量模式唤起）：队列已被取空，不该再弹同一条。
  const second = renderBanner();
  await waitFor(() => expect(takePendingCalls).toBe(2));
  expect(second.container).toBeEmptyDOMElement();
});

test("收到外部改动后显示被改动的文件路径", async () => {
  renderBanner();
  await emitChange("/home/me/.claude/settings.json");

  expect(await screen.findByRole("status")).toBeTruthy();
  expect(screen.getByText("/home/me/.claude/settings.json")).toBeTruthy();
});

test("多个文件分别列出，同一文件不重复", async () => {
  renderBanner();
  await emitChange("/home/me/.claude/settings.json");
  await emitChange("/home/me/.codex/config.toml");
  // 同一文件再次改动不该产生第二行。
  await emitChange("/home/me/.claude/settings.json");

  await waitFor(() => {
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });
});

test("关闭后消失", async () => {
  renderBanner();
  await emitChange("/home/me/.claude/settings.json");
  await screen.findByRole("status");

  await userEvent.click(screen.getByRole("button"));

  await waitFor(() => {
    expect(screen.queryByRole("status")).toBeNull();
  });
});

test("卸载时取消监听，避免泄漏", async () => {
  const { unmount } = renderBanner();
  await waitFor(() => expect(handler).toBeDefined());
  unmount();
  await waitFor(() => expect(unlistened).toBe(true));
});

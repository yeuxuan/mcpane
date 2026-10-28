import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import {
  type AppRouteStatus,
  DEFAULT_ROUTING_TUNING,
  type ProviderProfile,
  type RoutingConfig,
} from "../lib/ipc";
import { RoutingPanel } from "./RoutingPanel";

let stored: RoutingConfig = {
  enabled: false,
  failoverQueue: [],
  tuning: DEFAULT_ROUTING_TUNING,
  strategy: "failover",
};
let saved: RoutingConfig[] = [];
let status: AppRouteStatus[] = [];
let resetCalls: string[] = [];
let proxyError: string | null = null;
let recent: unknown[] = [];

vi.mock("../lib/ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/ipc")>();
  return {
    ...actual,
    ipc: {
      providerRoutingGet: () => Promise.resolve(stored),
      providerRoutingSet: (_app: string, config: RoutingConfig) => {
        saved.push(config);
        stored = config;
        return Promise.resolve(config);
      },
      routerStatus: () => Promise.resolve(status),
      routerResetBreaker: (_app: string, id: string) => {
        resetCalls.push(id);
        return Promise.resolve();
      },
      desktopProxyStatus: () =>
        Promise.resolve({
          running: true,
          port: 24871,
          hasUpstream: false,
          listenPort: 24871,
          lastError: proxyError,
        }),
      routerRecentRequests: () => Promise.resolve(recent),
      routerClearRequests: () => {
        recent = [];
        return Promise.resolve();
      },
    },
  };
});

const relay = (id: string, name: string) => ({ id, name }) as unknown as ProviderProfile;
const RELAYS = [relay("a", "中转 A"), relay("b", "中转 B"), relay("c", "中转 C")];

function renderIt(currentId: string | null = "a") {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const Wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return render(<RoutingPanel app="claude" relays={RELAYS} currentId={currentId} />, {
    wrapper: Wrapper,
  });
}

beforeEach(() => {
  stored = {
    enabled: false,
    failoverQueue: [],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  saved = [];
  status = [];
  resetCalls = [];
  proxyError = null;
  recent = [];
});

test("路由服务起不来时如实提示已保持直连", async () => {
  stored = {
    enabled: true,
    failoverQueue: [],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  proxyError = "绑定 127.0.0.1:24871 失败：Address already in use";
  renderIt();
  expect(await screen.findByText(/本地路由未能启动，客户端已保持直连/)).toBeTruthy();
});

test("最近请求展开后展示服务方、经过与用量", async () => {
  stored = {
    enabled: true,
    failoverQueue: ["b"],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  recent = [
    {
      id: 1,
      at: Date.now(),
      app: "claude",
      model: "glm-5",
      stream: true,
      providerId: "b",
      providerName: "中转 B",
      attempts: [{ providerId: "a", providerName: "中转 A", outcome: "HTTP 503" }],
      status: 200,
      firstByteMs: 800,
      durationMs: 4200,
      usage: { inputTokens: 1500, outputTokens: 300, cacheReadTokens: 0 },
      error: null,
    },
  ];
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "最近请求" }));
  expect(await screen.findByText("经过：中转 A（HTTP 503）")).toBeTruthy();
  expect(screen.getByText("首字 0.8s · 共 4.2s")).toBeTruthy();
  expect(screen.getByText(/↑1.5K ↓300/)).toBeTruthy();
});

test("开启路由即保存并展示转发顺序，当前供应商固定在第 1 位", async () => {
  renderIt();
  await userEvent.click(await screen.findByRole("switch", { name: "本地路由" }));
  await waitFor(() =>
    expect(saved).toEqual([
      { enabled: true, failoverQueue: [], tuning: DEFAULT_ROUTING_TUNING, strategy: "failover" },
    ]),
  );
  const list = await screen.findByRole("list");
  expect(within(list).getAllByRole("listitem")).toHaveLength(1);
  expect(within(list).getByText("当前")).toBeTruthy();
  expect(screen.getByText(/127\.0\.0\.1:24871/)).toBeTruthy();
});

test("备用供应商可添加、调整顺序与移出", async () => {
  stored = {
    enabled: true,
    failoverQueue: ["b", "c"],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  renderIt();
  const list = await screen.findByRole("list");
  await waitFor(() => expect(within(list).getAllByRole("listitem")).toHaveLength(3));

  const moveUp = screen.getAllByRole("button", { name: "上移" });
  expect(moveUp[0]).toHaveProperty("disabled", true);
  await userEvent.click(moveUp[1] as HTMLElement);
  await waitFor(() => expect(saved.at(-1)?.failoverQueue).toEqual(["c", "b"]));

  await userEvent.click(screen.getAllByRole("button", { name: "移出队列" })[0] as HTMLElement);
  await waitFor(() => expect(saved.at(-1)?.failoverQueue).toEqual(["b"]));
});

test("熔断中的供应商显示倒计时与最近错误，可手动恢复", async () => {
  stored = {
    enabled: true,
    failoverQueue: ["b"],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  status = [
    {
      app: "claude",
      lastServedBy: "b",
      sticky: { providerId: "b", remainingSecs: 37 },
      candidates: [
        {
          providerId: "a",
          name: "中转 A",
          primary: true,
          state: "open",
          consecutiveFailures: 4,
          openRemainingSecs: 42,
          lastError: "「中转 A」返回 HTTP 503",
          lastFailureAt: 1,
          served: 3,
          failed: 4,
          totals: { requests: 3, inputTokens: 12000, outputTokens: 800, cacheReadTokens: 0 },
          quotaExhaustedSecs: null,
        },
        {
          providerId: "b",
          name: "中转 B",
          primary: false,
          state: "closed",
          consecutiveFailures: 0,
          openRemainingSecs: null,
          lastError: null,
          lastFailureAt: null,
          served: 0,
          failed: 0,
          totals: { requests: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0 },
          quotaExhaustedSecs: null,
        },
      ],
    },
  ];
  renderIt();
  expect(await screen.findByText("已暂停，42 秒后重试")).toBeTruthy();
  expect(screen.getByText("「中转 A」返回 HTTP 503")).toBeTruthy();
  expect(screen.getByText("已响应 3 次 · 失败 4 次")).toBeTruthy();
  expect(screen.getByText(/输入 12K · 输出 800/)).toBeTruthy();
  expect(screen.getByText(/「中转 B」临时接替中；37 秒后/)).toBeTruthy();
  expect(screen.getByText("正常")).toBeTruthy();
  expect(screen.getByText("最近由它响应")).toBeTruthy();
  await userEvent.click(screen.getByRole("button", { name: "恢复" }));
  await waitFor(() => expect(resetCalls).toEqual(["a"]));
});

test("当前为官方登录时提示不经路由", async () => {
  stored = {
    enabled: true,
    failoverQueue: [],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  renderIt(null);
  expect(await screen.findByText(/当前为官方登录，不经本地路由/)).toBeTruthy();
});

test("高级设置默认折叠；超范围禁用保存并提示，合法值可保存", async () => {
  stored = {
    enabled: true,
    failoverQueue: [],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  renderIt();
  expect(screen.queryByLabelText("首字节超时（秒）", { exact: false })).toBeNull();

  await userEvent.click(await screen.findByRole("button", { name: "高级设置" }));
  const firstByte = await screen.findByLabelText("首字节超时（秒）", { exact: false });
  expect(firstByte).toHaveValue("60");
  const saveButton = screen.getByRole("button", { name: "保存" });
  expect(saveButton).toBeEnabled();

  await userEvent.clear(firstByte);
  await userEvent.type(firstByte, "5");
  expect(await screen.findByText("超出范围：10–600")).toBeTruthy();
  expect(saveButton).toBeDisabled();

  await userEvent.clear(firstByte);
  await userEvent.type(firstByte, "90");
  await waitFor(() => expect(saveButton).toBeEnabled());
  await userEvent.click(saveButton);
  await waitFor(() =>
    expect(saved.at(-1)?.tuning).toEqual({ ...DEFAULT_ROUTING_TUNING, firstByteTimeoutSecs: 90 }),
  );
});

test("保存只合并本次改动：窗口缓存过期时不撤回托盘上的开关操作", async () => {
  stored = {
    enabled: true,
    failoverQueue: [],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "高级设置" }));
  const firstByte = await screen.findByLabelText("首字节超时（秒）", { exact: false });
  // 窗口已加载后，用户在托盘关掉了本地路由（窗口缓存仍是开启）。
  stored = { ...stored, enabled: false };
  await userEvent.clear(firstByte);
  await userEvent.type(firstByte, "90");
  await userEvent.click(screen.getByRole("button", { name: "保存" }));
  await waitFor(() => expect(saved.at(-1)?.tuning.firstByteTimeoutSecs).toBe(90));
  expect(saved.at(-1)?.enabled).toBe(false);
});

test("高级设置「恢复默认」把草稿重置为缺省值", async () => {
  stored = {
    enabled: true,
    failoverQueue: [],
    tuning: {
      firstByteTimeoutSecs: 200,
      idleTimeoutSecs: 300,
      failureThreshold: 10,
      cooldownSecs: 900,
    },
    strategy: "failover",
  };
  renderIt();
  await userEvent.click(await screen.findByRole("button", { name: "高级设置" }));
  const firstByte = await screen.findByLabelText("首字节超时（秒）", { exact: false });
  expect(firstByte).toHaveValue("200");

  await userEvent.click(screen.getByRole("button", { name: "恢复默认" }));
  expect(firstByte).toHaveValue(String(DEFAULT_ROUTING_TUNING.firstByteTimeoutSecs));
  expect(await screen.findByLabelText("暂停时长（秒）", { exact: false })).toHaveValue(
    String(DEFAULT_ROUTING_TUNING.cooldownSecs),
  );
});

test("有备用供应商时可开启额度优先，保存为 quota-aware 且不动其余配置", async () => {
  stored = {
    enabled: true,
    failoverQueue: ["b"],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  renderIt();
  const toggle = await screen.findByRole("switch", { name: "额度用尽时自动让位" });
  expect(toggle.getAttribute("aria-checked")).toBe("false");
  await userEvent.click(toggle);
  await waitFor(() => expect(saved.at(-1)?.strategy).toBe("quota-aware"));
  expect(saved.at(-1)?.failoverQueue).toEqual(["b"]);
  await userEvent.click(await screen.findByRole("switch", { name: "额度用尽时自动让位" }));
  await waitFor(() => expect(saved.at(-1)?.strategy).toBe("failover"));
});

test("没有备用供应商时不展示额度优先开关", async () => {
  stored = {
    enabled: true,
    failoverQueue: [],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  renderIt();
  await screen.findByRole("list");
  expect(screen.queryByRole("switch", { name: "额度用尽时自动让位" })).toBeNull();
});

test("额度用尽被后置的候选显示预计恢复时间", async () => {
  stored = {
    enabled: true,
    failoverQueue: ["b", "c"],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "quota-aware",
  };
  const healthy = {
    state: "closed" as const,
    consecutiveFailures: 0,
    openRemainingSecs: null,
    lastError: null,
    lastFailureAt: null,
    served: 0,
    failed: 0,
    totals: { requests: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0 },
  };
  status = [
    {
      app: "claude",
      lastServedBy: null,
      sticky: null,
      candidates: [
        { ...healthy, providerId: "a", name: "中转 A", primary: true, quotaExhaustedSecs: 5400 },
        { ...healthy, providerId: "b", name: "中转 B", primary: false, quotaExhaustedSecs: 12600 },
        { ...healthy, providerId: "c", name: "中转 C", primary: false, quotaExhaustedSecs: null },
      ],
    },
  ];
  renderIt();
  expect(await screen.findByText("额度已用尽，约 90 分钟后恢复")).toBeTruthy();
  expect(screen.getByText("额度已用尽，约 4 小时后恢复")).toBeTruthy();
});

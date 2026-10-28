import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import type { ReactNode } from "react";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { PROVIDER_APPS } from "../lib/appVisibility";
import type { ClientApp, ClientEnv } from "../lib/ipc";
import { LocalEnvSection } from "./LocalEnvSection";

/**
 * 「装了但跑不起来」曾是这个面板给用户的全部交代——不说哪坏了、不说怎么办。
 * 这些用例钉住的是替代它的东西：每种失败各说各的原因，并把 CLI 自己的报错原文
 * 摆出来。断言落在用户读得到的文案上，而不是内部的 problem.kind。
 */

let envs: ClientEnv[] = [];

vi.mock("../lib/ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/ipc")>();
  return {
    ...actual,
    ipc: {
      clientEnvStatus: () => Promise.resolve(envs),
      openExternal: () => Promise.resolve(),
    },
  };
});

function env(app: ClientApp, over: Partial<ClientEnv> = {}): ClientEnv {
  return {
    app,
    installed: true,
    version: null,
    path: null,
    runnable: true,
    problem: null,
    conflicts: [],
    ...over,
  };
}

function renderIt() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const Wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return render(<LocalEnvSection />, { wrapper: Wrapper });
}

beforeEach(() => {
  envs = [];
});

/** 起不来 / 无响应 / 报错退出，补救动作各不相同，不能共用一句「跑不起来」。 */
test("三种失败各说各的原因，不再只报一句结论", async () => {
  envs = [
    env("codex", {
      runnable: false,
      path: "d:\\nvm4w\\nodejs\\codex",
      problem: { kind: "spawn", detail: "%1 不是有效的 Win32 应用程序。 (os error 193)" },
    }),
    env("gemini", {
      runnable: false,
      problem: { kind: "timeout", seconds: 10 },
    }),
    env("grok", {
      runnable: false,
      problem: { kind: "exit", code: 1, stderr: "Cannot find module 'dist/index.js'" },
    }),
  ];
  renderIt();

  expect(await screen.findByText("启动失败")).toBeInTheDocument();
  expect(screen.getByText("无响应")).toBeInTheDocument();
  expect(screen.getByText("报错退出")).toBeInTheDocument();
  expect(screen.queryByText("装了但跑不起来")).not.toBeInTheDocument();
});

/** 诊断的价值在于 CLI 自己说了什么——后端捞上来的 stderr 必须原样露出。 */
test("报错退出把退出码和 CLI 自己的报错原文摆出来", async () => {
  envs = [
    env("codex", {
      runnable: false,
      problem: { kind: "exit", code: 3, stderr: "Error: Cannot find module 'dist/index.js'" },
    }),
  ];
  renderIt();

  expect(await screen.findByText(/退出码 3/)).toBeInTheDocument();
  expect(screen.getByText(/Cannot find module/)).toBeInTheDocument();
});

/** 被信号杀死时没有退出码，那句"以退出码 null 退出"不能让用户看见。 */
test("没有退出码时换一句说法，不显示 null", async () => {
  envs = [env("codex", { runnable: false, problem: { kind: "exit", code: null, stderr: "" } })];
  renderIt();

  expect(await screen.findByText(/异常退出且未返回退出码/)).toBeInTheDocument();
  expect(screen.queryByText(/退出码 null/)).not.toBeInTheDocument();
});

/** 超时那条要给出等了多久，否则用户无从判断是自己网慢还是它真挂了。 */
test("无响应写明等待秒数", async () => {
  envs = [env("codex", { runnable: false, problem: { kind: "timeout", seconds: 10 } })];
  renderIt();

  expect(await screen.findByText(/已等待 10 秒/)).toBeInTheDocument();
});

/** 诊断块是全卡唯一的色块：没出问题时一个都不该冒出来。 */
test("装好的和没装的都不出诊断块", async () => {
  envs = [
    env("claude", { version: "2.1.216", path: "/usr/local/bin/claude" }),
    env("codex", { installed: false }),
  ];
  renderIt();

  expect(await screen.findByText("v2.1.216")).toBeInTheDocument();
  expect(screen.queryByRole("button", { name: /复制诊断/ })).not.toBeInTheDocument();
});

/** 出了问题才给「复制诊断」，内容要能独立看懂，所以路径必须在页面上。 */
test("有问题的行给出复制诊断入口", async () => {
  envs = [
    env("codex", {
      runnable: false,
      path: "d:\\nvm4w\\nodejs\\codex",
      problem: { kind: "spawn", detail: "os error 193" },
    }),
  ];
  renderIt();

  expect(await screen.findByRole("button", { name: /复制诊断/ })).toBeInTheDocument();
  expect(screen.getByTitle("d:\\nvm4w\\nodejs\\codex")).toBeInTheDocument();
});

/**
 * 摘要是"我该看哪一行"的答案，三个数必须等于列表行数——后端漏报某个 app 时
 * 那一行会显示「未检测到」，摘要就得把它算进未装，不能跳过。
 */
test("摘要计数与列表行数对得上，漏报的按未装计", async () => {
  envs = [
    env("claude", { version: "2.1.216" }),
    env("codex", { runnable: false, problem: { kind: "timeout", seconds: 10 } }),
  ];
  renderIt();

  const missing = PROVIDER_APPS.length - 2;
  expect(await screen.findByText("可用 1")).toBeInTheDocument();
  expect(screen.getByText("有问题 1")).toBeInTheDocument();
  expect(screen.getByText(`未装 ${missing}`)).toBeInTheDocument();
});

/** 没有问题时不显示「有问题 0」——零值噪声会让人以为有事。 */
test("全都正常时摘要不出现有问题那一段", async () => {
  envs = PROVIDER_APPS.map((app) => env(app, { version: "1.0.0" }));
  renderIt();

  expect(await screen.findByText(`可用 ${PROVIDER_APPS.length}`)).toBeInTheDocument();
  expect(screen.queryByText(/有问题/)).not.toBeInTheDocument();
});

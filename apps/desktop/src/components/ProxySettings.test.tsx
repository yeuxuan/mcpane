import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import type { DetectedProxy, ProxyConfig, ProxyTestResult } from "../lib/ipc";
import { ProxySettings } from "./ProxySettings";

let saved: ProxyConfig | null = null;
let stored: ProxyConfig = { mode: "system" };
let scanResult: DetectedProxy[] = [];
let testResult: ProxyTestResult = { ok: true, latencyMs: 42, error: null };
/** 置位后 proxy_set 报错，用于验证失败反馈。 */
let saveError: string | null = null;

vi.mock("../lib/ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../lib/ipc")>();
  return {
    ...actual,
    ipc: {
      proxyGet: () => Promise.resolve(stored),
      proxySet: (config: ProxyConfig) => {
        if (saveError) return Promise.reject(new Error(saveError));
        saved = config;
        stored = config;
        return Promise.resolve();
      },
      proxyScan: () => Promise.resolve(scanResult),
      proxyTest: () => Promise.resolve(testResult),
    },
  };
});

function renderIt() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const Wrapper = ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={client}>{children}</QueryClientProvider>
  );
  return render(<ProxySettings />, { wrapper: Wrapper });
}

beforeEach(() => {
  saved = null;
  stored = { mode: "system" };
  scanResult = [];
  testResult = { ok: true, latencyMs: 42, error: null };
  saveError = null;
});

/** 地址输入只在自定义模式下有意义，其余模式露出来只会让人以为它生效。 */
test("非自定义模式不显示地址输入框", async () => {
  renderIt();
  await screen.findByText("出网代理");
  expect(screen.queryByLabelText("代理地址")).toBeNull();
});

test("切到自定义后露出地址输入框", async () => {
  renderIt();
  await screen.findByText("出网代理");

  await userEvent.click(screen.getByRole("radio", { name: "自定义" }));

  expect(await screen.findByLabelText("代理地址")).toBeTruthy();
});

/**
 * 「直连」不是「清空地址」——它必须真的落库成 direct。
 * cc-switch 把两者混为一谈（URL 空即直连），结果是清空后 reqwest 仍读环境变量，
 * 用户以为关了其实没关。
 */
test("选直连即刻落库为 direct 而非空地址", async () => {
  renderIt();
  await screen.findByText("出网代理");

  await userEvent.click(screen.getByRole("radio", { name: "直连" }));

  await waitFor(() => expect(saved).toEqual({ mode: "direct" }));
});

test("选跟随系统即刻落库", async () => {
  stored = { mode: "direct" };
  renderIt();
  await screen.findByText("出网代理");

  await userEvent.click(screen.getByRole("radio", { name: "跟随系统" }));

  await waitFor(() => expect(saved).toEqual({ mode: "system" }));
});

/** 自定义模式要等用户填完再保存，不能边打字边写库。 */
test("自定义模式下切换模式本身不落库", async () => {
  renderIt();
  await screen.findByText("出网代理");

  await userEvent.click(screen.getByRole("radio", { name: "自定义" }));
  await screen.findByLabelText("代理地址");

  expect(saved).toBeNull();
});

test("保存自定义地址时去掉首尾空白", async () => {
  renderIt();
  await screen.findByText("出网代理");
  await userEvent.click(screen.getByRole("radio", { name: "自定义" }));

  await userEvent.type(await screen.findByLabelText("代理地址"), "  http://127.0.0.1:7897  ");
  await userEvent.click(screen.getByRole("button", { name: "保存" }));

  await waitFor(() => expect(saved).toEqual({ mode: "manual", url: "http://127.0.0.1:7897" }));
});

test("地址为空时保存与测试都不可点", async () => {
  renderIt();
  await screen.findByText("出网代理");
  await userEvent.click(screen.getByRole("radio", { name: "自定义" }));
  await screen.findByLabelText("代理地址");

  expect(screen.getByRole("button", { name: "保存" })).toHaveProperty("disabled", true);
  expect(screen.getByRole("button", { name: "测试" })).toHaveProperty("disabled", true);
});

test("已保存的自定义地址回填进输入框", async () => {
  stored = { mode: "manual", url: "socks5://127.0.0.1:7891" };
  renderIt();

  const field = await screen.findByLabelText("代理地址");
  expect(field).toHaveProperty("value", "socks5://127.0.0.1:7891");
});

test("扫描到结果后自动填入最快的一个", async () => {
  scanResult = [
    { url: "http://127.0.0.1:7897", port: 7897, hint: "Clash Verge / mihomo", latencyMs: 120 },
    { url: "socks5://127.0.0.1:7897", port: 7897, hint: "Clash Verge / mihomo", latencyMs: 300 },
  ];
  renderIt();
  await screen.findByText("出网代理");
  await userEvent.click(screen.getByRole("radio", { name: "自定义" }));

  await userEvent.click(await screen.findByRole("button", { name: "扫描" }));

  await waitFor(() =>
    expect(screen.getByLabelText("代理地址")).toHaveProperty("value", "http://127.0.0.1:7897"),
  );
  expect(await screen.findByRole("status")).toBeTruthy();
});

/** 扫不到要说清楚，不能静默——否则用户会以为按钮坏了。 */
test("扫描无结果时给出明确反馈且不改动地址", async () => {
  scanResult = [];
  renderIt();
  await screen.findByText("出网代理");
  await userEvent.click(screen.getByRole("radio", { name: "自定义" }));
  await userEvent.click(await screen.findByRole("button", { name: "扫描" }));

  expect(await screen.findByText("未发现可用的本地代理")).toBeTruthy();
  expect(screen.getByLabelText("代理地址")).toHaveProperty("value", "");
});

test("测试成功显示延迟", async () => {
  stored = { mode: "manual", url: "http://127.0.0.1:7897" };
  testResult = { ok: true, latencyMs: 42, error: null };
  renderIt();
  await screen.findByLabelText("代理地址");

  await userEvent.click(screen.getByRole("button", { name: "测试" }));

  expect(await screen.findByText("连接正常（42 ms）")).toBeTruthy();
});

test("测试失败显示后端给的原因", async () => {
  stored = { mode: "manual", url: "http://127.0.0.1:1" };
  testResult = { ok: false, latencyMs: null, error: "connection refused" };
  renderIt();
  await screen.findByLabelText("代理地址");

  await userEvent.click(screen.getByRole("button", { name: "测试" }));

  expect(await screen.findByText(/connection refused/)).toBeTruthy();
});

/**
 * 保存失败后选中态不能继续停在没存进去的那个模式上——界面显示的必须与
 * 实际生效的一致，否则用户以为切成直连了、其实还在走代理。
 *
 * 断言 `checked` 而非高亮用的类名：类名是实现细节（换个配色就红），
 * 而 `checked` 正是读屏用户听到的那个「已选中」，与用户感知一一对应。
 */
test("切换模式保存失败后回到服务端的真实模式", async () => {
  stored = { mode: "system" };
  saveError = "写库失败";
  renderIt();
  await screen.findByText("出网代理");

  await userEvent.click(screen.getByRole("radio", { name: "直连" }));
  await screen.findByText("写库失败");

  // 库里仍是 system，界面必须跟着退回去。
  await waitFor(() =>
    expect(screen.getByRole("radio", { name: "跟随系统" })).toHaveProperty("checked", true),
  );
  expect(screen.getByRole("radio", { name: "直连" })).toHaveProperty("checked", false);
});

/** 后端会拒掉非法地址，这个拒绝必须让用户看见，而不是静默失败。 */
test("保存失败时把后端的错误显示出来", async () => {
  saveError = "不支持的代理协议 `ftp`（支持 http / https / socks5 / socks5h）";
  renderIt();
  await screen.findByText("出网代理");
  await userEvent.click(screen.getByRole("radio", { name: "自定义" }));
  await userEvent.type(await screen.findByLabelText("代理地址"), "ftp://nope");
  await userEvent.click(screen.getByRole("button", { name: "保存" }));

  expect(await screen.findByText(/不支持的代理协议/)).toBeTruthy();
});

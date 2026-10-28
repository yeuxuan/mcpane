import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, test } from "vitest";
import { App } from "./App";

function renderApp() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return render(
    <QueryClientProvider client={queryClient}>
      <App />
    </QueryClientProvider>,
  );
}

test("应用壳渲染品牌、导航与 Hub 状态", () => {
  renderApp();
  expect(screen.getByRole("heading", { name: "MCPane" })).toBeInTheDocument();
  expect(screen.getByRole("navigation", { name: "主导航" })).toBeInTheDocument();
  // 无 Tauri runtime 时 Hub 状态兜底为未运行，不阻塞渲染。
  expect(screen.getByText("MCP Hub 未运行")).toBeInTheDocument();
});

test("导航切换页面", async () => {
  const user = userEvent.setup();
  renderApp();
  await user.click(screen.getByRole("button", { name: "供应商" }));
  expect(screen.getByRole("heading", { name: "供应商" })).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "MCP 服务器" }));
  expect(screen.getByRole("heading", { name: "MCP 服务器" })).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "工作区" }));
  expect(screen.getByRole("heading", { name: "工作区" })).toBeInTheDocument();
});

test("工作区设置经顶右切换器进入（不占侧栏）", async () => {
  const user = userEvent.setup();
  renderApp();
  // 侧栏是「工作区」总览页；明细的「工作区设置」页只从切换器 / 总览进
  expect(screen.queryByRole("button", { name: "工作区设置" })).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "切换工作区" }));
  await user.click(screen.getByRole("menuitem", { name: "管理工作区…" }));
  expect(screen.getByRole("heading", { name: "工作区设置" })).toBeInTheDocument();
});

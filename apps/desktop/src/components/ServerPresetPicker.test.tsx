import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import type { PresetFill } from "./ServerPresetPicker";
import { ServerPresetPicker } from "./ServerPresetPicker";

let opened: string[] = [];

vi.mock("../lib/ipc", () => ({
  ipc: {
    openExternal: (url: string) => {
      opened.push(url);
      return Promise.resolve();
    },
  },
}));

beforeEach(() => {
  opened = [];
});

function renderIt() {
  const picks: PresetFill[] = [];
  render(<ServerPresetPicker onPick={(fill) => picks.push(fill)} />);
  return picks;
}

/** 按名字定位，不按位置：调整预设顺序不该让测试因无关原因转红。 */
const use = (name: string) => screen.getByRole("button", { name: `使用 ${name}` });

test("列出官方预设并标注运行时前置", () => {
  renderIt();
  expect(screen.getByText("sequential-thinking")).toBeTruthy();
  expect(screen.getByText("github")).toBeTruthy();
  // GitHub 走远端 HTTP，不需要本地装任何东西——这是选它的理由，得说出来。
  expect(screen.getAllByText("无需本地安装").length).toBeGreaterThan(0);
  expect(screen.getAllByText("需要 Node.js").length).toBeGreaterThan(0);
});

/** 无参数的预设点一下就该走，不该再要一次「确认」。 */
test("无参数预设一键填入", async () => {
  const picks = renderIt();

  await userEvent.click(use("sequential-thinking"));

  await waitFor(() => expect(picks).toHaveLength(1));
  expect(picks[0]?.command).toBe("npx");
  expect(picks[0]?.args).toContain("@modelcontextprotocol/server-sequential-thinking");
  expect(picks[0]?.transport).toBe("stdio");
});

/** 需要参数的预设不能一点就走——得先把参数收上来。 */
test("有参数的预设先展开输入，不立即填入", async () => {
  const picks = renderIt();

  await userEvent.click(use("filesystem"));

  expect(await screen.findByLabelText(/允许访问的目录/)).toBeTruthy();
  expect(picks).toHaveLength(0);
});

test("必填参数没填时填入按钮不可点", async () => {
  renderIt();
  await userEvent.click(use("filesystem"));
  await screen.findByLabelText(/允许访问的目录/);

  expect(screen.getByRole("button", { name: /填入配置/ })).toHaveProperty("disabled", true);
});

test("填完参数后交出组装好的配置", async () => {
  const picks = renderIt();
  await userEvent.click(use("filesystem"));

  await userEvent.type(await screen.findByLabelText(/允许访问的目录/), "/Users/me/code");
  await userEvent.click(screen.getByRole("button", { name: /填入配置/ }));

  await waitFor(() => expect(picks).toHaveLength(1));
  expect(picks[0]?.args.at(-1)).toBe("/Users/me/code");
  expect(picks[0]?.transport).toBe("stdio");
});

/** 可选参数留空也该能填入——memory 的 MEMORY_FILE_PATH 不是必填。 */
test("可选参数留空不拦截", async () => {
  const picks = renderIt();
  await userEvent.click(use("memory"));
  await screen.findByLabelText(/记忆文件路径/);

  await userEvent.click(screen.getByRole("button", { name: /填入配置/ }));

  await waitFor(() => expect(picks).toHaveLength(1));
  expect(picks[0]?.env).toEqual({});
});

/** GitHub 走远端 HTTP：填出来的必须是 url + bearer，不是命令行。 */
test("GitHub 预设产出 HTTP 配置", async () => {
  const picks = renderIt();
  await userEvent.click(use("github"));

  await userEvent.type(await screen.findByLabelText(/GitHub Personal Access Token/), "ghp_x");
  await userEvent.click(screen.getByRole("button", { name: /填入配置/ }));

  await waitFor(() => expect(picks).toHaveLength(1));
  expect(picks[0]?.transport).toBe("http");
  expect(picks[0]?.url).toBe("https://api.githubcopilot.com/mcp/");
  expect(picks[0]?.bearer).toBe("ghp_x");
  expect(picks[0]?.command).toBe("");
});

/** Token 输入框不该在旁人路过时被看光。 */
test("Token 类输入用密码框", async () => {
  renderIt();
  await userEvent.click(use("github"));

  const field = await screen.findByLabelText(/GitHub Personal Access Token/);
  expect(field).toHaveProperty("type", "password");
});

/** 换一个预设时，上一个填的值不该串过去。 */
test("切换预设时清掉上一个的输入", async () => {
  renderIt();
  await userEvent.click(use("filesystem"));
  await userEvent.type(await screen.findByLabelText(/允许访问的目录/), "/tmp/a");

  await userEvent.click(use("git"));
  const field = await screen.findByLabelText(/Git 仓库路径/);

  expect(field).toHaveProperty("value", "");
});

test("文档链接走系统浏览器打开", async () => {
  renderIt();
  await userEvent.click(screen.getByRole("button", { name: "查看 filesystem 的文档" }));
  await waitFor(() => expect(opened).toHaveLength(1));
  expect(opened[0]).toContain("modelcontextprotocol/servers");
});

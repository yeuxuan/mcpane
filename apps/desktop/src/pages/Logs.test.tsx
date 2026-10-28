import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen, within } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import "../lib/i18n";
import { Logs } from "./Logs";

let statsResult: unknown = {
  total: 40,
  success: 38,
  avgDurationMs: 12.4,
  p95DurationMs: 250,
  byServer: [
    { key: "srv_gh", count: 30, errors: 2 },
    { key: "srv_fs", count: 10, errors: 0 },
  ],
  byMethod: [{ key: "tools/call", count: 40, errors: 2 }],
  byTool: [{ key: "github__search", count: 22, errors: 1 }],
  byConnection: [{ key: "client_codex", count: 30, errors: 3 }],
};

/** 仅 "请求日志读取失败" 测试翻转此标记；其余测试保持 logs_recent 正常返回。 */
let logsRecentShouldFail = false;
let logsRecentResult: unknown[] = [];
let logsGetResult: unknown = null;

/** 操作历史默认为空（只渲染空态）；回滚用例往里塞一条带快照的记录。 */
let historyResult: unknown[] = [];

const sampleLog = {
  id: "log_1",
  ts: 1_780_000_000,
  requestId: "req_1",
  clientConnectionId: "conn_1",
  sessionId: "sess_abc",
  workspaceId: "ws_1",
  serverId: "srv_gh",
  exposedName: "github__search",
  method: "tools/call",
  durationMs: 42,
  success: true,
  errorCategory: null,
  requestSize: 18,
  responseSize: 80,
  retryCount: 0,
};

const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
  switch (cmd) {
    case "logs_stats":
      return statsResult;
    case "logs_recent":
      if (logsRecentShouldFail) throw new Error("database is locked");
      return logsRecentResult;
    case "logs_get":
      if (!logsGetResult) throw new Error("not found");
      return logsGetResult;
    case "servers_list":
      // 统计里的 server_id 应映射成可读名称。
      return [{ id: "srv_gh", slug: "github", name: "GitHub", enabled: true }];
    case "workspaces_list":
      return [{ id: "ws_1", name: "默认工作区" }];
    case "hub_status":
      return {
        connections: [
          {
            clientConnectionId: "conn_1",
            clientType: "claude",
            tokenPrefix: "abcd1234",
            enabled: true,
            defaultWorkspaceId: null,
            toolExposureMode: "auto",
          },
        ],
      };
    case "config_history_list":
      return historyResult;
    default:
      return null;
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

function renderLogs() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <Logs />
    </QueryClientProvider>,
  );
}

test("统计仪表：成功率/P95 汇总，server_id 映射为名称，错误数标注", async () => {
  renderLogs();

  expect(await screen.findByText("95.0%")).toBeTruthy();
  expect(screen.getByText("250ms")).toBeTruthy();
  // srv_gh 经 servers_list 映射为 GitHub；未知 id 原样展示。
  expect(screen.getByText("GitHub")).toBeTruthy();
  expect(screen.getByText("srv_fs")).toBeTruthy();
  expect(screen.getByText("tools/call")).toBeTruthy();
  // 有错误的分组带错误标注（2 处：srv_gh 与 tools/call）。
  expect(screen.getAllByText("2 错")).toHaveLength(2);
});

test("空窗口：保留时间窗切换器与空态文案，不渲染零值仪表", async () => {
  statsResult = {
    total: 0,
    success: 0,
    avgDurationMs: 0,
    p95DurationMs: 0,
    byServer: [],
    byMethod: [],
    byTool: [],
    byConnection: [],
  };
  renderLogs();

  // 空窗口也要能切到其他窗口，切换器必须常驻。
  expect(await screen.findByText(/该时间窗内没有请求|No requests in this window/)).toBeTruthy();
  expect(screen.getByRole("radio", { name: /1 小时|1 hour/ })).toBeTruthy();
  // 零值仪表不渲染。
  expect(screen.queryByText(/成功率|Success rate/)).toBeNull();
});

test("时间窗切换：点击 7 天后按 hours=168 重新拉取统计", async () => {
  statsResult = {
    total: 40,
    success: 38,
    avgDurationMs: 12.4,
    p95DurationMs: 250,
    byServer: [],
    byMethod: [],
    byTool: [],
    byConnection: [],
  };
  renderLogs();

  const sevenDays = await screen.findByRole("radio", { name: /7 天|7 days/ });
  fireEvent.click(sevenDays);
  await vi.waitFor(() => {
    expect(invokeMock).toHaveBeenCalledWith("logs_stats", { hours: 168 });
  });
});

// logs_recent 读的是本地 SQLite（与 Hub 是否在跑无关）：isError 是真出错，
// 不是"还没数据"——回归覆盖这两种状态不能再共用同一句文案（曾经的缺陷：
// 加载失败时显示"暂无日志（Hub 未运行…）"，把错误说成了空态，归因也错）。
test("请求日志读取失败：提示读取失败，不出现「暂无日志」或 Hub 字样", async () => {
  logsRecentShouldFail = true;
  try {
    renderLogs();
    expect(await screen.findByText(/读取本地日志失败|Failed to read local logs/)).toBeTruthy();
    expect(screen.queryByText(/Hub/)).toBeNull();
    expect(screen.queryByText(/暂无日志|No logs yet/)).toBeNull();
  } finally {
    logsRecentShouldFail = false;
  }
});

test("请求日志真空表（无筛选）：显示「暂无日志」而非「暂无匹配日志」", async () => {
  renderLogs();
  expect(
    await screen.findByText(/暂无日志（还没有下游调用产生记录）|No logs yet \(no downstream/),
  ).toBeTruthy();
  expect(screen.queryByText(/暂无匹配日志|No matching logs/)).toBeNull();
});

/*
 * 回滚会覆盖磁盘上的配置文件，而历史列表里每行长得都一样——误点的代价与命中率
 * 都不低。此前这个按钮是一点即执行的，这条守的是那道后加的确认框：**点了按钮
 * 但还没确认时，config_rollback 一次都不能被调用**。只断言"弹出了对话框"是不够的，
 * 框弹出来了却照样把命令发出去，才是最坏的那种"看起来拦住了"。
 */
test("回滚要先确认：点按钮只弹框不执行，确认后才发命令", async () => {
  historyResult = [
    {
      id: "op_1",
      createdUnixSecs: 1_780_000_000,
      targetPath: "/Users/dev/.codex/config.toml",
      status: "applied",
      hashBefore: "aaa",
      hashAfter: "bbb",
      snapshot: { id: "snap_1", sourcePath: "/Users/dev/.codex/config.toml", storedAt: "" },
      changedPaths: ["mcp_servers.mcpane"],
      description: "接入 Codex",
    },
  ];
  try {
    renderLogs();
    // 页签走 SegmentedControl（原生 radio 单选组），不是 role=tab。
    fireEvent.click(await screen.findByRole("radio", { name: /操作历史|History/ }));

    fireEvent.click(await screen.findByRole("button", { name: /回滚|Roll back/ }));
    // 确认框已出现……
    expect(await screen.findByText(/回滚这次改动|Roll back this change/)).toBeTruthy();
    // ……但命令还一次都没发。
    expect(invokeMock).not.toHaveBeenCalledWith("config_rollback", expect.anything());

    const dialog = screen.getByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: /^(回滚|Roll back)$/ }));
    await vi.waitFor(() => {
      // 只传 id：快照与目标路径由后端从历史库回查（IPC 信任边界）。
      expect(invokeMock).toHaveBeenCalledWith("config_rollback", { recordId: "op_1" });
    });
  } finally {
    historyResult = [];
  }
});

test("点开一行：弹出详情，展示脱敏后的参数与返回", async () => {
  logsRecentResult = [sampleLog];
  logsGetResult = {
    ...sampleLog,
    requestSummary: JSON.stringify({ q: "mcp router" }),
    responseSummary: JSON.stringify({ content: [{ type: "text", text: "found 3" }] }),
  };
  try {
    renderLogs();
    // 表格里工具名剥掉与服务器列重复的 "github__" 前缀，详情里仍给全名。
    fireEvent.click(await screen.findByText("search"));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("github__search")).toBeTruthy();
    expect(await within(dialog).findByText(/mcp router/)).toBeTruthy();
    expect(within(dialog).getByText(/参数|Arguments/)).toBeTruthy();
    expect(within(dialog).getByText(/返回|Result/)).toBeTruthy();
    expect(within(dialog).getByText(/found 3/)).toBeTruthy();
    expect(within(dialog).getByText("GitHub")).toBeTruthy();
    expect(invokeMock).toHaveBeenCalledWith("logs_get", { id: "log_1" });
  } finally {
    logsRecentResult = [];
    logsGetResult = null;
  }
});

test("升级前的记录：详情只提示没有载荷，不假装有空 JSON", async () => {
  logsRecentResult = [sampleLog];
  logsGetResult = { ...sampleLog };
  try {
    renderLogs();
    fireEvent.click(await screen.findByText("search"));
    expect(
      await screen.findByText(/这条记录写于升级前|recorded before payload capture/),
    ).toBeTruthy();
    expect(screen.queryByText(/mcp router/)).toBeNull();
  } finally {
    logsRecentResult = [];
    logsGetResult = null;
  }
});

/*
 * 「写于升级前」的判据是**三列全空**（参数 / 返回 / 错误原文）。少算任何一项，
 * 就会对一条崭新的记录说它写于升级前——那是一句会撒谎的提示。
 *
 * 最容易被简化掉的是错误原文那一项：一次调用可以没有参数（无参工具）、没有返回
 * （下游直接报错），但只要留下了错误原文，它就不是旧记录。
 */
test("只有错误原文的新记录，不能被说成「写于升级前」", async () => {
  const failed = { ...sampleLog, success: false, errorCategory: "upstream" };
  logsRecentResult = [failed];
  logsGetResult = { ...failed, errorMessage: "upstream returned 502" };
  try {
    renderLogs();
    fireEvent.click(await screen.findByText("search"));
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findByText("upstream returned 502")).toBeTruthy();
    expect(within(dialog).queryByText(/写于升级前|recorded before payload capture/)).toBeNull();
  } finally {
    logsRecentResult = [];
    logsGetResult = null;
  }
});

test("结果筛选：只看失败时成功行消失", async () => {
  logsRecentResult = [sampleLog];
  try {
    renderLogs();
    expect(await screen.findByText("search")).toBeTruthy();
    fireEvent.click(screen.getByRole("radio", { name: /失败|Failed/ }));
    expect(await screen.findByText(/暂无匹配日志|No matching logs/)).toBeTruthy();
    expect(screen.queryByText("search")).toBeNull();
  } finally {
    logsRecentResult = [];
  }
});

test("失败详情：展示错误原文与分类", async () => {
  const failed = {
    ...sampleLog,
    success: false,
    errorCategory: "tool",
  };
  logsRecentResult = [failed];
  logsGetResult = {
    ...failed,
    errorMessage: "missing required query",
    requestSummary: JSON.stringify({}),
    responseSummary: JSON.stringify({ isError: true }),
  };
  try {
    renderLogs();
    fireEvent.click(await screen.findByText("search"));
    const dialog = await screen.findByRole("dialog");
    expect(await within(dialog).findByText("missing required query")).toBeTruthy();
    expect(within(dialog).getByText("tool")).toBeTruthy();
  } finally {
    logsRecentResult = [];
    logsGetResult = null;
  }
});

/*
 * 这张表在 v0.3.2 上是「hover 高亮但点不动」的：行有 hover 底色（视觉上明说
 * 我能点），却没有任何 onClick，箭头也 opacity-0 藏着。守两件事——整行可点，
 * 以及行尾那个箭头是**真按钮**且带无障碍名，键盘用户够得着。
 */
test("整行可点，行尾箭头是可聚焦的真按钮", async () => {
  logsRecentResult = [sampleLog];
  logsGetResult = { ...sampleLog };
  try {
    renderLogs();
    const open = await screen.findByRole("button", {
      name: /查看 github__search|View call details/,
    });
    // 藏起来的按钮拿不到无障碍名，这条同时守住了「箭头不能再 opacity-0」。
    expect(open).toBeTruthy();
    open.focus();
    expect(document.activeElement).toBe(open);
    fireEvent.click(open);
    expect(await screen.findByRole("dialog")).toBeTruthy();
  } finally {
    logsRecentResult = [];
    logsGetResult = null;
  }
});

/*
 * 500 行就是 500 个 Tab 停靠点：想跳过这张表得按 500 次。ARIA 的 grid/listbox
 * 模式是「进出用 Tab、内部用方向键」，所以表里只留一个停靠点。
 */
test("整张表只占一个 Tab 停靠点，方向键才在行间走", async () => {
  const now = Math.floor(Date.now() / 1000);
  logsRecentResult = [
    { ...sampleLog, id: "log_a", ts: now },
    { ...sampleLog, id: "log_b", ts: now - 60 },
    { ...sampleLog, id: "log_c", ts: now - 120 },
  ];
  try {
    renderLogs();
    const rows = await screen.findAllByRole("button", {
      name: /查看 github__search|View call details/,
    });
    expect(rows.map((r) => r.tabIndex)).toEqual([0, -1, -1]);
    // 焦点走到哪，停靠点就跟到哪——否则重新 Tab 进来会弹回第一行。
    const [first, second] = rows;
    if (!first || !second) throw new Error("应有三个行按钮");
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowDown" });
    expect(rows.map((r) => r.tabIndex)).toEqual([-1, 0, -1]);
  } finally {
    logsRecentResult = [];
  }
});

/*
 * 键盘打开详情后按 Esc，焦点必须回到刚才那一行——否则得从页首一路 Tab 回来，
 * 而 roving tabindex 只会把人送回第一行，原来看到哪儿就丢了。
 */
test("关掉详情后，焦点回到打开它的那一行", async () => {
  logsRecentResult = [sampleLog];
  logsGetResult = { ...sampleLog };
  try {
    renderLogs();
    const open = await screen.findByRole("button", {
      name: /查看 github__search|View call details/,
    });
    open.focus();
    fireEvent.click(open);
    const dialog = await screen.findByRole("dialog");
    expect(document.activeElement).not.toBe(open);
    fireEvent.keyDown(window, { key: "Escape" });
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(dialog).toBeTruthy();
    expect(document.activeElement).toBe(open);
  } finally {
    logsRecentResult = [];
    logsGetResult = null;
  }
});

test("上下键在行间移动焦点，跳过日期分隔行", async () => {
  const now = Math.floor(Date.now() / 1000);
  logsRecentResult = [
    { ...sampleLog, id: "log_a", ts: now },
    { ...sampleLog, id: "log_b", ts: now - 3 * 86400 },
  ];
  try {
    renderLogs();
    const rows = await screen.findAllByRole("button", {
      name: /查看 github__search|View call details/,
    });
    expect(rows).toHaveLength(2);
    const [first, second] = rows;
    if (!first || !second) throw new Error("应有两个行按钮");
    first.focus();
    fireEvent.keyDown(first, { key: "ArrowDown" });
    // 两条之间隔着一个日期分隔行，走位不能在它上面停一拍。
    expect(document.activeElement).toBe(second);
    fireEvent.keyDown(second, { key: "ArrowUp" });
    expect(document.activeElement).toBe(first);
  } finally {
    logsRecentResult = [];
  }
});

/*
 * 时刻列只写 HH:MM:SS，而「最近 500 条」在日常使用下常常横跨好几天——没有日期
 * 分段，前天的失败和一分钟前的失败长得一模一样。
 */
test("跨天日志按自然日分段，当天有失败时写明数目", async () => {
  const now = Math.floor(Date.now() / 1000);
  logsRecentResult = [
    { ...sampleLog, id: "log_a", ts: now },
    { ...sampleLog, id: "log_b", ts: now, success: false, errorCategory: "timeout" },
    { ...sampleLog, id: "log_c", ts: now - 86400 },
  ];
  try {
    renderLogs();
    expect(await screen.findByText(/^今天$|^Today$/)).toBeTruthy();
    expect(screen.getByText(/^昨天$|^Yesterday$/)).toBeTruthy();
    expect(screen.getByText(/2 次调用 · 1 失败|2 calls · 1 failed/)).toBeTruthy();
    expect(screen.getByText(/^1 次调用$|^1 calls$/)).toBeTruthy();
  } finally {
    logsRecentResult = [];
  }
});

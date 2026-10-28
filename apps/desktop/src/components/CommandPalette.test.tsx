import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { WorkspaceProvider } from "../lib/workspace";
import { CommandPalette } from "./CommandPalette";

const providersList = vi.fn();
const providerCurrent = vi.fn();
const providerLiveIds = vi.fn();
const providerSwitch = vi.fn();
const serversList = vi.fn();
const serverSetEnabled = vi.fn();
const workspacesList = vi.fn();

vi.mock("../lib/ipc", () => ({
  ipc: {
    providersList: () => providersList(),
    providerCurrent: () => providerCurrent(),
    providerLiveIds: (app: string) => providerLiveIds(app),
    providerSwitch: (app: string, id: string) => providerSwitch(app, id),
    serversList: () => serversList(),
    serverSetEnabled: (id: string, enabled: boolean) => serverSetEnabled(id, enabled),
    workspacesList: () => workspacesList(),
  },
}));

const PROVIDERS = [
  { id: "p-claude-1", app: "claude", name: "Relay A", sortOrder: 0 },
  { id: "p-claude-2", app: "claude", name: "Relay B", sortOrder: 1 },
  { id: "p-codex-1", app: "codex", name: "Relay C", sortOrder: 0 },
];

// claude 当前生效的是 p-claude-1，其余 app 均从未切换过。
const CURRENT = {
  claude: "p-claude-1",
  codex: null,
  gemini: null,
  grok: null,
  opencode: null,
  openclaw: null,
  hermes: null,
  "claude-desktop": null,
};

const SERVERS = [{ id: "s1", slug: "context7", name: "Context7", enabled: true }];

beforeEach(() => {
  providersList.mockReset().mockResolvedValue(PROVIDERS);
  providerCurrent.mockReset().mockResolvedValue(CURRENT);
  providerLiveIds.mockReset().mockResolvedValue([]);
  providerSwitch.mockReset().mockResolvedValue({ written: true, warnings: [] });
  serversList.mockReset().mockResolvedValue(SERVERS);
  serverSetEnabled.mockReset().mockResolvedValue(undefined);
  workspacesList.mockReset().mockResolvedValue([]);
});

function renderPalette() {
  const onNavigate = vi.fn();
  const onClose = vi.fn();
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(
    <QueryClientProvider client={queryClient}>
      <WorkspaceProvider>
        <CommandPalette onNavigate={onNavigate} onClose={onClose} />
      </WorkspaceProvider>
    </QueryClientProvider>,
  );
  return { onNavigate, onClose };
}

test("输入过滤：跨分组按子串匹配，大小写不敏感", async () => {
  const user = userEvent.setup();
  renderPalette();
  const input = await screen.findByRole("combobox", { name: "快速操作" });
  await screen.findByText("Claude Code → Relay A");
  expect(screen.getByText("Claude Code → Relay B")).toBeInTheDocument();
  expect(screen.getByText("Codex → Relay C")).toBeInTheDocument();
  expect(screen.getByText("概览")).toBeInTheDocument();

  await user.type(input, "relay b");
  expect(screen.queryByText("Claude Code → Relay A")).toBeNull();
  expect(screen.getByText("Claude Code → Relay B")).toBeInTheDocument();
  expect(screen.queryByText("概览")).toBeNull();
});

test("↓ 移动高亮，Enter 执行页面项并关闭面板", async () => {
  const user = userEvent.setup();
  const { onNavigate, onClose } = renderPalette();
  const input = await screen.findByRole("combobox", { name: "快速操作" });
  await screen.findByText("Claude Code → Relay A");

  await user.type(input, "{ArrowDown}{Enter}");
  expect(onNavigate).toHaveBeenCalledWith("mcp-servers");
  expect(onClose).toHaveBeenCalled();
});

test("当前生效的供应商项标注为当前且不可执行", async () => {
  const user = userEvent.setup();
  renderPalette();
  await screen.findByText("Claude Code → Relay A");

  const currentRow = screen.getByRole("option", { name: /Relay A/ });
  expect(currentRow).toHaveAttribute("aria-disabled", "true");
  await user.click(currentRow);
  expect(providerSwitch).not.toHaveBeenCalled();

  const otherRow = screen.getByRole("option", { name: /Relay B/ });
  expect(otherRow).not.toHaveAttribute("aria-disabled");
  await user.click(otherRow);
  await waitFor(() => expect(providerSwitch).toHaveBeenCalledWith("claude", "p-claude-2"));
});

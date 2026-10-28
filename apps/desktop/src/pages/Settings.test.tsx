import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";
import "../lib/i18n";
import { TourProvider } from "../lib/tourContext";
import { Settings } from "./Settings";

type Listener = () => void;
const listeners: Listener[] = [];

vi.mock("@tauri-apps/api/event", () => ({
  listen: (_name: string, cb: Listener) => {
    listeners.push(cb);
    return Promise.resolve(() => {});
  },
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string) => {
    // 数据备份/云同步分区在挂载时拉取，形状必须对（{} 会让表单 useEffect 抛错）。
    if (cmd === "backup_list") return Promise.resolve([]);
    if (cmd === "data_overview") {
      return Promise.resolve({
        dbBytes: 0,
        logs: { rows: 0, oldestTs: null, prunable: 0, retainDays: 30 },
        usage: { rows: 0, oldestTs: null, prunable: 0, retainDays: 90 },
      });
    }
    if (cmd === "sync_settings_get") {
      return Promise.resolve({
        provider: "off",
        autoSync: false,
        webdav: { baseUrl: "", username: "", remoteRoot: "", hasPassword: false },
        s3: {
          region: "",
          bucket: "",
          accessKeyId: "",
          endpoint: "",
          remoteRoot: "",
          hasSecretKey: false,
        },
        status: {
          lastSyncAt: null,
          lastSnapshotId: null,
          lastDeviceName: null,
          lastError: null,
          lastErrorSource: null,
        },
      });
    }
    return Promise.resolve({});
  },
}));

let hubEnabledValue: boolean;
let processStateValue: { state: string; attempt?: number; reason?: string };
let autostartValue: boolean;
/** 系统实际接受的结果：null = 照单全收，否则无论请求什么都返回它（模拟被策略挡下）。 */
let autostartAccepts: boolean | null;
let autostartReadFails: boolean;
/** 非空 = autostartSet 直接失败（如开发构建拒写）。 */
let autostartSetError: string | null;
/** Hub 离线时 hub_status 直接失败，「关于」分区的协议/版本两行只能留空。 */
let hubStatusFails: boolean;
/** true = hub_status 一直不回，用来观察请求在飞时那两行显示什么。 */
let hubStatusHangs: boolean;

vi.mock("../lib/ipc", () => ({
  ipc: {
    appInfo: () =>
      Promise.resolve({
        dataDir: "/tmp/mcpane",
        dbPath: "/tmp/mcpane/mcpane.db",
        hubEndpoint: "http://127.0.0.1:24860/mcp",
        appVersion: "0.2.1",
      }),
    hubStatus: () =>
      hubStatusHangs
        ? new Promise(() => {})
        : hubStatusFails
          ? Promise.reject(new Error("hub unreachable"))
          : Promise.resolve({
              name: "mcpane-hub",
              serverVersion: "0.2.1",
              protocolVersion: "2026-07-28",
              legacyProtocolVersion: "2025-11-25",
              endpoint: "http://127.0.0.1:24860/mcp",
              uptimeSecs: 1,
              connections: [],
            }),
    hubEnabledGet: () => Promise.resolve(hubEnabledValue),
    hubProcessState: () => Promise.resolve(processStateValue),
    hubSetEnabled: (enabled: boolean) => {
      hubEnabledValue = enabled;
      return Promise.resolve();
    },
    autostartGet: () =>
      autostartReadFails
        ? Promise.reject(new Error("no autolaunch manager"))
        : Promise.resolve(autostartValue),
    autostartSet: (enabled: boolean) => {
      if (autostartSetError) return Promise.reject(autostartSetError);
      autostartValue = autostartAccepts ?? enabled;
      return Promise.resolve(autostartValue);
    },
    // 以下六个是设置页各分区挂载时就会发起的查询，本文件的用例不断言它们，
    // 但 vi.mock 整体替换了 ipc：缺桩会让 queryFn 变成 undefined，TanStack
    // 每个用例刷一屏「No queryFn」警告，把真回归淹在噪声里。形状给到
    // 组件实际读取的字段为止。
    providerClientsInstalled: () => Promise.resolve({}),
    pricingSyncStatus: () =>
      Promise.resolve({ enabled: true, lastSyncedAt: null, lastWritten: null, lastError: null }),
    autoUpdateCheckGet: () => Promise.resolve(true),
    proxyGet: () => Promise.resolve({ mode: "system" }),
    providerConfigDirsGet: () => Promise.resolve({}),
    clientEnvStatus: () => Promise.resolve([]),
  },
}));

function renderSettings() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  // 设置页里的「重新运行引导」要读导览上下文——它在真实应用里就活在 Provider 内。
  return render(
    <QueryClientProvider client={queryClient}>
      <TourProvider page="settings">
        <Settings />
      </TourProvider>
    </QueryClientProvider>,
  );
}

function emitProcessState(next: { state: string; attempt?: number; reason?: string }) {
  processStateValue = next;
  for (const cb of listeners) cb();
}

beforeEach(() => {
  hubEnabledValue = false;
  processStateValue = { state: "disabled" };
  autostartValue = false;
  autostartAccepts = null;
  autostartReadFails = false;
  autostartSetError = null;
  hubStatusFails = false;
  hubStatusHangs = false;
  listeners.length = 0;
  // Hub 开关在「环境」分区；设置页记住上次分区，这里直接预置，
  // 免得每个用例都先点一次分区切换器。
  localStorage.setItem("mcpane:settings-section", "env");
});

// 回归用例：hub_set_enabled 的 invoke 只是 store 一个原子量就返回，
// supervisor 真正拉起子进程是异步的——开关不能在 mutation resolve 的那一刻就以为大功告成，
// 必须一直等到状态机报告 managed/disowned 才收起 loading。
test("开启 Hub：mutation 已 resolve 但进程还没被 supervisor 真正拉起时，开关继续显示 loading", async () => {
  const user = userEvent.setup();
  renderSettings();

  const toggle = await screen.findByRole("switch", { name: "开启 MCP Hub" });
  await user.click(toggle);

  // mutation 已经 resolve（hubEnabledValue 已翻转），但 supervisor 还没来得及
  // 把状态从 disabled 转过去——此时如果只看 mutation.isPending 会误判「已完成」。
  await waitFor(() => expect(toggle).toHaveAttribute("aria-busy", "true"));
  expect(toggle).toBeDisabled();

  // supervisor 进入拉起流程：继续转圈，且状态徽章实时可见。
  emitProcessState({ state: "starting" });
  await waitFor(() => expect(screen.getByText("启动中…")).toBeInTheDocument());
  expect(toggle).toHaveAttribute("aria-busy", "true");

  // 只有真正探活确认 managed，才算数，loading 才能收起。
  emitProcessState({ state: "managed" });
  await waitFor(() => expect(toggle).toHaveAttribute("aria-busy", "false"));
  expect(toggle).not.toBeDisabled();
  expect(screen.getByText("运行中")).toBeInTheDocument();
});

test("关闭 Hub：进程确认被杀掉（disabled）之前持续 loading", async () => {
  hubEnabledValue = true;
  processStateValue = { state: "managed" };
  const user = userEvent.setup();
  renderSettings();

  const toggle = await screen.findByRole("switch", { name: "关闭 MCP Hub" });
  await user.click(toggle);

  // mutation 秒回，但子进程还是 managed（kill 尚未完成）：必须继续转圈。
  await waitFor(() => expect(toggle).toHaveAttribute("aria-busy", "true"));

  emitProcessState({ state: "disabled" });
  await waitFor(() => expect(toggle).toHaveAttribute("aria-busy", "false"));
  await waitFor(() => expect(screen.getByText("已关闭")).toBeInTheDocument());
});

test("开启 Hub 失败（如端口被占用）：不无限转圈，loading 收起并显示失败徽章", async () => {
  const user = userEvent.setup();
  renderSettings();

  const toggle = await screen.findByRole("switch", { name: "开启 MCP Hub" });
  await user.click(toggle);
  await waitFor(() => expect(toggle).toHaveAttribute("aria-busy", "true"));

  emitProcessState({ state: "failed", reason: "portOccupied" });
  await waitFor(() => expect(toggle).toHaveAttribute("aria-busy", "false"));
  expect(screen.getByText("端口 24860 被占用")).toBeInTheDocument();
});

/** 开机自启在「通用」分区，与 Hub 那几个用例的预置分区不同。 */
function renderGeneralSection() {
  localStorage.setItem("mcpane:settings-section", "general");
  return renderSettings();
}

test("开机自启：开关状态来自系统实测值，点击后翻转", async () => {
  const user = userEvent.setup();
  renderGeneralSection();

  const toggle = await screen.findByRole("switch", { name: "开启开机自启" });
  await user.click(toggle);

  await waitFor(() => expect(screen.getByRole("switch", { name: "关闭开机自启" })).toBeChecked());
  expect(autostartValue).toBe(true);
});

// 回归用例：写系统自启动项可能被安全策略静默挡下（只读的 LaunchAgents 目录、
// 企业管控的注册表），命令回的是写完实测的状态。开关若回显入参，用户会以为
// 开好了，直到下次开机才发现没起来——必须跟随实测值并说明原因。
test("开机自启：系统拒绝写入时开关退回原状并给出原因", async () => {
  autostartAccepts = false;
  const user = userEvent.setup();
  renderGeneralSection();

  const toggle = await screen.findByRole("switch", { name: "开启开机自启" });
  await user.click(toggle);

  await waitFor(() =>
    expect(screen.getByText("系统未接受这项设置，请在系统的登录项设置中检查")).toBeInTheDocument(),
  );
  expect(screen.getByRole("switch", { name: "开启开机自启" })).not.toBeChecked();
});

// 读不到系统状态（非 Tauri 环境 / 插件未就绪）时不能装作"关着"还能点：
// 那会让用户点一下、以为开了，实际什么也没发生。标灰并说明。
test("开机自启：读取系统状态失败时开关标灰且说明原因", async () => {
  autostartReadFails = true;
  renderGeneralSection();

  const toggle = await screen.findByRole("switch", { name: "开启开机自启" });
  await waitFor(() => expect(toggle).toBeDisabled());
  expect(screen.getByText("读取不到系统自启动状态，此项暂不可用")).toBeInTheDocument();
});

test("开机自启：开发构建拒写时给出安装后才能开的说明", async () => {
  autostartSetError = "unpackaged-build";
  const user = userEvent.setup();
  renderGeneralSection();

  const toggle = await screen.findByRole("switch", { name: "开启开机自启" });
  await user.click(toggle);

  await waitFor(() =>
    expect(screen.getByText("开发构建不能写入登录项，请使用安装后的 MCPane")).toBeInTheDocument(),
  );
  expect(screen.getByRole("switch", { name: "开启开机自启" })).not.toBeChecked();
});

// 回归用例：转圈只跟用户点击走。应用启动期 supervisor 在后台拉起 hub 时打开设置页，
// 开关必须可用（用户没点过它），启动进度由状态徽章呈现而不是锁死开关。
test("后台启动期打开设置页：开关不锁、不转圈，状态徽章显示启动中", async () => {
  hubEnabledValue = true;
  processStateValue = { state: "starting" };
  renderSettings();

  const toggle = await screen.findByRole("switch", { name: "关闭 MCP Hub" });
  expect(toggle).toHaveAttribute("aria-busy", "false");
  expect(toggle).not.toBeDisabled();
  expect(await screen.findByText("启动中…")).toBeInTheDocument();

  // 状态迁移实时反映到徽章。
  emitProcessState({ state: "backoff", attempt: 2 });
  await waitFor(() => expect(screen.getByText("启动重试中（第 2 次）")).toBeInTheDocument());
  emitProcessState({ state: "managed" });
  await waitFor(() => expect(screen.getByText("运行中")).toBeInTheDocument());
  expect(toggle).toHaveAttribute("aria-busy", "false");
});

/** 协议/版本这类常量从首页挪到了「关于」，这里守住它们没在搬家途中丢掉。 */
function renderAboutSection() {
  localStorage.setItem("mcpane:settings-section", "about");
  return renderSettings();
}

test("关于：展示 MCP 协议版本与 Hub 版本", async () => {
  renderAboutSection();

  expect(await screen.findByText("MCP 2026-07-28 · 兼容 2025-11-25 及更早")).toBeInTheDocument();
  expect(screen.getByText("MCP 协议版本")).toBeInTheDocument();
  expect(screen.getByText("v0.2.1")).toBeInTheDocument();
  expect(screen.getByText("Hub 版本")).toBeInTheDocument();
});

// Hub 离线时这两项无从得知——不能拿上次的值糊弄，也不能显示成空白让人以为没这功能。
test("关于：Hub 不可达时协议/版本如实标注未运行", async () => {
  hubStatusFails = true;
  renderAboutSection();

  await waitFor(() => expect(screen.getAllByText("Hub 未运行")).toHaveLength(2));
  expect(screen.queryByText("MCP 2026-07-28 · 兼容 2025-11-25 及更早")).not.toBeInTheDocument();
});

// 请求还在飞就写「Hub 未运行」，等于拿加载中当结论——Hub 正常时也会闪一下这句假话。
test("关于：hub_status 未返回前不预判成未运行", async () => {
  hubStatusHangs = true;
  renderAboutSection();

  expect(await screen.findByText("MCP 协议版本")).toBeInTheDocument();
  expect(screen.queryByText("Hub 未运行")).not.toBeInTheDocument();
});

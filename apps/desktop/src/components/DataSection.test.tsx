import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen } from "@testing-library/react";
import { expect, test, vi } from "vitest";
import "../lib/i18n";
import { CloudSyncSection } from "./DataSection";

/** 可按用例覆写的服务端配置（RHF 经 values 选项落表单）。 */
let settingsResult: Record<string, unknown> = {};

function webdavSettings(overrides: Record<string, unknown> = {}) {
  return {
    provider: "webdav",
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
    ...overrides,
  };
}

/** 可按用例覆写的远端探测与上传结果。 */
let remoteInfoResult: Record<string, unknown> = { exists: false };
let uploadResult: Record<string, unknown> = { outcome: null, conflict: null };

const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
  switch (cmd) {
    case "sync_settings_get":
      return settingsResult;
    case "sync_settings_set":
      return settingsResult;
    case "sync_remote_info":
      return remoteInfoResult;
    case "sync_upload":
      return uploadResult;
    default:
      return null;
  }
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

function renderSection() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={queryClient}>
      <CloudSyncSection />
    </QueryClientProvider>,
  );
}

test("WebDAV 必填校验：空表单点保存 → 内联报错且不落库", async () => {
  settingsResult = webdavSettings();
  renderSection();

  fireEvent.click(await screen.findByRole("button", { name: /保存|Save/ }));

  // 地址/用户名/密码（未存过凭据）三处必填。
  expect(await screen.findAllByText(/必填|Required/)).toHaveLength(3);
  expect(invokeMock).not.toHaveBeenCalledWith("sync_settings_set", expect.anything());
});

test("WebDAV URL 格式校验：非 http(s) 地址被拒", async () => {
  settingsResult = webdavSettings();
  renderSection();

  const url = await screen.findByPlaceholderText("https://dav.jianguoyun.com/dav/mcpane");
  fireEvent.input(url, { target: { value: "ftp://dav.example.com" } });
  fireEvent.click(screen.getByRole("button", { name: /保存|Save/ }));

  expect(await screen.findByText(/http:\/\/ 或 https:\/\/|http:\/\/ or https:\/\//)).toBeTruthy();
  expect(invokeMock).not.toHaveBeenCalledWith("sync_settings_set", expect.anything());
});

test("已存凭据可留空沿用：合法输入保存成功，password 传 null", async () => {
  settingsResult = webdavSettings({
    webdav: {
      baseUrl: "https://dav.example.com/dav/box",
      username: "user",
      remoteRoot: "",
      hasPassword: true,
    },
  });
  renderSection();

  // 服务端数据预填可见（守住 Field 的 ref-as-prop 透传：断链时预填不显示）。
  const url = await screen.findByPlaceholderText<HTMLInputElement>(
    "https://dav.jianguoyun.com/dav/mcpane",
  );
  await vi.waitFor(() => {
    expect(url.value).toBe("https://dav.example.com/dav/box");
  });

  fireEvent.click(screen.getByRole("button", { name: /保存|Save/ }));

  await vi.waitFor(() => {
    expect(invokeMock).toHaveBeenCalledWith(
      "sync_settings_set",
      expect.objectContaining({
        input: expect.objectContaining({
          provider: "webdav",
          webdav: expect.objectContaining({
            baseUrl: "https://dav.example.com/dav/box",
            password: null,
          }),
        }),
      }),
    );
  });
});

/** 冲突流主线：远端快照 ≠ 本机基线 → 冲突话术对话框，确认即 force 上传。 */
test("上传冲突预判：冲突话术 + 确认即 force:true", async () => {
  settingsResult = webdavSettings({
    webdav: {
      baseUrl: "https://dav.example.com/dav/box",
      username: "user",
      remoteRoot: "",
      hasPassword: true,
    },
    status: {
      lastSyncAt: 1752000000,
      lastSnapshotId: "local-baseline",
      lastDeviceName: "this-mac",
      lastError: null,
      lastErrorSource: null,
    },
  });
  remoteInfoResult = {
    exists: true,
    deviceName: "other-mac",
    createdAt: 1752100000,
    snapshotId: "remote-newer",
    incompatible: null,
  };
  uploadResult = {
    outcome: { snapshotId: "s", totalBytes: 1, deviceName: "this-mac", createdAt: 1 },
    conflict: null,
  };
  renderSection();

  fireEvent.click(await screen.findByRole("button", { name: /^上传$|^Upload$/ }));

  // 冲突话术：标题 + 对方设备名 + 确认按钮为「强制上传」。
  expect(
    await screen.findByText(/云端已被其他设备更新|Remote updated by another device/),
  ).toBeTruthy();
  expect(screen.getByText(/other-mac/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /强制上传|Force upload/ }));

  await vi.waitFor(() => {
    expect(invokeMock).toHaveBeenCalledWith("sync_upload", { force: true });
  });
});

/** 竞态兜底：预判无冲突但 sync_upload 返回 conflict → 二次弹冲突框。 */
test("上传竞态兜底：引擎冲突返回后再弹冲突框", async () => {
  settingsResult = webdavSettings({
    webdav: {
      baseUrl: "https://dav.example.com/dav/box",
      username: "user",
      remoteRoot: "",
      hasPassword: true,
    },
    status: {
      lastSyncAt: 1752000000,
      lastSnapshotId: "same-snapshot",
      lastDeviceName: "this-mac",
      lastError: null,
      lastErrorSource: null,
    },
  });
  // 远端 = 本机基线 → 走普通覆盖确认（非冲突话术）。
  remoteInfoResult = {
    exists: true,
    deviceName: "this-mac",
    createdAt: 1752000000,
    snapshotId: "same-snapshot",
    incompatible: null,
  };
  // 但对话框开着期间被抢先：引擎检出冲突。
  uploadResult = {
    outcome: null,
    conflict: { deviceName: "sneaky-mac", createdAt: 1752200000, snapshotId: "sneaky" },
  };
  renderSection();

  fireEvent.click(await screen.findByRole("button", { name: /^上传$|^Upload$/ }));
  // 普通覆盖确认框（非冲突话术），确认按钮仍是「上传」。
  expect(await screen.findByText(/本次上传会覆盖它|this upload overwrites it/)).toBeTruthy();
  const dialogButtons = screen.getAllByRole("button", { name: /^上传$|^Upload$/ });
  const confirmButton = dialogButtons[dialogButtons.length - 1];
  if (!confirmButton) throw new Error("对话框确认按钮未找到");
  fireEvent.click(confirmButton);

  await vi.waitFor(() => {
    expect(invokeMock).toHaveBeenCalledWith("sync_upload", { force: false });
  });
  // 竞态返回 conflict → 二次弹出冲突话术框。
  expect(
    await screen.findByText(/云端已被其他设备更新|Remote updated by another device/),
  ).toBeTruthy();
  expect(screen.getByText(/sneaky-mac/)).toBeTruthy();
});

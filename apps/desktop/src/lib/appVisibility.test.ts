import { beforeEach, describe, expect, it } from "vitest";
import { PROVIDER_APPS, readVisibleApps, setVisibleApps } from "./appVisibility";

const V1_KEY = "mcpane:visible-apps";
const V2_KEY = "mcpane:visible-apps-v2";

describe("客户端可见性偏好", () => {
  beforeEach(() => localStorage.clear());

  it("无记录时全部可见——不按安装检测自动隐藏页签", () => {
    const visible = readVisibleApps();
    expect(PROVIDER_APPS.every((app) => visible[app])).toBe(true);
  });

  it("v1 存档一律作废并删除：那一版的值可能是自动探测写的，不是用户选的", () => {
    localStorage.setItem(V1_KEY, JSON.stringify({ gemini: false, "claude-desktop": false }));
    const visible = readVisibleApps();
    expect(visible.gemini).toBe(true);
    expect(visible["claude-desktop"]).toBe(true);
    expect(localStorage.getItem(V1_KEY)).toBeNull();
  });

  it("用户手动设置的偏好按 v2 键往返", () => {
    setVisibleApps({
      claude: true,
      "claude-desktop": false,
      codex: true,
      gemini: false,
      grok: true,
      opencode: true,
      openclaw: true,
      hermes: true,
    });
    expect(localStorage.getItem(V2_KEY)).not.toBeNull();
    const visible = readVisibleApps();
    expect(visible["claude-desktop"]).toBe(false);
    expect(visible.gemini).toBe(false);
    expect(visible.claude).toBe(true);
  });

  it("存档缺字段时按默认补全（将来新增客户端对老用户默认可见）", () => {
    localStorage.setItem(V2_KEY, JSON.stringify({ claude: false }));
    const visible = readVisibleApps();
    expect(visible.claude).toBe(false);
    expect(visible.codex).toBe(true);
  });
});

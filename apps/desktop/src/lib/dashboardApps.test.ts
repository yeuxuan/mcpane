import { beforeEach, describe, expect, it } from "vitest";
import {
  DASHBOARD_APP_CHOICES,
  MAX_DASHBOARD_APPS,
  readDashboardApps,
  resolveDashboardApps,
  setDashboardApps,
} from "./dashboardApps";

const ALL_INSTALLED = Object.fromEntries(DASHBOARD_APP_CHOICES.map((app) => [app, true]));

describe("概览首卡客户端偏好", () => {
  beforeEach(() => localStorage.clear());

  it("additive 客户端不可选", () => {
    expect(DASHBOARD_APP_CHOICES).not.toContain("opencode");
  });

  it("无记录返回 null，默认取 claude + codex", () => {
    expect(readDashboardApps()).toBeNull();
    expect(resolveDashboardApps(null, ALL_INSTALLED)).toEqual(["claude", "codex"]);
  });

  it("默认顺序里没装的跳过，由后续已安装的补位", () => {
    expect(
      resolveDashboardApps(null, { claude: true, gemini: true, "claude-desktop": true }),
    ).toEqual(["claude", "claude-desktop"]);
  });

  it("偏好往返，且超出上限的存档被截断", () => {
    setDashboardApps(["gemini", "claude"]);
    expect(readDashboardApps()).toEqual(["claude", "gemini"]);
    localStorage.setItem("mcpane:dashboard-apps", JSON.stringify(["claude", "codex", "gemini"]));
    expect(readDashboardApps()).toHaveLength(MAX_DASHBOARD_APPS);
  });

  it("存档损坏视为无记录；显式清空则一个都不显示", () => {
    localStorage.setItem("mcpane:dashboard-apps", "{oops");
    expect(readDashboardApps()).toBeNull();
    setDashboardApps([]);
    expect(resolveDashboardApps(readDashboardApps(), ALL_INSTALLED)).toEqual([]);
  });

  it("偏好里没装的客户端不展示", () => {
    expect(resolveDashboardApps(["gemini", "codex"], { codex: true })).toEqual(["codex"]);
  });
});

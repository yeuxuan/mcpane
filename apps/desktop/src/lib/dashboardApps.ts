/**
 * 概览页「当前供应商」首卡显示哪些客户端：最多 {@link MAX_DASHBOARD_APPS} 个。
 *
 * 该卡片只负责「快速切换 + 近 24 小时花费」，上限是为了排版（一行两格）。设备级偏好，
 * 与 appVisibility 一样走 localStorage。
 */
import { CLIENTS, isAdditiveApp } from "./clients";
import type { ClientApp } from "./ipc";

export const MAX_DASHBOARD_APPS = 2;

/** 可上首卡的客户端：additive 客户端没有「当前生效项」，排除。 */
export const DASHBOARD_APP_CHOICES: ClientApp[] = CLIENTS.map((meta) => meta.app).filter(
  (app) => !isAdditiveApp(app),
);

/** 无偏好时的优先顺序：主力的 claude / codex 在前，其余按注册序补位。 */
const DEFAULT_ORDER: ClientApp[] = [
  ...new Set<ClientApp>(["claude", "codex", ...DASHBOARD_APP_CHOICES]),
];

const STORAGE_KEY = "mcpane:dashboard-apps";

/** 已落盘的选择；无记录/存档损坏返回 null（由调用方回落默认）。 */
export function readDashboardApps(): ClientApp[] | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    return DASHBOARD_APP_CHOICES.filter((app) => parsed.includes(app)).slice(0, MAX_DASHBOARD_APPS);
  } catch {
    return null;
  }
}

/** 写偏好。存不下就只本次会话生效。 */
export function setDashboardApps(apps: ClientApp[]) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(apps));
  } catch {
    // 忽略：本次会话内 React 状态仍生效。
  }
}

/** 偏好 → 实际展示：只留已安装的；无偏好时按默认顺序取前两个已安装的。 */
export function resolveDashboardApps(
  preferred: ClientApp[] | null,
  installed: Partial<Record<ClientApp, boolean>>,
): ClientApp[] {
  return (preferred ?? DEFAULT_ORDER).filter((app) => installed[app]).slice(0, MAX_DASHBOARD_APPS);
}

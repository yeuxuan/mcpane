/**
 * 客户端可见性偏好：供应商页显示哪些客户端页签。
 *
 * 对齐 cc-switch 的 `visibleApps`：默认**全部显示**，由用户手动关掉不用的。
 * 刻意不按安装检测自动隐藏——页签无声消失比多两个页签更让人困惑（"我的
 * Gemini 去哪了？"），"先配置后安装"也是合理用法。安装态只用于**标注**：
 * 未检测到的客户端页签标灰，点进去给安装引导（见 ClientInstallNotice）。
 *
 * 设备级偏好，与主题/语言一样走 localStorage（后端 settings 只管数据目录/端口等）。
 */
import { CLIENTS } from "./clients";
import type { ClientApp } from "./ipc";

export type VisibleApps = Record<ClientApp, boolean>;

/** 供应商页可切换的客户端（单一事实源见 lib/clients.ts）。 */
export const PROVIDER_APPS: ClientApp[] = CLIENTS.map((meta) => meta.app);

const STORAGE_KEY = "mcpane:visible-apps-v2";
/**
 * v1 的值可能不是用户选的：曾有一版按安装检测自动写入偏好（没装的客户端直接
 * 关掉），页签就那么无声少了两个。两者在存档里长得一模一样，无法区分，只能
 * 整体作废——换 key 从"全部可见"重新开始，顺手删掉旧键不留残影。
 */
const LEGACY_STORAGE_KEY = "mcpane:visible-apps";
// 默认全部可见——不按安装检测自动隐藏（见上方模块注释）。
const DEFAULT: VisibleApps = Object.fromEntries(
  CLIENTS.map((meta) => [meta.app, true]),
) as VisibleApps;

/** 已落盘的偏好；无记录/不可用时返回 null（由调用方决定默认值）。 */
function storedVisibleApps(): VisibleApps | null {
  try {
    localStorage.removeItem(LEGACY_STORAGE_KEY);
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<VisibleApps>;
    // 与默认合并 ⇒ 将来新增客户端对老用户默认可见。
    return { ...DEFAULT, ...parsed };
  } catch {
    // Safari 隐私模式 / 存档损坏 → 当作无记录，不该让供应商页起不来。
    return null;
  }
}

/** 读偏好；无记录时全部可见。 */
export function readVisibleApps(): VisibleApps {
  return storedVisibleApps() ?? { ...DEFAULT };
}

/** 写偏好。存不下就只本次会话生效，好过直接失败。 */
export function setVisibleApps(value: VisibleApps) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(value));
  } catch {
    // 忽略：本次会话内 React 状态仍生效。
  }
}

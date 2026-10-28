/**
 * 主题：light / dark / system 三态。
 *
 * `data-theme` 上恒为 light|dark 二选一——system 在此解析成具体值再落到 DOM，
 * CSS 因而只需写一份暗色覆盖（见 index.css），不必为 system 再写一遍
 * `@media (prefers-color-scheme)`。
 *
 * 原生标题栏一并同步：内容暗、标题栏亮（用户系统是浅色但选了暗色，或反之）
 * 会割裂成两截。
 */
import { getCurrentWindow } from "@tauri-apps/api/window";

export type ThemePreference = "light" | "dark" | "system";
type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "mcpane:theme";
const DARK_QUERY = "(prefers-color-scheme: dark)";

/** 读偏好；localStorage 不可用或值非法时回落 system。 */
export function readThemePreference(): ThemePreference {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === "light" || raw === "dark" || raw === "system") return raw;
  } catch {
    // Safari 隐私模式等场景会抛——回落默认值即可，不该让整个应用起不来。
  }
  return "system";
}

function resolveTheme(preference: ThemePreference): ResolvedTheme {
  if (preference !== "system") return preference;
  return window.matchMedia?.(DARK_QUERY).matches ? "dark" : "light";
}

function paint(resolved: ResolvedTheme) {
  document.documentElement.dataset.theme = resolved;
  // 同步原生标题栏。失败不影响 CSS 主题，故一律吞掉：
  // - getCurrentWindow() 在非 Tauri 环境（浏览器里跑 vite dev）会同步抛，
  //   Promise 的 catch 接不住，必须 try 包住，否则启动即崩；
  // - setTheme 缺 core:window:allow-set-theme 权限时异步 reject。
  try {
    getCurrentWindow()
      .setTheme(resolved)
      .catch(() => {});
  } catch {
    // 非 Tauri 宿主：只做 CSS 换肤。
  }
}

/** 写偏好并立即生效。 */
export function setThemePreference(preference: ThemePreference) {
  try {
    localStorage.setItem(STORAGE_KEY, preference);
  } catch {
    // 存不下就只在本次会话生效，好过直接失败。
  }
  paint(resolveTheme(preference));
}

/**
 * 启动时调用：应用已存偏好，并在 system 模式下跟随系统切换。
 * 返回取消监听函数。
 */
export function initTheme(): () => void {
  paint(resolveTheme(readThemePreference()));

  const media = window.matchMedia?.(DARK_QUERY);
  if (!media) return () => {};
  const onSystemChange = () => {
    // 只有 system 模式跟随；显式选了 light/dark 的用户不该被系统切换掀翻。
    if (readThemePreference() === "system") paint(resolveTheme("system"));
  };
  media.addEventListener("change", onSystemChange);
  return () => media.removeEventListener("change", onSystemChange);
}

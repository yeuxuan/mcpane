import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { initTheme, readThemePreference, setThemePreference } from "./theme";

const setThemeMock = vi.fn(() => Promise.resolve());

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ setTheme: setThemeMock }),
}));

/** 可控的 matchMedia：systemDark 决定 prefers-color-scheme 结果。 */
let systemDark = false;
let mediaListeners: Array<() => void> = [];

function installMatchMedia() {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: query.includes("dark") && systemDark,
    addEventListener: (_: string, cb: () => void) => {
      mediaListeners.push(cb);
    },
    removeEventListener: (_: string, cb: () => void) => {
      mediaListeners = mediaListeners.filter((l) => l !== cb);
    },
  }));
}

function currentTheme() {
  return document.documentElement.dataset.theme;
}

beforeEach(() => {
  localStorage.clear();
  systemDark = false;
  mediaListeners = [];
  setThemeMock.mockClear();
  document.documentElement.removeAttribute("data-theme");
  installMatchMedia();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

test("默认跟随系统", () => {
  expect(readThemePreference()).toBe("system");
});

test("system 模式按系统解析成具体值落到 DOM", () => {
  systemDark = true;
  initTheme();
  expect(currentTheme()).toBe("dark");
});

test("显式选择覆盖系统偏好", () => {
  systemDark = true;
  setThemePreference("light");
  expect(currentTheme()).toBe("light");
  expect(readThemePreference()).toBe("light");
});

test("偏好持久化，重启后仍生效", () => {
  setThemePreference("dark");
  document.documentElement.removeAttribute("data-theme"); // 模拟重启

  initTheme();
  expect(currentTheme()).toBe("dark");
});

test("system 模式跟随系统切换", () => {
  initTheme();
  expect(currentTheme()).toBe("light");

  systemDark = true;
  for (const notify of mediaListeners) notify();

  expect(currentTheme()).toBe("dark");
});

test("显式选择后不被系统切换掀翻", () => {
  setThemePreference("light");
  initTheme();

  systemDark = true;
  for (const notify of mediaListeners) notify();

  expect(currentTheme()).toBe("light");
});

test("同步原生标题栏，避免内容与标题栏割裂", () => {
  setThemePreference("dark");
  expect(setThemeMock).toHaveBeenCalledWith("dark");
});

test("取消监听后不再跟随系统", () => {
  const stop = initTheme();
  stop();
  systemDark = true;
  for (const notify of mediaListeners) notify();
  expect(currentTheme()).toBe("light");
});

test("localStorage 不可用时回落 system 而非崩溃", () => {
  const getItem = vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
    throw new Error("隐私模式");
  });
  expect(readThemePreference()).toBe("system");
  getItem.mockRestore();
});

test("非法存储值回落 system", () => {
  localStorage.setItem("mcpane:theme", "chartreuse");
  expect(readThemePreference()).toBe("system");
});

import { expect, type Page, test } from "./support";

/**
 * 弹窗打开时焦点困在弹窗内。真机 WKWebView 里 Option+Tab 曾一路走出弹窗、落到背景的
 * 工作区切换器上，按 Space 还会在背景里打开下拉。WKWebView 默认 Tab 只走输入框、
 * Option+Tab 才走全部控件，所以 WebKit 下两种按法都要过一遍（见 playwright.config.ts）。
 */

/** 当前焦点是否在弹窗内，以及它的可读名（aria-label 或文字；落到 body 时直说）。 */
function focusState(page: Page) {
  return page.evaluate(() => {
    const el = document.activeElement as HTMLElement | null;
    const name =
      !el || el === document.body
        ? "<body>"
        : (el.getAttribute("aria-label") ?? el.textContent?.trim().slice(0, 40) ?? "");
    return { inDialog: Boolean(el?.closest('[role="dialog"]')), name };
  });
}

test("弹窗打开时 Tab 不会跑到背景", async ({ page, browserName }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "快速操作" }).click();
  await expect(page.getByRole("dialog", { name: "快速操作" })).toBeVisible();

  const keys = ["Tab", "Shift+Tab"];
  if (browserName === "webkit") keys.push("Alt+Tab", "Alt+Shift+Tab");
  for (const key of keys) {
    const seen = new Set<string>();
    // 命令面板里只有两三个可聚焦控件，按 8 次足够绕好几圈。
    for (let i = 0; i < 8; i++) {
      await page.keyboard.press(key);
      const state = await focusState(page);
      expect(state.inDialog, `${key} 第 ${i + 1} 次落到了弹窗外：${state.name}`).toBe(true);
      seen.add(state.name);
    }
    // 不只是被弹回原处：关闭按钮和搜索框都轮到过，才算真的在弹窗里循环。
    expect([...seen], `${key} 没有在弹窗内循环`).toEqual(
      expect.arrayContaining(["关闭", "快速操作"]),
    );
  }

  await expect(page.getByRole("button", { name: "切换工作区" })).not.toBeFocused();
  await expect(page.getByRole("menu")).toHaveCount(0);
});

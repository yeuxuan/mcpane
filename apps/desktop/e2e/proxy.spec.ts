import { expect, test } from "./support";

/**
 * 出网代理设置（ADR-017）的渲染与交互门面。
 *
 * 非 Tauri 宿主下 IPC 会抛错，故这里只覆盖不依赖后端的部分：分组、模式切换
 * 露出输入框、以及与「外观」那组同名按钮的隔离。真正的读写行为由
 * ProxySettings.test.tsx（mock IPC）与 Rust 侧单测覆盖。
 */

/** 打开设置并切到指定分区（设置页按「通用 / 环境 / 数据 / 关于」分组）。 */
async function openSettings(page: import("@playwright/test").Page, section = "环境") {
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "设置", exact: true })).toBeVisible();
  await page
    .getByRole("group", { name: "设置分区" })
    .getByRole("radio", { name: section, exact: true })
    .click();
}

function proxyGroup(page: import("@playwright/test").Page) {
  return page.getByRole("group", { name: "出网代理" });
}

test("代理设置默认跟随系统，且不显示地址输入框", async ({ page }) => {
  await page.goto("/");
  await openSettings(page);

  await expect(proxyGroup(page)).toBeVisible();
  await expect(page.getByLabel("代理地址")).toHaveCount(0);
});

test("切到自定义后露出地址输入框", async ({ page }) => {
  await page.goto("/");
  await openSettings(page);

  await proxyGroup(page).getByRole("radio", { name: "自定义", exact: true }).click();

  await expect(page.getByLabel("代理地址")).toBeVisible();
});

/**
 * 「外观」与「出网代理」各有一个「跟随系统」。分区之后两者不同屏，但组名隔离
 * 仍是必须的——读屏用户在按钮列表里靠组名分辨这两个同名项。
 */
test("两处同名的「跟随系统」各归其组，互不干扰", async ({ page }) => {
  await page.goto("/");
  await openSettings(page, "环境");
  await expect(proxyGroup(page).getByRole("radio", { name: "跟随系统", exact: true })).toHaveCount(
    1,
  );

  await page
    .getByRole("group", { name: "设置分区" })
    .getByRole("radio", { name: "通用", exact: true })
    .click();
  await expect(
    page.getByRole("group", { name: "外观" }).getByRole("radio", { name: "跟随系统", exact: true }),
  ).toHaveCount(1);
  // 分区切走后代理那组不再在场：同名项不会互相干扰到定位。
  await expect(proxyGroup(page)).toHaveCount(0);
});

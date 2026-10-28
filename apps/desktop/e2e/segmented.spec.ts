import { expect, test } from "./support";

/**
 * 分段选择器（外观 / 语言 / 出网代理三处共用 SegmentedControl）的行为守卫。
 *
 * 这里守的三条都是**静默失效**型——坏掉时 DOM 结构不变、断言文案的测试全绿，
 * 只有用户发现「怎么点不动 / 看不见焦点 / 鼠标划过没反应」。各页的功能测试
 * （theme.spec / proxy.spec）只管选中后的结果，管不到这一层。
 *
 * 三条各自对应组件里一个不显然的取舍，改动时若把它们改坏，这里会红：
 * - 悬停：radio 铺满整个选项，鼠标停在它上面时**兄弟** span 并不 :hover，
 *   必须走 group-hover。写成 hover: 时颜色一动不动，且没有任何别的测试会发现。
 * - 焦点环：radio 是透明的，焦点环必须靠 peer-focus-visible 转画到 span 上。
 * - 方向键：换掉一排 button、改用原生 radio 的**全部理由**就在这条上。
 */

async function openSettings(page: import("@playwright/test").Page) {
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await expect(page.getByRole("heading", { name: "设置", exact: true })).toBeVisible();
}

const appearance = (page: import("@playwright/test").Page) =>
  page.getByRole("group", { name: "外观" });

/** 未选中项的 span 颜色（radio 透明，视觉全在它兄弟 span 上）。 */
const spanColor = (radio: ReturnType<typeof appearance>) =>
  radio.evaluate((el) => getComputedStyle(el.nextElementSibling as HTMLElement).color);

test("悬停未选中项时文字变深", async ({ page }) => {
  await page.goto("/");
  await openSettings(page);
  const dark = appearance(page).getByRole("radio", { name: "深色", exact: true });

  const idle = await spanColor(dark);
  await dark.hover();

  await expect
    .poll(() => spanColor(dark), {
      message: "铺满的 radio 之上，兄弟 span 必须靠 group-hover 变色",
    })
    .not.toBe(idle);
});

test("键盘走到分段选择器时焦点环可见", async ({ page }) => {
  await page.goto("/");
  await openSettings(page);

  // 用真键盘而非 focus()：编程聚焦不触发 :focus-visible（鼠标点击也不该冒焦点环）。
  let landed = false;
  for (let i = 0; i < 60 && !landed; i++) {
    await page.keyboard.press("Tab");
    landed = await page.evaluate(
      () =>
        document.activeElement?.tagName === "INPUT" &&
        document.activeElement?.closest('fieldset[aria-label="外观"]') !== null,
    );
  }
  expect(landed, "Tab 应能把焦点送进「外观」这组").toBe(true);

  const ring = await page.evaluate(() => {
    const el = document.activeElement as HTMLElement;
    return {
      focusVisible: el.matches(":focus-visible"),
      boxShadow: getComputedStyle(el.nextElementSibling as HTMLElement).boxShadow,
    };
  });
  expect(ring.focusVisible).toBe(true);
  expect(ring.boxShadow, "焦点环必须转画到 span 上，否则键盘用户看不见自己在哪").not.toBe("none");
});

/** 方向键切换是原生 radio 白送的；一排 button 无论如何做不到。 */
test("方向键切换选中项并即刻生效", async ({ page }) => {
  await page.goto("/");
  await openSettings(page);
  const group = appearance(page);

  await group.getByRole("radio", { name: "浅色", exact: true }).check();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

  await page.keyboard.press("ArrowRight");

  await expect(group.getByRole("radio", { name: "深色", exact: true })).toBeChecked();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
});

/**
 * 同页三组 radio 必须各有各的 name。共用 name 的话它们会被浏览器当成同一组，
 * 选「深色」会把「出网代理」的选中项一起清掉——而三组的选中态都由 React 托管，
 * 界面上看不出异样，只有 DOM 里的 checked 悄悄丢了。
 */
test("三组分段选择器互不串组", async ({ page }) => {
  await page.goto("/");
  await openSettings(page);

  const names = await page.evaluate(() =>
    [...document.querySelectorAll("fieldset input[type=radio]")].map((el) => [
      (el.closest("fieldset") as HTMLElement)?.getAttribute("aria-label"),
      (el as HTMLInputElement).name,
    ]),
  );
  const byGroup = new Map<string, Set<string>>();
  for (const [group, name] of names) {
    if (!group || !name) continue;
    byGroup.set(group, (byGroup.get(group) ?? new Set()).add(name));
  }
  // 每组内共用一个 name，组与组之间互不相同。
  for (const [group, set] of byGroup) {
    expect(set.size, `「${group}」组内的 radio 应共用同一个 name`).toBe(1);
  }
  const allNames = [...byGroup.values()].map((s) => [...s][0]);
  expect(new Set(allNames).size, "各组的 name 必须互不相同，否则会被当成同一组").toBe(
    allNames.length,
  );
});

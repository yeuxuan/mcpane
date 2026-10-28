/**
 * E2E 公共 fixture：所有 spec 从这里导入 `test`，而不是直接用 @playwright/test。
 *
 * 每个用例都是全新 profile（localStorage 空），导览会压暗整页并吃掉所有点击——那是
 * 给真人看的一次性上手流程，不是被测行为。这里在页面脚本执行前关掉自动触发，让用例
 * 落在稳定的常态界面上。导览自身的行为由 onboarding.spec.ts 覆盖，它直接用
 * @playwright/test 而不走这个 fixture。
 *
 * 关的是 autoStart 开关，而不是把八套逐个标记成已看过：将来加了新的一套，这里不必
 * 跟着改，也就不会出现"漏了一套，某个 spec 突然被遮罩吃掉点击"的连锁失败。
 *
 * 首启向导排在导览之前，同样会盖住整页，所以一并标记成走过（wizard: true）。它
 * 自己的行为由 welcome.spec.ts 覆盖。
 */
import AxeBuilder from "@axe-core/playwright";
import { test as base, expect as baseExpect, type Page } from "@playwright/test";

/** 与 lib/onboarding.ts 的 STORAGE_KEY / SCHEMA 保持一致。 */
const TOURS_KEY = "mcpane:tours-v1";
const AUTO_START_OFF = JSON.stringify({ v: 1, tours: {}, autoStart: false, wizard: true });

export const test = base.extend({
  page: async ({ page }, use) => {
    await page.addInitScript(`
      try { localStorage.setItem(${JSON.stringify(TOURS_KEY)}, ${JSON.stringify(AUTO_START_OFF)}); } catch {}
    `);
    await use(page);
  },
});

export { expect, type Page } from "@playwright/test";

/**
 * axe 扫描 + 断言无 serious/critical 违规。
 *
 * `label` 会拼进失败信息——一条用例里连扫多处（浅色/暗色、几个弹窗）时，
 * 光看 violation.id 分不出是哪一处挂的。
 *
 * `selector` 传 `[role="dialog"]` 可只扫弹窗：弹窗打开时背景内容被蒙层刻意压暗，
 * 那部分的对比度不适用，整页扫会报一堆无意义的违规。
 */
export async function expectNoSevereA11yViolations(
  page: Page,
  label: string,
  selector?: string,
): Promise<void> {
  const builder = new AxeBuilder({ page });
  const results = await (selector ? builder.include(selector) : builder).analyze();
  const severe = results.violations.filter((violation) =>
    ["serious", "critical"].includes(violation.impact ?? ""),
  );
  baseExpect(
    severe.flatMap((violation) =>
      violation.nodes.map((node) => `${label}: ${violation.id} :: ${node.html}`),
    ),
  ).toHaveLength(0);
}

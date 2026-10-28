import { expect, test } from "vitest";
import { formatPrice, MAX_PRICE, parsePrice, scalePrices } from "./priceOverride";

test("parsePrice：合法数字通过，空串 / 非数字 / 负数 / 超上限被拒", () => {
  expect(parsePrice(" 0.6 ")).toBe(0.6);
  expect(parsePrice("0")).toBe(0);
  expect(parsePrice(String(MAX_PRICE))).toBe(MAX_PRICE);
  for (const bad of ["", "  ", "abc", "-1", "1e400", "NaN", "Infinity", String(MAX_PRICE + 1)]) {
    expect(parsePrice(bad), bad).toBeNull();
  }
});

test("scalePrices：按倍率缩放四项并去掉浮点噪声", () => {
  const official = { input: 3, output: 15, cacheRead: 0.3, cacheCreation: 3.75 };
  expect(scalePrices(official, 0.6)).toEqual({
    input: 1.8,
    output: 9,
    cacheRead: 0.18,
    cacheCreation: 2.25,
  });
  expect(scalePrices(official, 0)).toEqual({ input: 0, output: 0, cacheRead: 0, cacheCreation: 0 });
});

test("scalePrices：非法倍率或缩放后超上限返回 null", () => {
  const official = { input: 3, output: 15, cacheRead: 0.3, cacheCreation: 0 };
  expect(scalePrices(official, -1)).toBeNull();
  expect(scalePrices(official, Number.NaN)).toBeNull();
  expect(scalePrices(official, 1e9)).toBeNull();
});

test("formatPrice：不带浮点噪声与多余的 0", () => {
  expect(formatPrice(1.7999999999999998)).toBe("1.8");
  expect(formatPrice(15)).toBe("15");
  expect(formatPrice(0.0000001)).toBe("0");
  expect(formatPrice(0.18)).toBe("0.18");
});

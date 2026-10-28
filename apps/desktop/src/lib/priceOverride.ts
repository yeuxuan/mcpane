import type { ModelPrices } from "./ipc";

/** 与后端 `MAX_PRICE_PER_MILLION` 一致：挡住手滑多打的零。 */
export const MAX_PRICE = 1_000_000;

/** 解析输入框里的价格；空串、非数字、负数、超上限都返回 null。 */
export function parsePrice(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "") return null;
  const value = Number(trimmed);
  return Number.isFinite(value) && value >= 0 && value <= MAX_PRICE ? value : null;
}

/** 最多 6 位小数、去掉末尾的 0（`3 * 0.6` 的浮点噪声不进输入框）。 */
export function formatPrice(value: number): string {
  return String(Number(value.toFixed(6)));
}

/** 官方价按倍率缩放（中转站按官方价几折这类场景）。倍率非法返回 null。 */
export function scalePrices(official: ModelPrices, multiplier: number): ModelPrices | null {
  if (!Number.isFinite(multiplier) || multiplier < 0) return null;
  const scale = (value: number) => Number((value * multiplier).toFixed(6));
  const scaled = {
    input: scale(official.input),
    output: scale(official.output),
    cacheRead: scale(official.cacheRead),
    cacheCreation: scale(official.cacheCreation),
  };
  return Object.values(scaled).every((v) => v <= MAX_PRICE) ? scaled : null;
}

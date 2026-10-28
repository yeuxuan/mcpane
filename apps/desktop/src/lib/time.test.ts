import i18next from "i18next";
import { describe, expect, it } from "vitest";
import "./i18n";

/** 用真实的 t：顺带验证时长文案确实接上了 i18n。 */
const t = (key: string, options?: Record<string, unknown>) => i18next.t(key, options);

import { formatDuration } from "./time";

describe("formatDuration", () => {
  it("秒级", () => {
    expect(formatDuration(0, t)).toBe("0 秒");
    expect(formatDuration(11, t)).toBe("11 秒");
    expect(formatDuration(59.9, t)).toBe("59 秒");
  });

  it("分级：分 + 秒", () => {
    expect(formatDuration(60, t)).toBe("1 分 0 秒");
    expect(formatDuration(312, t)).toBe("5 分 12 秒");
  });

  it("小时级：时 + 分（不再显示秒）", () => {
    expect(formatDuration(3600, t)).toBe("1 小时 0 分");
    expect(formatDuration(5023, t)).toBe("1 小时 23 分");
  });

  it("天级：天 + 时（不再显示分）", () => {
    expect(formatDuration(86400, t)).toBe("1 天 0 小时");
    expect(formatDuration(86400 * 2 + 3600 * 5 + 61, t)).toBe("2 天 5 小时");
  });

  it("负数与非法输入按 0 处理", () => {
    expect(formatDuration(-5, t)).toBe("0 秒");
  });
});

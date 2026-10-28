import { expect, test } from "vitest";
import { formatBalance, quotaKind } from "./quota";

const p = (id: string, baseUrl: string | null) => ({ id, baseUrl });

// 与 Rust `ProviderService::quota` 的分派同规则：前端判错会出现「点了按钮却报不支持」。
test("quotaKind 按 base_url 分两类", () => {
  // 官方订阅一律不查额度（不请求官方接口）。
  expect(quotaKind(p("official-claude", null))).toBeNull();
  expect(quotaKind(p("official-codex", null))).toBeNull();
  expect(quotaKind(p("official-gemini", null))).toBeNull();
  expect(quotaKind(p("k", "https://api.kimi.com/coding/"))).toBe("plan");
  expect(quotaKind(p("z", "https://open.bigmodel.cn/api/anthropic"))).toBe("plan");
  expect(quotaKind(p("m", "https://api.minimaxi.com/anthropic"))).toBe("plan");
  expect(quotaKind(p("d", "https://api.deepseek.com/anthropic"))).toBe("balance");
  expect(quotaKind(p("s", "https://api.siliconflow.cn/v1"))).toBe("balance");
  expect(quotaKind(p("o", "https://openrouter.ai/api/v1"))).toBe("balance");
  expect(quotaKind(p("n", "https://api.novita.ai/openai"))).toBe("balance");
  expect(quotaKind(p("st", "https://api.stepfun.com/v1"))).toBe("balance");
  expect(quotaKind(p("x", "https://relay.example.com"))).toBeNull();
  expect(quotaKind(p("x", null))).toBeNull();
});

test("formatBalance：CNY/USD 用符号，其余带币种", () => {
  expect(formatBalance({ amount: 12.3, currency: "CNY" })).toBe("¥12.30");
  expect(formatBalance({ amount: 3.2, currency: "usd" })).toBe("$3.20");
  expect(formatBalance({ amount: 1, currency: "EUR" })).toBe("1.00 EUR");
});

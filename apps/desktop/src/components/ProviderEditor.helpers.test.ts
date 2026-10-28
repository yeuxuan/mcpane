import { describe, expect, it } from "vitest";
import {
  deepMergeJson,
  deepRemoveMatching,
  diffFragment,
  formatFragment,
  parseFragment,
} from "./ProviderEditor";

describe("配置片段编辑器的基线/差集语义", () => {
  const live = {
    env: { USER_VAR: "keep", API_TIMEOUT_MS: "3000000" },
    permissions: { allow: ["Bash"] },
  };

  it("deepMergeJson：overlay 覆盖同名标量、对象递归合并", () => {
    const merged = deepMergeJson(live, { env: { ENABLE_TOOL_SEARCH: "true" } });
    expect(merged.env).toEqual({
      USER_VAR: "keep",
      API_TIMEOUT_MS: "3000000",
      ENABLE_TOOL_SEARCH: "true",
    });
    expect(merged.permissions).toEqual({ allow: ["Bash"] });
  });

  it("deepRemoveMatching：按值匹配移除并修剪空父级，不动手改值", () => {
    const removed = deepRemoveMatching(live, {
      env: { API_TIMEOUT_MS: "3000000" },
      permissions: { allow: ["Bash"] },
    });
    expect(removed).toEqual({ env: { USER_VAR: "keep" } });
    const kept = deepRemoveMatching(live, { env: { API_TIMEOUT_MS: "999" } });
    expect((kept.env as Record<string, unknown>).API_TIMEOUT_MS).toBe("3000000");
  });

  it("diffFragment：只捕获新增/修改，忽略未动与删除的键", () => {
    const baseline = { env: { USER_VAR: "keep" }, permissions: { allow: ["Bash"] } };
    expect(diffFragment(structuredClone(baseline), baseline)).toBeNull();
    const edited = {
      env: { USER_VAR: "changed", ENABLE_TOOL_SEARCH: "true" },
      permissions: { allow: ["Bash"] },
      attribution: { commit: "", pr: "" },
    };
    expect(diffFragment(edited, baseline)).toEqual({
      env: { USER_VAR: "changed", ENABLE_TOOL_SEARCH: "true" },
      attribution: { commit: "", pr: "" },
    });
    const deleted = { env: { USER_VAR: "keep" } };
    expect(diffFragment(deleted, baseline)).toBeNull();
  });

  it("深遍历跳过 __proto__/constructor/prototype，原型不被改写", () => {
    // JSON.parse 会把 __proto__ 生成为自有键；`result[key]=` 赋值会走 setter
    // 改写副本原型而非落键。三个 walker 必须统一跳过（对齐 cc-switch 硬化）。
    const hostile = JSON.parse(
      '{"__proto__": {"polluted": "yes"}, "constructor": {"x": 1}, "env": {"OK": "1"}}',
    ) as Record<string, unknown>;
    const merged = deepMergeJson({ env: { KEEP: "1" } }, hostile);
    expect(merged.env).toEqual({ KEEP: "1", OK: "1" });
    expect(Object.getPrototypeOf(merged)).toBe(Object.prototype);
    expect(({} as Record<string, unknown>).polluted).toBeUndefined();

    const removed = deepRemoveMatching({ env: { OK: "1" } }, hostile);
    expect(Object.getPrototypeOf(removed)).toBe(Object.prototype);

    const diffed = diffFragment(hostile, {});
    expect(diffed).toEqual({ env: { OK: "1" } });
  });
});

describe("配置片段 <-> 编辑器文本（Claude=JSON / Codex=TOML）", () => {
  it("claude：JSON 格式往返", () => {
    const fragment = { env: { API_TIMEOUT_MS: "3000000" } };
    const text = formatFragment("claude", fragment);
    expect(text).toContain('"API_TIMEOUT_MS"');
    expect(parseFragment("claude", text)).toEqual(fragment);
  });

  it("codex：TOML 格式往返（嵌套表 + 数组）", () => {
    const fragment = {
      model_reasoning_effort: "high",
      notify: ["/bin/notify", "turn-ended"],
      tools: { web_search: true },
    };
    const text = formatFragment("codex", fragment);
    expect(text).toContain('model_reasoning_effort = "high"');
    expect(text).toContain("[tools]");
    expect(parseFragment("codex", text)).toEqual(fragment);
  });

  it("codex：历史 JSON 片段里的 null 叶子被静默剔除，不抛异常", () => {
    expect(formatFragment("codex", { model_reasoning_effort: "high", legacy: null })).toBe(
      'model_reasoning_effort = "high"\n',
    );
    expect(formatFragment("codex", { legacy: null })).toBe("");
  });

  it("空片段显示为空串，空文本解析回 null", () => {
    expect(formatFragment("codex", {})).toBe("");
    expect(formatFragment("claude", undefined)).toBe("");
    expect(parseFragment("codex", "  ")).toBeNull();
    expect(parseFragment("claude", "")).toBeNull();
  });

  it("非法文本抛错（由调用方提示）", () => {
    expect(() => parseFragment("codex", "model = ")).toThrow();
    expect(() => parseFragment("claude", "{oops}")).toThrow();
  });
});

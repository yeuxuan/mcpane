import { describe, expect, it } from "vitest";
import { minimalDiff } from "./JsonEditorView";

/** 应用 diff 后必须还原出新文本——这是最小 diff 的正确性底线。 */
function apply(oldText: string, diff: { from: number; to: number; insert: string }): string {
  return oldText.slice(0, diff.from) + diff.insert + oldText.slice(diff.to);
}

describe("minimalDiff", () => {
  it("相同文本返回 null（不触发编辑器 dispatch）", () => {
    expect(minimalDiff("abc", "abc")).toBeNull();
    expect(minimalDiff("", "")).toBeNull();
  });

  it("尾部追加键（快捷开关典型场景）：只替换尾段", () => {
    const oldText = '{\n  "env": {}\n}';
    const newText = '{\n  "env": {},\n  "attribution": {}\n}';
    const diff = minimalDiff(oldText, newText);
    expect(diff).not.toBeNull();
    expect(apply(oldText, diff as NonNullable<typeof diff>)).toBe(newText);
    // 公共前缀 '{\n  "env": {}' 必须被保留，不允许从 0 开始整段替换。
    expect((diff as NonNullable<typeof diff>).from).toBeGreaterThan(0);
  });

  it("中段删除键（开关关闭场景）：diff 应还原新文本", () => {
    const oldText = '{\n  "a": 1,\n  "b": 2,\n  "c": 3\n}';
    const newText = '{\n  "a": 1,\n  "c": 3\n}';
    const diff = minimalDiff(oldText, newText);
    expect(apply(oldText, diff as NonNullable<typeof diff>)).toBe(newText);
  });

  it("空文档到整份内容：全量替换（from=0, to=旧长度）", () => {
    const diff = minimalDiff("", '{"a":1}');
    expect(diff).toEqual({ from: 0, to: 0, insert: '{"a":1}' });
  });

  it("整份内容清空：insert 为空串", () => {
    const diff = minimalDiff('{"a":1}', "");
    expect(diff).toEqual({ from: 0, to: 7, insert: "" });
  });

  it("公共前后缀重叠（重复字符）不越界", () => {
    // "aaa" -> "aa"：前缀收缩到 2 后，后缀收缩必须停在 start 边界。
    const diff = minimalDiff("aaa", "aa");
    expect(apply("aaa", diff as NonNullable<typeof diff>)).toBe("aa");
  });
});

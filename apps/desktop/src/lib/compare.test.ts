import { describe, expect, it } from "vitest";
import { buildCompareRows, foldCompareRows } from "./compare";

describe("buildCompareRows", () => {
  it("等价文本逐行双侧渲染", () => {
    const rows = buildCompareRows("a\nb\n", "a\nb\n");
    expect(rows).toHaveLength(2);
    expect(rows[0]).toEqual({
      left: { text: "a", changed: false },
      right: { text: "a", changed: false },
    });
  });

  it("纯新增只出现在右栏（首次写入场景：before 为空）", () => {
    const rows = buildCompareRows("", '{\n  "mcpServers": {}\n}\n');
    expect(rows.every((row) => row.left === undefined)).toBe(true);
    expect(rows.map((row) => row.right?.changed)).toEqual([true, true, true]);
  });

  it("替换块左右逐行对齐，短侧留空", () => {
    const rows = buildCompareRows("keep\nold\n", "keep\nnew1\nnew2\n");
    expect(rows[0]?.left?.changed).toBe(false);
    // old → new1 对齐；new2 左侧留空
    expect(rows[1]).toEqual({
      left: { text: "old", changed: true },
      right: { text: "new1", changed: true },
    });
    expect(rows[2]).toEqual({ left: undefined, right: { text: "new2", changed: true } });
  });

  it("纯删除只出现在左栏", () => {
    const rows = buildCompareRows("a\ngone\n", "a\n");
    expect(rows[1]).toEqual({ left: { text: "gone", changed: true } });
  });
});

/** 生成 n 行内容（未变更基底）。 */
const lines = (n: number, prefix = "line") =>
  Array.from({ length: n }, (_, i) => `${prefix}-${i}`).join("\n");

describe("foldCompareRows", () => {
  it("长配置只有尾部变更时：头部未变更区折叠，变更块带上下文展开", () => {
    const before = `${lines(20)}\n`;
    const after = `${before}added-1\nadded-2\n`;
    const segments = foldCompareRows(buildCompareRows(before, after));

    expect(segments[0]?.kind).toBe("fold");
    // 20 行未变更，尾部 3 行作为变更上下文保留 → 折叠 17 行
    expect(segments[0]?.rows).toHaveLength(17);
    const visible = segments[1];
    expect(visible?.kind).toBe("lines");
    // 3 行上下文 + 2 行新增
    expect(visible?.rows).toHaveLength(5);
    expect(visible?.rows.at(-1)?.right?.text).toBe("added-2");
  });

  it("变更在中部时：前后未变更区各自折叠", () => {
    const before = `${lines(10, "head")}\nold\n${lines(10, "tail")}`;
    const after = `${lines(10, "head")}\nnew\n${lines(10, "tail")}`;
    const segments = foldCompareRows(buildCompareRows(before, after));

    expect(segments.map((s) => s.kind)).toEqual(["fold", "lines", "fold"]);
    // 3 上文 + 1 替换 + 3 下文
    expect(segments[1]?.rows).toHaveLength(7);
  });

  it("未变更区不足折叠阈值时并入可见段，不产生碎片", () => {
    const before = "a\nb\nold\nc\nd";
    const after = "a\nb\nnew\nc\nd";
    const segments = foldCompareRows(buildCompareRows(before, after));
    expect(segments).toHaveLength(1);
    expect(segments[0]?.kind).toBe("lines");
    expect(segments[0]?.rows).toHaveLength(5);
  });

  it("段落 start 与行号对应，行数守恒", () => {
    const before = `${lines(30)}\n`;
    const after = `${before}added\n`;
    const rows = buildCompareRows(before, after);
    const segments = foldCompareRows(rows);
    expect(segments.reduce((sum, s) => sum + s.rows.length, 0)).toBe(rows.length);
    expect(segments[0]?.start).toBe(0);
    expect(segments[1]?.start).toBe(segments[0]?.rows.length);
  });
});

import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach } from "vitest";

// jsdom 不做布局，Range 缺 getClientRects/getBoundingClientRect——CodeMirror
// 的光标层测量（TextTile.coordsIn）每次渲染都会在 stderr 抛 TypeError。测试
// 并不因此失败，但成片的堆栈会把真回归淹没在噪声里。补上返回空矩形的实现：
// CodeMirror 对空结果有降级路径，测量拿不到坐标只影响光标绘制，不影响文档操作。
// 带特性检测：jsdom 将来实现了就自动让位。
if (typeof Range !== "undefined" && typeof Range.prototype.getClientRects !== "function") {
  const emptyRect = {
    x: 0,
    y: 0,
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    width: 0,
    height: 0,
    toJSON: () => ({}),
  } as DOMRect;
  Range.prototype.getClientRects = () => {
    const list: DOMRect[] = [];
    return Object.assign(list, { item: () => null }) as unknown as DOMRectList;
  };
  Range.prototype.getBoundingClientRect = () => emptyRect;
}

// vitest globals 关闭时，Testing Library 不会自动注册 cleanup。
afterEach(() => {
  cleanup();
});

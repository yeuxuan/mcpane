/**
 * 前后配置对比的行配对：把 jsdiff 的行级变更整理成左右双栏可渲染的行。
 * 删除/新增相邻时按"替换块"逐行对齐，其余侧留空占位。
 */
import { diffLines } from "diff";

interface CompareCell {
  text: string;
  /** 该行属于变更（左=删除 / 右=新增）。 */
  changed: boolean;
}

export interface CompareRow {
  /** 左栏（写入前）；undefined 表示该行只存在于右侧。 */
  left?: CompareCell;
  /** 右栏（写入后）；undefined 表示该行只存在于左侧。 */
  right?: CompareCell;
}

/** 按行拆分 diff 块文本（块尾换行不产生多余空行）。 */
function splitBlock(value: string): string[] {
  const lines = value.split("\n");
  if (lines.at(-1) === "") lines.pop();
  return lines;
}

/** 折叠后的渲染段：lines 直接渲染，fold 收起为「N 行未变更」可展开条。 */
export type CompareSegment =
  | { kind: "lines"; start: number; rows: CompareRow[] }
  | { kind: "fold"; start: number; rows: CompareRow[] };

/**
 * 把行序列折叠成 diff 视图段：变更行前后保留 `context` 行上下文，
 * 连续未变更超过 `minFold` 行的区域收起——长配置文件里唯一的变更
 * 不再沉底，打开预览即可看到。
 */
export function foldCompareRows(rows: CompareRow[], context = 3, minFold = 5): CompareSegment[] {
  const keep = new Array<boolean>(rows.length).fill(false);
  rows.forEach((row, index) => {
    const changed = !row.left || !row.right || row.left.changed || row.right.changed;
    if (!changed) return;
    const from = Math.max(0, index - context);
    const to = Math.min(rows.length - 1, index + context);
    for (let i = from; i <= to; i += 1) keep[i] = true;
  });

  const segments: CompareSegment[] = [];
  let index = 0;
  while (index < rows.length) {
    const kept = keep[index];
    let end = index;
    while (end < rows.length && keep[end] === kept) end += 1;
    const slice = rows.slice(index, end);
    const last = segments.at(-1);
    if (!kept && slice.length >= minFold) {
      segments.push({ kind: "fold", start: index, rows: slice });
    } else if (last?.kind === "lines") {
      // 不足折叠阈值的未变更段并入相邻可见段，避免碎片
      last.rows.push(...slice);
    } else {
      segments.push({ kind: "lines", start: index, rows: slice });
    }
    index = end;
  }
  return segments;
}

export function buildCompareRows(before: string, after: string): CompareRow[] {
  const changes = diffLines(before, after);
  const rows: CompareRow[] = [];
  let index = 0;

  while (index < changes.length) {
    const change = changes[index];
    if (!change) break;
    const next = changes[index + 1];

    if (change.removed && next?.added) {
      // 替换块：删除与新增逐行对齐，短的一侧留空。
      const removed = splitBlock(change.value);
      const added = splitBlock(next.value);
      const length = Math.max(removed.length, added.length);
      for (let i = 0; i < length; i += 1) {
        const leftLine = removed[i];
        const rightLine = added[i];
        rows.push({
          left: leftLine === undefined ? undefined : { text: leftLine, changed: true },
          right: rightLine === undefined ? undefined : { text: rightLine, changed: true },
        });
      }
      index += 2;
      continue;
    }

    for (const line of splitBlock(change.value)) {
      if (change.removed) {
        rows.push({ left: { text: line, changed: true } });
      } else if (change.added) {
        rows.push({ right: { text: line, changed: true } });
      } else {
        rows.push({
          left: { text: line, changed: false },
          right: { text: line, changed: false },
        });
      }
    }
    index += 1;
  }

  return rows;
}

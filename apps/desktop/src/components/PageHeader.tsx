import type { ReactNode } from "react";

/**
 * 紧凑页眉：标题与操作同处一行（工具式，而非手册式）。
 * 标题文本保持各页原样（E2E/单测按精确 heading 文案断言）。
 */
export function PageHeader({ title, actions }: { title: ReactNode; actions?: ReactNode }) {
  return (
    <div className="mb-5 flex min-h-9 items-center justify-between gap-4">
      <h2 className="min-w-0 truncate text-[19px] font-semibold tracking-tight text-ink">
        {title}
      </h2>
      {actions ? <div className="flex shrink-0 items-center gap-2">{actions}</div> : null}
    </div>
  );
}

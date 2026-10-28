import type * as React from "react";

import { cn } from "@/lib/utils";

/**
 * 填充式输入框（改自 shadcn 默认的描边式）。
 *
 * 默认那版是 `border-input` 描边 + 透明底：一屏四五个字段并排时，边框会把白纸
 * 切成一格一格的网格，正是"整页发灰"的来源。这里改成白纸上的浅色凹槽——
 * 未聚焦时靠底色差存在感，聚焦时底色翻成纸白 + 主色描边 + 光环，从"槽"变成
 * "正在写的那一行"，状态差异比原来的"灰框变蓝框"大得多。
 *
 * 高度从 h-10 收到 h-9，与 ui.tsx 的 Field / Select / SearchInput 对齐——
 * 两套高度混用是接入 shadcn 时遗留的不一致。
 */
function Input({ className, type, ...props }: React.ComponentProps<"input">) {
  return (
    <input
      type={type}
      data-slot="input"
      className={cn(
        "h-9 w-full min-w-0 rounded-lg border border-transparent bg-slot px-3 text-[13px] text-ink outline-none",
        "transition-[background-color,border-color,box-shadow] selection:bg-primary selection:text-primary-foreground",
        "file:inline-flex file:h-7 file:border-0 file:bg-transparent file:font-medium file:text-foreground file:text-sm",
        "placeholder:text-faint disabled:pointer-events-none disabled:cursor-not-allowed disabled:opacity-50",
        "focus-visible:border-accent focus-visible:bg-card focus-visible:ring-[3px] focus-visible:ring-accent/18",
        "aria-invalid:border-destructive aria-invalid:ring-[3px] aria-invalid:ring-destructive/20",
        className,
      )}
      {...props}
    />
  );
}

export { Input };

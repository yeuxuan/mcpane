import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** shadcn/ui 约定的 className 合并工具。 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * 把 mutation / Tauri invoke 抛出的错误归一化为可展示文案。
 * Tauri 命令 reject 的多是 Rust 侧 Err 的字符串；也兜底 Error 对象与其它值。
 */
export function errorText(error: unknown): string {
  if (typeof error === "string") return error;
  if (error instanceof Error) return error.message;
  // 目前所有 Tauri 命令都 reject 字符串；万一将来有命令抛结构化对象，
  // `String(obj)` 会得到 "[object Object]"，序列化一下至少还能看出是什么。
  if (error && typeof error === "object") {
    try {
      return JSON.stringify(error);
    } catch {
      /* 含循环引用等——落到下面的 String() */
    }
  }
  return String(error);
}

/**
 * 连接 ID（conn_ + 32 位 uuid）直接铺在卡片上会撑破布局。中间省略保留头尾：
 * 头部足以辨识，尾部避免同前缀混淆；dev 引导那类可读短 ID 原样返回。
 */
export function shortConnectionId(id: string): string {
  return id.length > 20 ? `${id.slice(0, 11)}…${id.slice(-4)}` : id;
}

import { invoke } from "@tauri-apps/api/core";

/**
 * 前端错误落盘（生产环境 webview 控制台无人接收，crash.log 是唯一线索）。
 *
 * 频控在此侧完成：同一错误文本只上报一次、单次会话上限 20 条，
 * 防错误风暴（渲染循环抛错每帧一次）刷爆 IPC 与日志。
 * 上报自身失败静默——日志通道不该制造新错误。
 */
const seen = new Set<string>();
const MAX_REPORTS_PER_SESSION = 20;
let reported = 0;

export function reportFrontendError(kind: string, message: string, stack?: string): void {
  const text = stack ? `${kind}: ${message}\n${stack}` : `${kind}: ${message}`;
  const key = text.slice(0, 300);
  if (seen.has(key) || reported >= MAX_REPORTS_PER_SESSION) return;
  seen.add(key);
  reported += 1;
  // try/catch 兜同步抛错（非 Tauri 环境 invoke 直接 throw）：本函数运行在
  // error 事件监听器里，自己抛错会再触发一次 error 事件。
  try {
    invoke("log_frontend_error", { message: text }).catch(() => {});
  } catch {
    // 日志通道不可用：静默。
  }
}

/** 挂载全局兜底监听。应在 render 之前调用，越早覆盖面越大。 */
export function installGlobalErrorLogging(): void {
  window.addEventListener("error", (event) => {
    const err = event.error as Error | undefined;
    reportFrontendError(
      "window.onerror",
      event.message || String(err ?? "unknown"),
      err?.stack ?? `${event.filename ?? "?"}:${event.lineno ?? 0}:${event.colno ?? 0}`,
    );
  });
  window.addEventListener("unhandledrejection", (event) => {
    const reason: unknown = event.reason;
    const err = reason instanceof Error ? reason : undefined;
    reportFrontendError(
      "unhandledrejection",
      err?.message ?? (typeof reason === "string" ? reason : JSON.stringify(reason ?? null)),
      err?.stack,
    );
  });
}

import { Component, type ErrorInfo, type ReactNode } from "react";
import { reportFrontendError } from "../lib/frontendLog";

interface ErrorBoundaryProps {
  children: ReactNode;
}

interface ErrorBoundaryState {
  error: Error | null;
}

/**
 * 应用顶层错误边界：渲染异常兜底为可操作的说明页而非白屏。
 *
 * 刻意不用 ui.tsx / i18n：它们本身可能就是崩溃源，fallback 必须零依赖。
 * 文案中英并列，不走翻译层。
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    reportFrontendError(
      "ErrorBoundary",
      error.message,
      `${error.stack ?? ""}\ncomponent stack:${info.componentStack ?? ""}`,
    );
  }

  render(): ReactNode {
    const { error } = this.state;
    if (!error) return this.props.children;
    const detail = `${error.message}\n${error.stack ?? ""}`;
    return (
      <div className="flex min-h-screen flex-col items-center justify-center gap-4 bg-app p-8 text-ink">
        <p className="font-mono text-lg">❯ 界面渲染出错 / Rendering crashed</p>
        <p className="max-w-md text-center text-sm text-muted">
          错误现场已写入 crash.log。可复制下方详情反馈，或重新加载界面。
          <br />
          Details were written to crash.log. Copy them for a bug report, or reload.
        </p>
        <pre className="max-h-64 max-w-2xl overflow-auto rounded-md border border-line bg-card p-3 text-xs">
          {detail}
        </pre>
        <div className="flex gap-3">
          <button
            type="button"
            className="rounded-md border border-line px-4 py-1.5 text-sm hover:bg-card"
            onClick={() => {
              navigator.clipboard?.writeText(detail).catch(() => {});
            }}
          >
            复制详情 / Copy
          </button>
          <button
            type="button"
            className="rounded-md border border-line px-4 py-1.5 text-sm hover:bg-card"
            onClick={() => window.location.reload()}
          >
            重新加载 / Reload
          </button>
        </div>
      </div>
    );
  }
}

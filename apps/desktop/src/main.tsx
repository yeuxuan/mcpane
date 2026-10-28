import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import "./index.css";
import "./lib/i18n";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { installGlobalErrorLogging } from "./lib/frontendLog";
import { QUERY_STALE_MS } from "./lib/liveQuery";
import { initTheme } from "./lib/theme";

// 先装错误兜底再做任何事：init 阶段的异常同样要落 crash.log。
installGlobalErrorLogging();
// index.html 的内联脚本已在首帧前定好 data-theme，这里接管 system 跟随与原生标题栏。
initTheme();

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchIntervalInBackground: false,
      // 几乎每个查询都显式 retry:false（IPC/本地库出错重试没意义，还会拖住
      // 陈旧缓存丢弃逻辑）。默认就设成 false，新查询忘了写也不会偷偷重试一次。
      retry: false,
      staleTime: QUERY_STALE_MS,
    },
  },
});

const rootElement = document.getElementById("root");
if (!rootElement) {
  throw new Error("MCPane: #root element not found");
}

createRoot(rootElement).render(
  <StrictMode>
    <ErrorBoundary>
      <QueryClientProvider client={queryClient}>
        <App />
      </QueryClientProvider>
    </ErrorBoundary>
  </StrictMode>,
);

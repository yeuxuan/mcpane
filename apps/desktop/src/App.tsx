import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense, useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CommandPalette } from "./components/CommandPalette";
import { InitRecovery } from "./components/InitRecovery";
import { OnboardingTour } from "./components/OnboardingTour";
import { MAIN_NAV, type PageKey, Shell } from "./components/Shell";
import { WelcomeWizard } from "./components/WelcomeWizard";
import { ipc } from "./lib/ipc";
import { detectPlatform } from "./lib/platform";
import { TourProvider, useTour } from "./lib/tourContext";
import { WorkspaceProvider } from "./lib/workspace";
import { Clients } from "./pages/Clients";
import { Dashboard } from "./pages/Dashboard";
import { Logs } from "./pages/Logs";
import { Projects } from "./pages/Projects";
import { Providers } from "./pages/Providers";
import { Servers } from "./pages/Servers";
import { Settings } from "./pages/Settings";
import { Workspaces } from "./pages/Workspaces";

/**
 * 用量页按需加载：它是 recharts 的唯一引用点，而图表库比这一页自身的代码大一个量级。
 * 其余页面都是轻的，静态引入即可——拆得越碎，切页时的等待反而越多。
 */
const Usage = lazy(() => import("./pages/Usage").then((m) => ({ default: m.Usage })));

const PAGE_STORAGE_KEY = "mcpane.page";

const PAGE_KEYS: readonly PageKey[] = [
  "dashboard",
  "clients",
  "providers",
  "mcp-servers",
  "projects",
  "workspaces",
  "usage",
  "logs",
  "settings",
];

/** 轻量模式销毁重建 WebView 后恢复上次所在页面；非法值回退概览页。 */
function loadInitialPage(): PageKey {
  const stored = localStorage.getItem(PAGE_STORAGE_KEY);
  return PAGE_KEYS.includes(stored as PageKey) ? (stored as PageKey) : "dashboard";
}

/**
 * 导览覆盖层。挂在外壳之外：它要压住整屏，还要能驱动 setPage 把人带到别的页面。
 *
 * `key={active.id}` 不能省——它让每套导览拿到全新的组件实例。共用实例的话，连着开两套
 * 时（走完总览接着进某一页），第二套会沿用第一套算出来的起始下标，从中间开始讲。
 */
function TourOverlay({ onNavigate }: { onNavigate: (page: PageKey) => void }) {
  const { active, step, close, wizardOpen, finishWizard } = useTour();
  // 向导先于导览：它开着的时候导览一律不弹（触发侧也拦了一道）。
  if (wizardOpen) return <WelcomeWizard onNavigate={onNavigate} onDone={finishWizard} />;
  if (!active) return null;
  return (
    <OnboardingTour
      key={active.id}
      tourId={active.id}
      onNavigate={onNavigate}
      onStep={step}
      onClose={close}
    />
  );
}

function AppShell() {
  const { t } = useTranslation();
  const [page, setPageState] = useState<PageKey>(loadInitialPage);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const setPage = useCallback((next: PageKey) => {
    localStorage.setItem(PAGE_STORAGE_KEY, next);
    setPageState(next);
  }, []);

  // 全局快捷键：⌘K/Ctrl+K 开关命令面板，⌘1…⌘7/Ctrl+1…7 直跳 MAIN_NAV 对应页。
  // 已有别的弹窗（role=dialog[aria-modal=true]）时一律不响应——那多半是编辑表单，
  // 贸然弹出面板或切页会把未保存的改动晾在背后；命令面板自己那层弹窗除外，
  // 否则 ⌘K 永远只能打开、关不掉。
  useEffect(() => {
    const isMac = detectPlatform() === "macos";
    const onKeyDown = (event: KeyboardEvent) => {
      const mod = isMac ? event.metaKey : event.ctrlKey;
      if (!mod) return;
      const anyDialogOpen = document.querySelector('[role="dialog"][aria-modal="true"]') !== null;
      if (event.key.toLowerCase() === "k") {
        if (anyDialogOpen && !paletteOpen) return;
        event.preventDefault();
        setPaletteOpen((open) => !open);
        return;
      }
      if (anyDialogOpen) return;
      const index = Number(event.key) - 1;
      const target = Number.isInteger(index) ? MAIN_NAV[index] : undefined;
      if (target) {
        event.preventDefault();
        setPage(target);
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [paletteOpen, setPage]);

  return (
    <TourProvider page={page}>
      <Shell page={page} onNavigate={setPage} onOpenPalette={() => setPaletteOpen(true)}>
        {page === "dashboard" ? <Dashboard onNavigate={setPage} /> : null}
        {page === "clients" ? <Clients /> : null}
        {page === "providers" ? <Providers /> : null}
        {page === "mcp-servers" ? <Servers onNavigate={setPage} /> : null}
        {page === "projects" ? <Projects onNavigate={setPage} /> : null}
        {page === "workspaces" ? <Workspaces /> : null}
        {page === "usage" ? (
          <Suspense
            fallback={<p className="py-8 text-center text-xs text-faint">{t("usage.loading")}</p>}
          >
            <Usage />
          </Suspense>
        ) : null}
        {page === "logs" ? <Logs /> : null}
        {page === "settings" ? <Settings /> : null}
      </Shell>
      <TourOverlay onNavigate={setPage} />
      {paletteOpen ? (
        <CommandPalette onNavigate={setPage} onClose={() => setPaletteOpen(false)} />
      ) : null}
    </TourProvider>
  );
}

export function App() {
  // 首帧已提交 → 让后端把启动时隐藏的主窗口亮出来（见 window::on_app_ready）。
  // 非 Tauri 宿主（浏览器跑 vite dev）invoke 会 reject，吞掉即可。
  useEffect(() => {
    ipc.windowReady().catch(() => {});
  }, []);
  // init 门控：db 打不开时所有页面都没意义，全屏渲染恢复页而非让
  // 每条命令各自裸报错。探针失败（IPC 本身挂了）按正常放行——
  // 那是另一类故障，恢复页帮不上忙。
  const init = useQuery({
    queryKey: ["init-status"],
    queryFn: ipc.initStatus,
    staleTime: Number.POSITIVE_INFINITY,
    retry: false,
  });
  if (init.data) {
    return <InitRecovery error={init.data} onRetry={() => init.refetch()} />;
  }
  return (
    <WorkspaceProvider>
      <AppShell />
    </WorkspaceProvider>
  );
}

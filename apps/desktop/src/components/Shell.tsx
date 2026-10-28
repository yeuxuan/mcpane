import { useQuery } from "@tanstack/react-query";
import {
  ChartColumn,
  Command,
  FolderKanban,
  KeyRound,
  LayoutDashboard,
  type LucideIcon,
  MonitorSmartphone,
  ScrollText,
  Server,
  Settings,
} from "lucide-react";
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { ipc } from "../lib/ipc";
import { detectPlatform } from "../lib/platform";
import { queryKeys } from "../lib/queryKeys";
import { cn } from "../lib/utils";
import { ExternalChangeBanner } from "./ExternalChangeBanner";
import { ImportLinkDialog } from "./ImportLinkDialog";
import { TrayNoticeBanner } from "./TrayNoticeBanner";
import { UpdateBanner } from "./UpdateBanner";
import { BrandMark } from "./ui";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";

export type PageKey =
  | "dashboard"
  | "clients"
  | "providers"
  | "mcp-servers"
  | "projects"
  | "workspaces"
  | "usage"
  | "logs"
  | "settings";

/** 可作侧栏项的键：工作区经顶右切换器进入，不占侧栏，故排除。 */
type NavKey = Exclude<PageKey, "workspaces">;

/** 侧栏主导航（连接并入客户端、历史并入日志；设置钉在底部单列）。
 * 命令面板的「页面」分组与 ⌘1…⌘7 快捷键都直接复用这份顺序，故导出。 */
export const MAIN_NAV: NavKey[] = [
  "dashboard",
  "mcp-servers",
  "providers",
  "clients",
  "projects",
  "usage",
  "logs",
];

export const NAV_ICON: Record<NavKey, LucideIcon> = {
  dashboard: LayoutDashboard,
  "mcp-servers": Server,
  providers: KeyRound,
  clients: MonitorSmartphone,
  projects: FolderKanban,
  usage: ChartColumn,
  logs: ScrollText,
  settings: Settings,
};

/** 模块级定义：在 Shell 函数体内定义会让每次渲染产生新组件类型，
 * React 按类型不同整棵卸载重挂导航子树（Shell 随 hub-status 轮询高频渲染）。 */
function NavButton({
  pageKey,
  active,
  onNavigate,
}: {
  pageKey: NavKey;
  active: boolean;
  onNavigate: (page: PageKey) => void;
}) {
  const { t } = useTranslation();
  const Icon = NAV_ICON[pageKey];
  return (
    <button
      type="button"
      onClick={() => onNavigate(pageKey)}
      aria-current={active ? "page" : undefined}
      // 首启导览的聚光灯靠它定位（见 OnboardingTour 的 TOUR_STOPS）。
      data-tour={`nav-${pageKey}`}
      className={cn(
        "flex w-full items-center gap-2.5 rounded-lg px-2.5 py-[7px] text-[13px] transition-colors",
        active
          ? "bg-card font-medium text-ink shadow-[var(--shadow-card)]"
          : "text-muted hover:bg-hover hover:text-ink",
      )}
    >
      {/* 选中项图标取主色；其余用 muted 而非 faint——faint 一整列会让侧栏发灰发虚。 */}
      <Icon
        size={17}
        strokeWidth={2}
        aria-hidden
        className={active ? "text-accent" : "text-muted"}
      />
      {t(`nav.${pageKey}`)}
    </button>
  );
}

export function Shell({
  page,
  onNavigate,
  onOpenPalette,
  children,
}: {
  page: PageKey;
  onNavigate: (page: PageKey) => void;
  /** 打开命令面板（⌘K）；侧栏顶部放一个不起眼的入口，主要靠快捷键。 */
  onOpenPalette: () => void;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const isMac = detectPlatform() === "macos";
  const hub = useQuery({
    queryKey: queryKeys.hubStatus,
    queryFn: ipc.hubStatus,
    refetchInterval: 5000,
    retry: false,
    // Shell 只关心版本号与连接数：剔除 uptimeSecs 等每轮必变字段，避免 5s 一次的无效重渲染。
    select: (data) => ({
      version: data.serverVersion,
      active: data.connections.filter((c) => c.enabled).length,
      total: data.connections.length,
    }),
  });
  const live = hub.isSuccess;

  return (
    <div className="flex h-screen flex-col bg-app text-ink">
      {/* 顶栏：品牌 + 常驻工作区切换器 */}
      <header className="flex h-14 shrink-0 items-center gap-3 border-b border-line bg-chrome px-4">
        <div className="flex items-center gap-2.5">
          <span
            aria-hidden
            className="flex h-8 w-8 items-center justify-center rounded-[10px] bg-[#3a3a41] text-white shadow-sm"
          >
            {/* 与应用图标同款：终端提示符 + 四格面板（右上格绿色），空态复用同一标记 */}
            <BrandMark size={18} />
          </span>
          <h1 className="text-[15px] font-semibold tracking-tight">MCPane</h1>
        </div>
        <div className="flex-1" />
        <WorkspaceSwitcher onManage={() => onNavigate("workspaces")} />
      </header>

      {/* 配置被外部改动的提醒：与当前页面无关，故挂在外壳层 */}
      <ExternalChangeBanner />
      {/* 托盘操作失败/警告：托盘无反馈通道，挂外壳层可见 */}
      <TrayNoticeBanner />
      {/* 启动静默检查发现新版本的提醒（同为全局事件，挂外壳层） */}
      <UpdateBanner />
      {/* mcpane:// 深链导入：链接可能在任意页面到达，确认框挂外壳层 */}
      <ImportLinkDialog />

      <div className="flex min-h-0 flex-1">
        <aside className="flex w-[220px] shrink-0 flex-col border-r border-line bg-chrome">
          {/* 命令面板入口：低调放在导航最上方，主要靠快捷键，按钮只是让人发现它存在。 */}
          <div className="px-2.5 pt-2.5">
            <button
              type="button"
              onClick={onOpenPalette}
              className="flex w-full items-center gap-2.5 rounded-lg px-2.5 py-[7px] text-[13px] text-muted transition-colors hover:bg-hover hover:text-ink"
            >
              <Command size={17} strokeWidth={2} aria-hidden className="text-muted" />
              <span className="flex-1 text-left">{t("palette.openButton")}</span>
              <span className="text-[11px] text-muted">{isMac ? "⌘K" : "Ctrl+K"}</span>
            </button>
          </div>

          {/* 主导航 */}
          <nav className="flex-1 overflow-y-auto px-2.5 py-3" aria-label={t("shell.mainNav")}>
            <div data-tour="nav-list" className="space-y-0.5">
              {MAIN_NAV.map((key) => (
                <NavButton key={key} pageKey={key} active={page === key} onNavigate={onNavigate} />
              ))}
            </div>
          </nav>

          {/* 设置 */}
          <div className="px-2.5 pb-1">
            <NavButton pageKey="settings" active={page === "settings"} onNavigate={onNavigate} />
          </div>

          {/* Hub 状态 */}
          <div className="border-t border-line p-2.5">
            <div
              data-tour="hub-status"
              className="flex items-center gap-2.5 rounded-lg px-2.5 py-2"
            >
              {/* 圆点只是文字状态的视觉重复，语义交给紧邻的那行文本承担：
                  它本来挂着 aria-label="hub-up"，屏幕阅读器会先念一句键名，
                  再念一遍「MCP Hub v…」。role="status" 移到文本上，播报的
                  内容才是人话，Hub 起落时也照样会播。 */}
              <span
                aria-hidden
                className={cn(
                  "relative flex h-2.5 w-2.5 shrink-0 items-center justify-center rounded-full",
                  live ? "bg-run-dot" : "bg-faint",
                )}
              >
                {live ? (
                  <span className="absolute h-2.5 w-2.5 animate-ping rounded-full bg-run-dot opacity-60" />
                ) : null}
              </span>
              <div className="min-w-0">
                <p role="status" className="text-xs font-medium text-ink">
                  {live ? t("shell.hubRunning", { version: hub.data.version }) : t("shell.hubDown")}
                </p>
                {live ? (
                  <p className="truncate text-[11px] text-muted">
                    {t("shell.hubConnections", { active: hub.data.active, total: hub.data.total })}
                  </p>
                ) : null}
              </div>
            </div>
          </div>
        </aside>

        {/* relative 必须有：页内的绝对定位后代（如每行结果列的 sr-only 文本）没有
            定位祖先时会落到初始包含块（视口）上——它们不随 main 滚动，却照样计入
            html 的 scrollHeight，日志这种长表会把窗口撑出一条几千像素的滚动条，
            整个外壳（含侧栏）都能被滚走。 */}
        <main className="relative flex-1 overflow-y-auto">
          {/* key 随页切换重挂载，触发 page-fade-in 淡入。 */}
          <div key={page} className="page-fade-in mx-auto max-w-[1320px] px-8 py-6">
            {children}
          </div>
        </main>
      </div>
    </div>
  );
}

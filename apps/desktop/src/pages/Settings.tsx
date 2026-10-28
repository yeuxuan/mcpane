import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import type { LucideIcon } from "lucide-react";
import {
  CircleDollarSign,
  Compass,
  Database,
  Download,
  Gauge,
  Globe,
  HardDrive,
  Info,
  Languages,
  LayoutGrid,
  Monitor,
  Moon,
  Power,
  RefreshCw,
  Server,
  Sun,
  Waypoints,
} from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { BrandGlyph, SurfaceBadge } from "../components/BrandIcon";
import { CcSwitchImportPanel } from "../components/CcSwitchImport";
import { ConfigDirSection } from "../components/ConfigDirSection";
import { BackupSection, CloudSyncSection, RetentionSection } from "../components/DataSection";
import { LocalEnvSection } from "../components/LocalEnvSection";
import { PageHeader } from "../components/PageHeader";
import { ProxySettings } from "../components/ProxySettings";
import { Button, Card, IconChip, SegmentedControl, StatusBadge, Switch } from "../components/ui";
import {
  PROVIDER_APPS,
  readVisibleApps,
  setVisibleApps,
  type VisibleApps,
} from "../lib/appVisibility";
import { CLIENT_META } from "../lib/clients";
import {
  DASHBOARD_APP_CHOICES,
  MAX_DASHBOARD_APPS,
  readDashboardApps,
  resolveDashboardApps,
  setDashboardApps,
} from "../lib/dashboardApps";
import { formatAt } from "../lib/datetime";
import { setLanguage } from "../lib/i18n";
import { type ClientApp, type HubProcessState, ipc, type UpdateStatus } from "../lib/ipc";
import { QUERY_STALE_MS } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { readThemePreference, setThemePreference } from "../lib/theme";
import { useTour } from "../lib/tourContext";
import { cn, errorText } from "../lib/utils";

/** 设置分区（顺序即展示序）。 */
const SETTINGS_SECTIONS = ["general", "env", "data", "about"] as const;
type SettingsSection = (typeof SETTINGS_SECTIONS)[number];
const SECTION_KEY = "mcpane:settings-section";

function loadSection(): SettingsSection {
  const stored = localStorage.getItem(SECTION_KEY);
  return SETTINGS_SECTIONS.find((id) => id === stored) ?? "general";
}

/** supervisor 状态 → 实时状态徽章：启停过程的每次迁移（启动中/重试/失败/关闭）都可见。 */
function hubStateBadge(
  state: HubProcessState | undefined,
  t: (key: string, options?: Record<string, unknown>) => string,
): { tone: "run" | "stop" | "danger" | "warn"; text: string } | null {
  switch (state?.state) {
    case "managed":
    case "disowned":
      return { tone: "run", text: t("settings.hubBadge.running") };
    case "probing":
    case "starting":
      return { tone: "warn", text: t("settings.hubBadge.starting") };
    case "backoff":
      return { tone: "warn", text: t("settings.hubBadge.backoff", { attempt: state.attempt }) };
    case "failed":
      return {
        tone: "danger",
        text: t(
          state.reason === "portOccupied"
            ? "settings.hubBadge.portOccupied"
            : "settings.hubBadge.repeatedCrash",
        ),
      };
    case "disabled":
      return { tone: "stop", text: t("settings.hubBadge.disabled") };
    case "shutDown":
      return { tone: "stop", text: t("settings.hubBadge.shutDown") };
    default:
      // 状态未知（查询未返回 / 非 Tauri 环境）：不显示徽章，也绝不转圈。
      return null;
  }
}

function InfoRow({
  icon,
  tone,
  label,
  value,
  mono,
  hint,
}: {
  icon: LucideIcon;
  tone: "blue" | "slate" | "green" | "sky" | "teal" | "amber";
  label: string;
  value: string;
  mono?: boolean;
  hint?: string;
}) {
  return (
    <div className="flex items-center gap-3 py-3">
      <IconChip icon={icon} tone={tone} size={34} />
      <span className="flex-1 text-sm text-ink" title={hint}>
        {label}
      </span>
      <span className={cn("truncate text-right text-sm text-muted", mono && "font-mono text-xs")}>
        {value}
      </span>
    </div>
  );
}

/** 设置（TASK-506 部分：数据目录 / 端口 / 日志保留）。 */
export function Settings() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  // 偏好的事实源是 localStorage；这里只镜像一份用于渲染选中态。
  const [theme, setTheme] = useState(readThemePreference);
  // 记住上次看的分区：设置里的事往往要来回改几趟，每次都从头找很烦。
  const [section, setSection] = useState<SettingsSection>(loadSection);
  const selectSection = (next: SettingsSection) => {
    setSection(next);
    try {
      localStorage.setItem(SECTION_KEY, next);
    } catch {
      // 存不下就只本次会话生效。
    }
  };
  const [visibleApps, setVisibleAppsState] = useState<VisibleApps>(readVisibleApps);
  const visibleCount = PROVIDER_APPS.filter((app) => visibleApps[app]).length;
  const toggleAppVisible = (app: ClientApp, next: boolean) => {
    // 不允许关掉最后一个：至少留一个客户端，否则供应商页整页空白。
    if (!next && visibleCount === 1) return;
    const updated = { ...visibleApps, [app]: next };
    setVisibleAppsState(updated);
    setVisibleApps(updated);
  };
  // 安装态只用于标注"未检测到"——解释这些客户端为什么默认关着，不强制开关。
  const installed = useQuery({
    queryKey: queryKeys.clientsInstalled,
    queryFn: ipc.providerClientsInstalled,
    retry: false,
    staleTime: 5 * 60 * 1000,
  });
  // 概览首卡的客户端（至多两个）：无记录时呈现默认解析结果，用户一动才落盘。
  const [dashboardChoice, setDashboardChoice] = useState<ClientApp[] | null>(readDashboardApps);
  const dashboardApps = dashboardChoice ?? resolveDashboardApps(null, installed.data ?? {});
  const toggleDashboardApp = (app: ClientApp, next: boolean) => {
    const updated = next
      ? [...dashboardApps, app].slice(0, MAX_DASHBOARD_APPS)
      : dashboardApps.filter((a) => a !== app);
    setDashboardChoice(updated);
    setDashboardApps(updated);
  };
  const info = useQuery({
    queryKey: queryKeys.appInfo,
    queryFn: ipc.appInfo,
    retry: false,
  });
  // 协议版本与 Hub 版本是 Hub 进程报的，只有「关于」分区要用。
  const hubStatus = useQuery({
    queryKey: queryKeys.hubStatus,
    queryFn: ipc.hubStatus,
    enabled: section === "about",
    retry: false,
  });
  /** Hub 报的常量：拿不到就说拿不到，但请求还在飞的时候不能先扣一顶「未运行」的帽子。 */
  const hubConstant = (value: string | undefined) =>
    value ?? (hubStatus.isFetching ? "…" : t("settings.hubOffline"));
  const hubEnabled = useQuery({
    queryKey: queryKeys.hubEnabled,
    queryFn: ipc.hubEnabledGet,
    retry: false,
  });
  // 用户本次点击的目标状态：null = 没有进行中的切换。转圈只跟它走——
  // 页面加载、应用启动期的后台拉起都不该锁开关，那些由状态徽章呈现。
  const [pendingTarget, setPendingTarget] = useState<boolean | null>(null);
  // 偏好写库后 supervisor 异步拉起/杀掉子进程（set_enabled 只 store 一个原子量 + notify，
  // 不等真正 spawn/探活完成就返回）：mutation 的 isPending 只覆盖那次 invoke 往返，
  // 真正的启停耗时必须靠这个状态查询确认。切换中/过渡态提速轮询。
  const hubProcessState = useQuery({
    queryKey: queryKeys.hubProcessState,
    queryFn: ipc.hubProcessState,
    refetchInterval: (query) => {
      const s = query.state.data?.state;
      const transitional = s === "probing" || s === "starting" || s === "backoff";
      return transitional || pendingTarget !== null ? 500 : 5000;
    },
    staleTime: QUERY_STALE_MS,
    retry: false,
  });
  // supervisor 每次状态迁移都会 emit——监听后立即刷新，状态徽章与转圈都即时跟进，
  // 不用等下一个轮询周期。
  useEffect(() => {
    const unlisten = listen("hub-process-state", () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.hubProcessState });
      queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus });
    }).catch(() => () => {});
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [queryClient]);
  // Hub 是全应用总闸，开不起来/关不掉必须给出原因——此前 onError 只收转圈，
  // 开关自己弹回原位，用户既不知道失败了、也无从判断该去看什么。
  // 处理方式对齐同页的 autostart（下方 setAutostart）。
  const [hubToggleError, setHubToggleError] = useState<string | null>(null);
  const setHubEnabled = useMutation({
    mutationFn: (enabled: boolean) => ipc.hubSetEnabled(enabled),
    onMutate: (enabled) => {
      setHubToggleError(null);
      setPendingTarget(enabled);
    },
    onError: (error) => {
      setPendingTarget(null);
      setHubToggleError(t("settings.hubToggleFailed", { error: errorText(error) }));
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.hubEnabled });
      queryClient.invalidateQueries({ queryKey: queryKeys.hubProcessState });
      queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus });
    },
  });
  // 只有 supervisor 真正到达与目标一致的终态才收起转圈：
  // 开 → managed/disowned（探活确认拉起成功）或 failed（明确失败，交由徽章呈现）；
  // 关 → disabled/shutDown（子进程确认已被杀掉）。中间的 probing/starting/backoff 继续转。
  const processState = hubProcessState.data?.state;
  useEffect(() => {
    if (pendingTarget === null || !processState) return;
    const reached = pendingTarget
      ? processState === "managed" || processState === "disowned" || processState === "failed"
      : processState === "disabled" || processState === "shutDown";
    if (reached) setPendingTarget(null);
  }, [pendingTarget, processState]);
  const hubToggling = setHubEnabled.isPending || pendingTarget !== null;
  const hubBadge = hubStateBadge(hubProcessState.data, t);
  // 开机自启：事实源是操作系统（LaunchAgent / 注册表 / .desktop），不入库。
  // 用户在系统设置里关掉后这里要如实显示"关"，所以每次都问系统而不是问库。
  const autostart = useQuery({
    queryKey: queryKeys.autostart,
    queryFn: ipc.autostartGet,
    retry: false,
  });
  // 写系统自启动项可能被安全策略挡下（企业管控的注册表、只读的 LaunchAgents），
  // 命令返回的是写完实测的状态——与请求不符就照实说，不能让开关空欢喜。
  const [autostartError, setAutostartError] = useState<string | null>(null);
  const setAutostart = useMutation({
    mutationFn: ipc.autostartSet,
    onMutate: () => setAutostartError(null),
    onSuccess: (actual, requested) => {
      queryClient.setQueryData(queryKeys.autostart, actual);
      if (actual !== requested) setAutostartError(t("settings.autostartRejected"));
    },
    onError: (error) => {
      const text = errorText(error);
      // 与 Rust `UNPACKAGED_BUILD_ERROR` 对齐。
      setAutostartError(
        text === "unpackaged-build"
          ? t("settings.autostartDevBuild")
          : t("settings.autostartFailed", { error: text }),
      );
    },
  });
  // 应用更新：检查 → 下载安装 → 重启生效；自动检查开关入库。
  const [updateStatus, setUpdateStatus] = useState<UpdateStatus | null>(null);
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [updateInstalled, setUpdateInstalled] = useState(false);
  const checkUpdate = useMutation({
    mutationFn: ipc.updateCheck,
    onMutate: () => setUpdateError(null),
    onSuccess: setUpdateStatus,
    onError: (error) => setUpdateError(t("update.checkFailed", { error: errorText(error) })),
  });
  const installUpdate = useMutation({
    mutationFn: ipc.updateInstall,
    onMutate: () => setUpdateError(null),
    onSuccess: () => setUpdateInstalled(true),
    onError: (error) => setUpdateError(t("update.installFailedWith", { error: errorText(error) })),
  });
  const pricingSync = useQuery({
    queryKey: queryKeys.pricingSync,
    queryFn: ipc.pricingSyncStatus,
  });
  // invoke 本身抛错时 onSuccess 不触发，卡片文案（读的是 lastError）保持旧值——
  // 转圈停下就没了下文。同步的成败必须自己报，不能借状态查询的嘴。
  const [pricingError, setPricingError] = useState<string | null>(null);
  const setPricingSyncEnabled = useMutation({
    mutationFn: ipc.pricingSyncSetEnabled,
    onMutate: () => setPricingError(null),
    onSuccess: (state) => queryClient.setQueryData(queryKeys.pricingSync, state),
    onError: (error) =>
      setPricingError(t("settings.pricingSyncFailed", { error: errorText(error) })),
  });
  const runPricingSync = useMutation({
    mutationFn: ipc.pricingSyncRun,
    onMutate: () => setPricingError(null),
    onSuccess: (state) => queryClient.setQueryData(queryKeys.pricingSync, state),
    onError: (error) =>
      setPricingError(t("settings.pricingSyncFailed", { error: errorText(error) })),
  });

  const autoUpdate = useQuery({
    queryKey: queryKeys.autoUpdateCheck,
    queryFn: ipc.autoUpdateCheckGet,
    retry: false,
  });
  const setAutoUpdate = useMutation({
    mutationFn: ipc.autoUpdateCheckSet,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: queryKeys.autoUpdateCheck }),
  });
  const updateStatusLine = updateError
    ? updateError
    : updateInstalled
      ? t("update.restartToApply")
      : updateStatus
        ? updateStatus.available
          ? t("update.availableVersion", { version: updateStatus.version ?? "" })
          : t("update.upToDate")
        : null;

  return (
    <div>
      <PageHeader title={t("nav.settings")} />

      {/* 分区（对齐 cc-switch 的设置页分栏）：单列平铺到十几张卡片时，
          找一项要滚半页；按"在改什么"分四组，进来先选组再看内容。 */}
      {/* 锚点包在 inline-flex 上而不是外层块级 div：后者是全宽的，高亮框会横跨
          整个内容区，看着像在指一整行而不是那四个页签。 */}
      <div className="mb-4">
        <div data-tour="settings-sections" className="inline-flex">
          <SegmentedControl
            label={t("settings.section.label")}
            value={section}
            options={SETTINGS_SECTIONS.map((id) => ({
              value: id,
              label: t(`settings.section.${id}`),
            }))}
            onChange={selectSection}
          />
        </div>
      </div>

      {section === "general" ? (
        <>
          <Card className="mb-5 flex items-center gap-3 p-5">
            <IconChip icon={Languages} tone="slate" />
            <div className="flex-1">
              <p className="text-sm font-medium text-ink">{t("settings.language")}</p>
              <p className="text-xs text-muted">{t("settings.languageDesc")}</p>
            </div>
            {/* 语言名用各自的语言写（endonym）：把「简体中文」译成 "Simplified Chinese"
            反而让中文用户找不到自己那项。这也是 i18n 守卫要跳过这一组的原因。 */}
            <SegmentedControl
              label={t("settings.language")}
              value={i18n.language === "en" ? "en" : "zh-CN"}
              options={[
                { value: "zh-CN", label: "简体中文" },
                { value: "en", label: "English" },
              ]}
              onChange={setLanguage}
            />
          </Card>

          <Card className="mb-5 flex items-center gap-3 p-5">
            <IconChip
              icon={theme === "dark" ? Moon : theme === "light" ? Sun : Monitor}
              tone="slate"
            />
            <div className="flex-1">
              <p className="text-sm font-medium text-ink">{t("settings.theme")}</p>
              <p className="text-xs text-muted">{t("settings.themeDesc")}</p>
            </div>
            <SegmentedControl
              label={t("settings.theme")}
              value={theme}
              options={(["light", "dark", "system"] as const).map((option) => ({
                value: option,
                label: t(`settings.theme.${option}`),
              }))}
              onChange={(next) => {
                setThemePreference(next);
                setTheme(next);
              }}
            />
          </Card>

          <Card className="mb-5 flex items-center gap-3 p-5">
            <IconChip icon={Power} tone={autostart.data ? "green" : "slate"} />
            <div className="flex-1">
              <p className="text-sm font-medium text-ink">{t("settings.autostart")}</p>
              <p className="text-xs text-muted">
                {autostartError ??
                  (autostart.isError
                    ? t("settings.autostartUnavailable")
                    : t("settings.autostartDesc"))}
              </p>
            </div>
            <Switch
              checked={autostart.data ?? false}
              // 读不到系统状态时不装作"关着"能点：标灰 + 上面那行说明为什么。
              disabled={autostart.isError || autostart.isLoading}
              loading={setAutostart.isPending}
              label={t(autostart.data ? "settings.autostartTurnOff" : "settings.autostartTurnOn")}
              onChange={(next) => setAutostart.mutate(next)}
            />
          </Card>

          <Card className="mb-5 p-5">
            <div className="flex items-center gap-3">
              <IconChip icon={LayoutGrid} tone="slate" />
              <div className="flex-1">
                <p className="text-sm font-medium text-ink">{t("settings.appVisibility.title")}</p>
                <p className="text-xs text-muted">{t("settings.appVisibility.desc")}</p>
              </div>
            </div>
            <div className="mt-3 space-y-0.5">
              {PROVIDER_APPS.map((app) => {
                const isLastVisible = visibleApps[app] && visibleCount === 1;
                return (
                  <div
                    key={app}
                    className="flex items-center gap-3 rounded-lg py-1.5"
                    title={isLastVisible ? t("settings.appVisibility.lastHint") : undefined}
                  >
                    <span
                      aria-hidden
                      className="relative flex h-7 w-7 shrink-0 items-center justify-center"
                      style={{ color: CLIENT_META[app].color }}
                    >
                      <BrandGlyph name={app} size={17} />
                      {/* Claude Code 与 Claude Desktop 在这张表里上下相邻、logo 相同。 */}
                      <SurfaceBadge app={app} className="right-0 bottom-0 border-card bg-hover" />
                    </span>
                    <span className="flex-1 text-[13px] text-ink">
                      {CLIENT_META[app].label}
                      {installed.data && !installed.data[app] ? (
                        <span className="ml-2 text-[11px] text-faint">
                          {t("settings.appVisibility.notDetected")}
                        </span>
                      ) : null}
                    </span>
                    <Switch
                      label={CLIENT_META[app].label}
                      checked={visibleApps[app]}
                      disabled={isLastVisible}
                      onChange={(next) => toggleAppVisible(app, next)}
                    />
                  </div>
                );
              })}
            </div>
          </Card>

          <Card className="mb-5 p-5">
            <div className="flex items-center gap-3">
              <IconChip icon={Gauge} tone="slate" />
              <div className="flex-1">
                <p className="text-sm font-medium text-ink">{t("settings.dashboardApps.title")}</p>
                <p className="text-xs text-muted">
                  {t("settings.dashboardApps.desc", { max: MAX_DASHBOARD_APPS })}
                </p>
              </div>
            </div>
            <div className="mt-3 space-y-0.5">
              {DASHBOARD_APP_CHOICES.map((app) => {
                const checked = dashboardApps.includes(app);
                const full = !checked && dashboardApps.length >= MAX_DASHBOARD_APPS;
                return (
                  <div
                    key={app}
                    className="flex items-center gap-3 rounded-lg py-1.5"
                    title={full ? t("settings.dashboardApps.fullHint") : undefined}
                  >
                    <span
                      aria-hidden
                      className="relative flex h-7 w-7 shrink-0 items-center justify-center"
                      style={{ color: CLIENT_META[app].color }}
                    >
                      <BrandGlyph name={app} size={17} />
                      <SurfaceBadge app={app} className="right-0 bottom-0 border-card bg-hover" />
                    </span>
                    <span className="flex-1 text-[13px] text-ink">{CLIENT_META[app].label}</span>
                    <Switch
                      label={t("settings.dashboardApps.toggle", { app: CLIENT_META[app].label })}
                      checked={checked}
                      disabled={full}
                      onChange={(next) => toggleDashboardApp(app, next)}
                    />
                  </div>
                );
              })}
            </div>
          </Card>

          <ReplayGuideCard />
        </>
      ) : null}

      {section === "env" ? (
        <>
          <Card className="mb-5 flex items-center gap-3 p-5">
            <IconChip icon={Waypoints} tone={hubBadge?.tone === "run" ? "green" : "slate"} />
            <div className="flex-1">
              <div className="flex items-center gap-2">
                <p className="text-sm font-medium text-ink">MCP Hub</p>
                {hubBadge ? <StatusBadge tone={hubBadge.tone}>{hubBadge.text}</StatusBadge> : null}
              </div>
              <p className="text-xs text-muted">{t("settings.hubToggleDesc")}</p>
              {hubToggleError ? <p className="mt-1 text-xs text-danger">{hubToggleError}</p> : null}
            </div>
            <Switch
              checked={hubEnabled.data ?? true}
              onChange={(next) => setHubEnabled.mutate(next)}
              label={t(hubEnabled.data ? "settings.hubTurnOff" : "settings.hubTurnOn")}
              disabled={hubEnabled.isLoading}
              loading={hubToggling}
            />
          </Card>

          {/* 本地环境紧跟应用更新：两者都是"这台机器上装了什么、是不是最新"，
          且它是排障第一站，不该沉在语言/外观后面。 */}
          <LocalEnvSection />

          <ConfigDirSection />

          <ProxySettings />
        </>
      ) : null}

      {/* 数据分区读作一条线：数据攒在哪（本机）→ 留一份（备份）→ 搬到别处（云同步）。
          清理/备份/同步此前各是一张同等重量的卡，看不出它们是同一件事的三个阶段。 */}
      {section === "data" ? (
        <>
          <RetentionSection />

          <Card className="mb-5 flex items-center gap-3 p-5">
            <IconChip
              icon={CircleDollarSign}
              // 出错才着色；正常时中性，与其它分区图标一致（错误详情另有内联文案）。
              tone={pricingError || pricingSync.data?.lastError ? "rose" : "slate"}
            />
            <div className="flex-1">
              <p className="text-sm font-medium text-ink">{t("settings.pricingSync")}</p>
              {pricingError ? (
                <p className="text-xs text-danger">{pricingError}</p>
              ) : (
                <p className="text-xs text-muted">
                  {pricingSync.data?.lastError ??
                    (pricingSync.data?.lastSyncedAt
                      ? t("settings.pricingSyncLast", {
                          time: formatAt(i18n.language, "time", pricingSync.data.lastSyncedAt),
                          count: pricingSync.data.lastWritten ?? 0,
                        })
                      : t("settings.pricingSyncDesc"))}
                </p>
              )}
            </div>
            <Button loading={runPricingSync.isPending} onClick={() => runPricingSync.mutate()}>
              {t("settings.pricingSyncNow")}
            </Button>
            <Switch
              checked={pricingSync.data?.enabled ?? true}
              loading={setPricingSyncEnabled.isPending || pricingSync.isLoading}
              label={t("settings.pricingSync")}
              onChange={(next) => setPricingSyncEnabled.mutate(next)}
            />
          </Card>

          <Card className="mb-5 p-5">
            <div className="flex items-center gap-3">
              <IconChip icon={Download} tone="slate" />
              <div className="flex-1">
                <p className="text-sm font-medium text-ink">{t("settings.ccswitch.title")}</p>
                <p className="text-xs text-muted">{t("settings.ccswitch.desc")}</p>
              </div>
            </div>
            <div className="mt-3">
              <CcSwitchImportPanel />
            </div>
          </Card>

          <BackupSection />

          <CloudSyncSection />
        </>
      ) : null}

      {section === "about" ? (
        <>
          <Card className="mb-5 px-5 py-1">
            <div className="divide-y divide-line">
              <InfoRow
                icon={Info}
                tone="slate"
                label={t("settings.appVersion")}
                value={info.data?.appVersion ?? "-"}
              />
              <InfoRow
                icon={Waypoints}
                tone="slate"
                label={t("settings.protocolVersion")}
                hint={t("settings.protocolVersionHint")}
                value={hubConstant(
                  hubStatus.data &&
                    `MCP ${hubStatus.data.protocolVersion} · ${t("settings.protocolCompatible", {
                      version: hubStatus.data.legacyProtocolVersion,
                    })}`,
                )}
              />
              <InfoRow
                icon={Server}
                tone="slate"
                label={t("settings.hubVersion")}
                value={hubConstant(hubStatus.data && `v${hubStatus.data.serverVersion}`)}
              />
              <InfoRow
                icon={HardDrive}
                tone="slate"
                label={t("settings.dataDir")}
                value={info.data?.dataDir ?? "-"}
                mono
              />
              <InfoRow
                icon={Database}
                tone="slate"
                label={t("settings.database")}
                value={info.data?.dbPath ?? "-"}
                mono
              />
              <InfoRow
                icon={Globe}
                tone="slate"
                label={t("settings.hubEndpoint")}
                value={info.data?.hubEndpoint ?? "-"}
                mono
              />
            </div>
          </Card>

          <Card className="mb-5 flex items-center gap-3 p-5">
            <IconChip icon={RefreshCw} tone="slate" />
            <div className="flex-1">
              <p className="text-sm font-medium text-ink">{t("update.title")}</p>
              <p className="text-xs text-muted">{updateStatusLine ?? t("update.desc")}</p>
            </div>
            <span className="flex items-center gap-2 text-xs text-muted">
              {t("update.autoCheck")}
              <Switch
                checked={autoUpdate.data ?? true}
                loading={setAutoUpdate.isPending || autoUpdate.isLoading}
                label={t("update.autoCheck")}
                onChange={(next) => setAutoUpdate.mutate(next)}
              />
            </span>
            {updateInstalled ? (
              <Button variant="primary" onClick={() => ipc.updateRestart()}>
                {t("update.restart")}
              </Button>
            ) : updateStatus?.available ? (
              <Button
                variant="primary"
                loading={installUpdate.isPending}
                onClick={() => installUpdate.mutate()}
              >
                {t("update.install")}
              </Button>
            ) : (
              <Button loading={checkUpdate.isPending} onClick={() => checkUpdate.mutate()}>
                {t("update.checkNow")}
              </Button>
            )}
          </Card>
        </>
      ) : null}
    </div>
  );
}

/**
 * 重新运行引导。整个界面上**只有这一处**能重放——页头不放、也不做九套导览的
 * 列表管理：那会把"顺手带一程"变成一个要用户去配置的功能（这是 b7c8498 撤掉
 * 全部入口时的判断，仍然成立）。这里补的只是它缺的那一样：想再看一遍时有路可走。
 */
function ReplayGuideCard() {
  const { t } = useTranslation();
  const { restart } = useTour();
  return (
    <Card className="mb-5 flex items-center gap-3 p-5">
      <IconChip icon={Compass} tone="slate" />
      <div className="flex-1">
        <p className="text-sm font-medium text-ink">{t("settings.replayGuide")}</p>
        <p className="text-xs text-muted">{t("settings.replayGuideDesc")}</p>
      </div>
      <Button onClick={restart}>{t("settings.replayGuideAction")}</Button>
    </Card>
  );
}

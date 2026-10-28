import { useQuery, useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { Check, ChevronRight, Copy, Globe, PlugZap, Terminal, Waypoints } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ClientChip } from "../components/ClientChip";
import { CurrentProvidersCard } from "../components/CurrentProvidersCard";
import { PageHeader } from "../components/PageHeader";
import { QueryState } from "../components/QueryState";
import type { PageKey } from "../components/Shell";
import { Button, Card, IconChip, StatusBadge, Tag } from "../components/ui";
import { ipc } from "../lib/ipc";
import { livePoll, QUERY_STALE_MS } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { formatDuration } from "../lib/time";
import { cn } from "../lib/utils";
import { cardStatus, diagnosticsView } from "./Servers";

/**
 * 运行时长实时显示：以轮询到达时刻为锚点本地外推，每秒递增，
 * 不再随 5s 轮询整段跳变，也不显示原始秒数。
 */
function LiveUptime({ uptimeSecs, anchorMs }: { uptimeSecs: number; anchorMs: number }) {
  const { t } = useTranslation();
  const [nowMs, setNowMs] = useState(() => Date.now());
  useEffect(() => {
    // 窗口退到托盘（document.hidden）时不再触发重渲染，恢复可见后下一跳自动追平。
    const id = setInterval(() => {
      if (!document.hidden) setNowMs(Date.now());
    }, 1000);
    return () => clearInterval(id);
  }, []);
  const floorRef = useRef(0);
  let secs = uptimeSecs + Math.max(0, Math.floor((nowMs - anchorMs) / 1000));
  // 轮询刷新锚点可能让外推值回退 1-2 秒：小幅回退按单调钳住；大幅回退是 Hub 重启，如实显示。
  if (secs < floorRef.current && floorRef.current - secs <= 3) secs = floorRef.current;
  floorRef.current = secs;
  return <>{t("dashboard.uptimeInline", { value: formatDuration(secs, t) })}</>;
}

/**
 * 端点铭牌 —— 这张卡的主角。
 *
 * 概览卡上唯一需要被「拿走」的信息就是这个地址：配任何客户端都要把它粘过去。
 * 它独占一块凹槽、等宽大字，且整块即复制按钮（点哪都行，不必瞄准角落那个小图标）。
 * 协议/版本这类查阅一次的常量不在这里，归设置 → 关于。
 *
 * 复制逻辑没有复用 [[CopyButton]]：那是个 button，嵌进这块 button 里就是嵌套
 * 按钮（HTML 非法，键盘 Tab 也会多出一站）。整块可点的收益大于省这十行。
 */
function EndpointPlate({ endpoint }: { endpoint: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const resetTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(resetTimer.current), []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(endpoint);
      setCopied(true);
      clearTimeout(resetTimer.current);
      resetTimer.current = setTimeout(() => setCopied(false), 1500);
    } catch {
      /* 剪贴板不可用时静默——同 CopyButton 惯例，复制非关键路径 */
    }
  };

  return (
    <button
      type="button"
      onClick={copy}
      className="group flex w-full items-center gap-3 rounded-lg bg-slot px-3.5 py-3 text-left transition-colors hover:bg-hover"
    >
      <span className="min-w-0 flex-1">
        {/* 凹槽底上不能用 faint：#6e6e78 落在 --color-slot 上只有 4.47:1，11px 小字差 AA 一线。 */}
        <span className="block text-[11px] font-medium tracking-[0.03em] text-muted">
          {t("dashboard.endpointEyebrow")}
        </span>
        <span className="mt-1 block truncate font-mono text-[15px] font-medium text-ink">
          {endpoint}
        </span>
      </span>
      <span
        role="status"
        aria-live="polite"
        className={cn(
          "flex shrink-0 items-center gap-1.5 text-xs transition-colors",
          copied ? "text-run" : "text-muted group-hover:text-ink",
        )}
      >
        {copied ? (
          <Check size={14} strokeWidth={2.5} aria-hidden />
        ) : (
          <Copy size={14} strokeWidth={2} aria-hidden />
        )}
        {copied ? t("ui.copied") : t("ui.copy")}
      </span>
    </button>
  );
}

/** 铭牌的离线形态：结构不变以免整卡跳动，但不给地址——复制一个连不上的端点只会配错客户端。 */
function EndpointPlateOffline({ hint, action }: { hint: string; action?: React.ReactNode }) {
  const { t } = useTranslation();
  return (
    <div className="flex w-full items-center gap-3 rounded-lg bg-slot px-3.5 py-3">
      <PlugZap size={16} strokeWidth={2} aria-hidden className="shrink-0 text-muted" />
      <span className="min-w-0 flex-1">
        {/* 凹槽底上不能用 faint：#6e6e78 落在 --color-slot 上只有 4.47:1，11px 小字差 AA 一线。 */}
        <span className="block text-[11px] font-medium tracking-[0.03em] text-muted">
          {t("dashboard.endpointOffline")}
        </span>
        <span className="mt-1 block truncate text-[13px] text-muted">{hint}</span>
      </span>
      {action}
    </div>
  );
}

/**
 * 实况轨上的一站：`51 工具已聚合` 这样的一句话，数字与说明同行。
 *
 * 三个 26px 大数字并排是上一版的样子：字号最大、却是全卡最不需要精读的信息
 * （下游与客户端的明细就在下面两张卡里）。压回一行后，它们才让位给端点。
 */
function RailStat({
  value,
  label,
  accent,
  muted,
}: {
  value: React.ReactNode;
  label: string;
  /** 中枢产出（聚合工具数）用主色：轨的两端是别人家的东西，中间这一站才是 MCPane 做的事。 */
  accent?: boolean;
  /** Hub 不可达时读数是占位破折号——按 faint 收，别让三个黑破折号成为全卡最重的一行。 */
  muted?: boolean;
}) {
  return (
    <span className="flex shrink-0 items-baseline gap-1.5">
      <span
        className={cn(
          "font-semibold text-[15px] tabular-nums",
          muted ? "text-faint" : accent ? "text-accent" : "text-ink",
        )}
      >
        {value}
      </span>
      <span className={cn("text-xs", muted ? "text-faint" : "text-muted")}>{label}</span>
    </span>
  );
}

/** 站与站之间的流向：一段发丝线收进箭头。线随剩余宽度伸缩，故整条轨横贯卡片。 */
function RailFlow() {
  return (
    <span aria-hidden className="flex min-w-4 flex-1 items-center text-line-strong">
      <span className="h-px flex-1 bg-current" />
      <ChevronRight size={11} strokeWidth={3} className="-ml-px shrink-0" />
    </span>
  );
}

export function Dashboard({ onNavigate }: { onNavigate?: (page: PageKey) => void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const clients = useQuery({
    queryKey: queryKeys.clients,
    queryFn: ipc.detectClients,
    retry: false,
  });
  const hub = useQuery({
    queryKey: queryKeys.hubStatus,
    queryFn: ipc.hubStatus,
    // 断连时提速轮询：sidecar 拉起只需 1-2s，5s 间隔会让启动期"未连接"停留过久。
    refetchInterval: (query) => (query.state.status === "error" ? 1000 : 5000),
    staleTime: QUERY_STALE_MS,
    retry: false,
  });
  // 仅在 Hub 未连接时轮询托管进程状态，用于区分"正在启动"与真实故障。
  const hubProcess = useQuery({
    queryKey: queryKeys.hubProcessState,
    queryFn: ipc.hubProcessState,
    refetchInterval: 1000,
    staleTime: QUERY_STALE_MS,
    retry: false,
    enabled: hub.isError,
  });
  // supervisor 每次状态迁移都会 emit——监听后立即刷新，启动瞬间无需等轮询周期。
  useEffect(() => {
    const unlisten = listen("hub-process-state", () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus });
      queryClient.invalidateQueries({ queryKey: queryKeys.hubProcessState });
    }).catch(() => () => {});
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [queryClient]);
  const diagnostics = useQuery({
    queryKey: queryKeys.hubDiagnostics,
    queryFn: ipc.hubDiagnostics,
    ...livePoll(10000),
    enabled: hub.isSuccess,
  });
  const servers = useQuery({
    queryKey: queryKeys.servers,
    queryFn: ipc.serversList,
    retry: false,
  });
  const mcpClients = useQuery({
    queryKey: queryKeys.mcpClients,
    queryFn: ipc.mcpClientsStatus,
    retry: false,
  });

  const live = hub.isSuccess;
  const processState = hubProcess.data;
  const offlineHint =
    processState?.state === "disabled"
      ? t("dashboard.offline.disabled")
      : processState?.state === "failed"
        ? processState.reason === "portOccupied"
          ? // 探测已经查出占用者是谁（lsof+ps），报出来省得用户自己去查一趟。
            processState.occupant
            ? t("dashboard.offline.portOccupiedBy", {
                name: processState.occupant.name,
                pid: processState.occupant.pid,
              })
            : t("dashboard.offline.portOccupied")
          : t("dashboard.offline.repeatedCrash")
        : t("dashboard.offline.starting");
  const activeConns = hub.data?.connections.filter((c) => c.enabled).length ?? 0;
  const totalConns = hub.data?.connections.length ?? 0;
  const catalogTools = Number(diagnostics.data?.catalogTools ?? 0);
  // Hub 不可达时丢弃陈旧诊断缓存（与服务器页同口径）：诊断查询被 hub.isSuccess
  // 门控，Hub 挂掉后它停跑、isError 不会翻真，只能拿 live 当闸——否则卡片与
  // 就绪计数会拿最后一次成功的 status 一直报绿。
  const downstreams = live ? diagnosticsView(diagnostics).downstreams : undefined;
  const enabledServers = (servers.data ?? []).filter((s) => s.enabled);
  const readyServers = enabledServers.filter((s) => downstreams?.[s.id]?.status === "ready").length;

  const SHOWN = 6;
  const shownServers = enabledServers.slice(0, SHOWN);

  // 与客户端页同一口径：Claude Code / Codex 走专用探测，其余来自 mcp_clients_status。
  // 只列已安装的，已连接的排前面。
  const clientRows = [
    ...(clients.data
      ? [
          { id: "claude", title: "Claude Code", ...clients.data.claude },
          { id: "codex", title: "Codex", ...clients.data.codex },
        ]
      : []),
    ...(mcpClients.data ?? []).map((c) => ({ ...c, title: c.display })),
  ]
    .filter((c) => c.installed)
    .sort((a, b) => Number(b.connected) - Number(a.connected));
  const shownClients = clientRows.slice(0, SHOWN);

  return (
    <div>
      <PageHeader title={t("nav.dashboard")} />

      <CurrentProvidersCard />

      {/* Hub 状态：身份 → 接入地址（主角）→ 实况轨 */}
      <Card data-tour="dashboard-cards" className="mb-4 p-5">
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
          <div className="flex items-center gap-3.5">
            <IconChip icon={Waypoints} tone={live ? "green" : "slate"} size={36} />
            <div>
              <div className="flex items-center gap-2">
                <p className="text-base font-semibold text-ink">{t("dashboard.hubName")}</p>
                <StatusBadge tone={live ? "run" : "stop"}>
                  {live
                    ? t("dashboard.hub.running")
                    : processState?.state === "disabled"
                      ? t("dashboard.hub.disabled")
                      : processState?.state === "failed"
                        ? t("dashboard.hub.disconnected")
                        : t("dashboard.hub.starting")}
                </StatusBadge>
              </div>
              <p className="mt-0.5 text-xs text-muted">{t("dashboard.hubDesc")}</p>
            </div>
          </div>
          {live ? (
            <span className="text-xs text-muted tabular-nums">
              <LiveUptime uptimeSecs={hub.data.uptimeSecs} anchorMs={hub.dataUpdatedAt} />
            </span>
          ) : null}
        </div>

        {/* 接入地址：整块可点即复制。离线时换成不带地址的提示，结构不变。 */}
        <div className="mt-4">
          {live ? (
            <EndpointPlate endpoint={hub.data.endpoint} />
          ) : (
            <EndpointPlateOffline
              hint={offlineHint}
              // 只有「在设置里关掉了」这一种离线有一键出路；端口被占、反复崩溃都没有，
              // 给个跳转按钮只会让人白跑一趟。
              action={
                processState?.state === "disabled" && onNavigate ? (
                  <Button size="sm" onClick={() => onNavigate("settings")}>
                    {t("dashboard.offline.openSettings")}
                  </Button>
                ) : null
              }
            />
          )}
        </div>

        {/* 实况轨：下游服务 → 聚合产出 → 客户端，按真实数据流向串成一行读数 */}
        <div className="mt-4 flex items-center gap-2.5 border-t border-line pt-4">
          {live && enabledServers.length === 0 && onNavigate ? (
            // 一个服务都没接时，「0/0」什么也没说——这一站换成入口，把空态变成下一步。
            <button
              type="button"
              onClick={() => onNavigate("mcp-servers")}
              className="flex shrink-0 items-center gap-0.5 font-medium text-[13px] text-accent hover:underline"
            >
              {t("dashboard.rail.noServers")}
              <ChevronRight size={14} strokeWidth={2.5} aria-hidden />
            </button>
          ) : (
            <RailStat
              // Hub 已连上但诊断查询还没返回时 downstreams 为空，readyServers 恒为 0——
              // 显示「0/N」像是下游全挂了。这段过渡按 muted 破折号处理，等诊断到位再出真值。
              value={live && downstreams ? `${readyServers}/${enabledServers.length}` : "—"}
              label={t("dashboard.rail.downstreams")}
              muted={!live || !downstreams}
            />
          )}
          <RailFlow />
          <RailStat
            value={live ? catalogTools : "—"}
            label={t("dashboard.rail.tools")}
            accent={live && catalogTools > 0}
            muted={!live}
          />
          <RailFlow />
          <RailStat
            value={live ? `${activeConns}/${totalConns}` : "—"}
            label={t("dashboard.rail.connections")}
            muted={!live}
          />
        </div>
      </Card>

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        {/* 客户端：已安装的列出来，连没连一眼可见；明细与操作在客户端页 */}
        <Card className="p-5">
          <div className="mb-3 flex items-center justify-between">
            <p className="text-sm font-medium text-ink">{t("dashboard.clients.title")}</p>
            {onNavigate ? (
              <button
                type="button"
                onClick={() => onNavigate("clients")}
                className="text-xs font-medium text-accent hover:underline"
              >
                {t("dashboard.servers.manage")}
              </button>
            ) : null}
          </div>
          <QueryState
            query={clients}
            loading={<p className="text-sm text-muted">{t("dashboard.clients.detecting")}</p>}
            error={(message) => (
              <p className="text-sm text-danger">
                {t("dashboard.clients.detectError", { error: message })}
              </p>
            )}
          >
            {() =>
              clientRows.length > 0 ? (
                <div className="space-y-2.5">
                  {shownClients.map((client) => (
                    <div key={client.id} className="flex items-center gap-3">
                      <ClientChip id={client.id} title={client.title} size={28} />
                      <span className="min-w-0 flex-1 truncate text-sm text-ink">
                        {client.title}
                      </span>
                      {client.connected ? (
                        <Tag tone="green">{t("clients.connected")}</Tag>
                      ) : (
                        <span className="text-xs text-faint">
                          {t("dashboard.clients.notConnected")}
                        </span>
                      )}
                    </div>
                  ))}
                  {clientRows.length > SHOWN ? (
                    <p className="pt-1 text-xs text-faint">
                      {t("dashboard.clients.more", { count: clientRows.length - SHOWN })}
                    </p>
                  ) : null}
                </div>
              ) : (
                <p className="text-sm text-muted">{t("dashboard.clients.empty")}</p>
              )
            }
          </QueryState>
        </Card>

        {/* 下游服务健康：聚合器概览的核心信息 */}
        <Card className="p-5">
          <div className="mb-3 flex items-center justify-between">
            <p className="text-sm font-medium text-ink">{t("dashboard.servers.title")}</p>
            {onNavigate ? (
              <button
                type="button"
                onClick={() => onNavigate("mcp-servers")}
                className="text-xs font-medium text-accent hover:underline"
              >
                {t("dashboard.servers.manage")}
              </button>
            ) : null}
          </div>
          <QueryState
            query={servers}
            loading={<p className="text-sm text-muted">{t("dashboard.servers.loading")}</p>}
            error={(message) => (
              <p className="text-sm text-danger">
                {t("dashboard.servers.loadError", { error: message })}
              </p>
            )}
          >
            {() =>
              enabledServers.length > 0 ? (
                <div className="space-y-2.5">
                  {shownServers.map((server) => {
                    const status = cardStatus(server, downstreams, live, t);
                    const crashCount = downstreams?.[server.id]?.crashCount ?? 0;
                    return (
                      <div key={server.id} className="flex items-center gap-3">
                        {/* 与服务器页同一套传输图标：本地命令青、远程地址蓝。 */}
                        <IconChip
                          icon={server.transport === "stdio" ? Terminal : Globe}
                          tone={server.transport === "stdio" ? "teal" : "blue"}
                          size={28}
                        />
                        <span className="min-w-0 flex-1 truncate text-sm text-ink">
                          {server.name}
                        </span>
                        {crashCount > 0 ? (
                          <span className="text-[11px] text-warn">
                            {t("dashboard.servers.restarts", { count: crashCount })}
                          </span>
                        ) : null}
                        <StatusBadge tone={status.tone}>{status.label}</StatusBadge>
                      </div>
                    );
                  })}
                  {enabledServers.length > SHOWN ? (
                    <p className="pt-1 text-xs text-faint">
                      {t("dashboard.servers.more", { count: enabledServers.length - SHOWN })}
                    </p>
                  ) : null}
                </div>
              ) : (
                <p className="text-sm text-muted">{t("dashboard.servers.empty")}</p>
              )
            }
          </QueryState>
        </Card>
      </div>
    </div>
  );
}

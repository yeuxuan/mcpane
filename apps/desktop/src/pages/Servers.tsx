import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Download,
  FileJson,
  LogOut,
  MonitorDown,
  MoreHorizontal,
  Pencil,
  Plus,
  Trash2,
  Upload,
} from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { PageHeader } from "../components/PageHeader";
import { endpointSummary, ServerTransportChip } from "../components/ServerBrief";
import { ServerExportModal } from "../components/ServerExportModal";
import { importSummaryText } from "../components/ServerImportPanel";
import { ServerSetupModal, STATUS_BADGE } from "../components/ServerSetupModal";
import {
  ClientVisibilityIcons,
  clientVisibilityForServer,
  useClientVisibility,
  VisibilityCountBar,
  visibilityCounts,
} from "../components/ServerVisibility";
import type { PageKey } from "../components/Shell";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "../components/shadcn/dropdown-menu";
import {
  Button,
  Card,
  EmptyState,
  ErrorBanner,
  IconButton,
  SearchInput,
  StatusBadge,
  Switch,
  Tag,
} from "../components/ui";
import { ipc, type ServerDef } from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { invalidateServerQueries } from "../lib/serverQueries";
import { errorText } from "../lib/utils";
import { DEFAULT_WS, useWorkspace } from "../lib/workspace";

export type Downstreams = Record<
  string,
  {
    status?: string;
    crashCount?: number;
    lastError?: string | null;
    /** 失败原因代号（Hub `failure_kind`）：界面据此显示具体原因，而不是笼统的「连接失败」。 */
    lastErrorKind?: string | null;
    /** 远程 OAuth 服务器是否已登录；非 OAuth 服务器为 null。 */
    signedIn?: boolean | null;
  }
>;

/** 失败原因代号 → 具体说法；认不出的返回 null（沿用笼统的「连接失败」，原文仍在 tooltip）。 */
function failureLabel(
  kind: string | null | undefined,
  t: (key: string, options?: Record<string, unknown>) => string,
): string | null {
  if (!kind) return null;
  const http = /^http_(\d{3})$/.exec(kind);
  if (http) return t("server.reason.http", { status: http[1] });
  const keys: Record<string, string> = {
    command_not_found: "server.reason.commandNotFound",
    exited: "server.reason.exited",
    no_response: "server.reason.noResponse",
    connection_refused: "server.reason.connectionRefused",
  };
  return keys[kind] ? t(keys[kind]) : null;
}

/**
 * 诊断查询 → cardStatus 入参的换算。单独抽出是因为这里有过一个真 bug：
 * Hub 挂掉后查询进入 error 态，但 `data` 仍是最后一次成功的缓存——直接用它，
 * 整页卡片会拿着陈旧 status 永远显示绿色「运行中」。error 时必须丢弃陈旧
 * 诊断；而「查询还没跑完」（初次加载）不算不可达，按「启动中」处理。
 */
export function diagnosticsView(diag: { isError: boolean; data?: { downstreams?: unknown } }): {
  downstreams: Downstreams | undefined;
  hubReachable: boolean;
} {
  return {
    downstreams: diag.isError ? undefined : (diag.data?.downstreams as Downstreams | undefined),
    hubReachable: !diag.isError,
  };
}

/** 卡片状态徽标：停用/接入 Hub 真实运行状态（启动中→运行中/失败），诊断不可用时回退。 */
/**
 * 收 `t` 而非返回 i18n key：未知状态要把原始状态串透出来
 *（`server.status.unknown` 带插值），返回 key 就表达不了那个参数。
 */
export function cardStatus(
  server: ServerDef,
  downstreams: Downstreams | undefined,
  hubReachable: boolean,
  t: (key: string, options?: Record<string, unknown>) => string,
): { tone: "run" | "stop" | "warn" | "danger"; label: string } {
  if (!server.enabled) return { tone: "stop", label: t("server.status.disabled") };
  const status = downstreams?.[server.id]?.status;
  if (status) {
    // 失败时优先说具体原因（uvx 没装 / 进程退出 / 连接被拒 / HTTP 404…），
    // 降级态也一样：它们同样是探测在报错。
    const reason =
      status === "failed" || status === "degraded"
        ? failureLabel(downstreams?.[server.id]?.lastErrorKind, t)
        : null;
    if (reason) return { tone: status === "failed" ? "danger" : "warn", label: reason };
    const known = STATUS_BADGE[status];
    return known
      ? { tone: known.tone, label: t(known.key) }
      : { tone: "warn", label: t("server.status.unknown", { status }) };
  }
  // 已启用但诊断里还没有该下游：Hub 在跑说明正在连接；Hub 不可达则无从确认，
  // 只能报「未连接」——用与编辑弹窗同一套说法（那里对同一状态早就是灰色
  // 「未连接」）。此前这里回的是绿色「已启用」：Hub 挂掉、一个工具都调不动的
  // 时候，整页卡片反而全绿，是最不该给的信号。
  return hubReachable
    ? { tone: "warn", label: t("server.status.starting") }
    : { tone: "stop", label: t("server.status.disconnected") };
}

/** MCP Server 管理：接入弹窗（导入 JSON / 手动创建 / 远程）+ 列表 + 编辑 + 启停 + 删除 + 导入导出。 */
export function Servers({ onNavigate }: { onNavigate?: (page: PageKey) => void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const servers = useQuery({
    queryKey: queryKeys.servers,
    queryFn: ipc.serversList,
    retry: false,
  });
  // 轮询 Hub 诊断：下游 spawn/连接是异步的，卡片状态要能从「启动中」过渡到
  // 「运行中/连接失败」，靠轮询把 Hub 侧真实状态刷进来。
  const diagnostics = useQuery({
    queryKey: queryKeys.hubDiagnostics,
    queryFn: ipc.hubDiagnostics,
    ...livePoll(5000),
  });
  const { downstreams, hubReachable } = diagnosticsView(diagnostics);
  // 当前工作区 Catalog → 每个 server 暴露的工具数（聚合器的核心价值信息）。
  // 必须跟随工作区：工具启停是按工作区隔离的，而工作区切换器常驻在 Shell 上，
  // 此前这里写死 ws_default，切到别的工作区后卡片上的数字与编辑弹窗里逐条
  // 数出来的对不上，且怎么切都不动。
  const { selectedId } = useWorkspace();
  const catalog = useQuery({
    queryKey: queryKeys.catalog(selectedId),
    queryFn: () => ipc.catalogList(selectedId === DEFAULT_WS ? null : selectedId),
    ...livePoll(5000),
  });
  const toolCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const tool of catalog.data?.tools ?? []) {
      counts[tool.serverId] = (counts[tool.serverId] ?? 0) + 1;
    }
    return counts;
  }, [catalog.data]);
  const refresh = () => invalidateServerQueries(queryClient);

  const [search, setSearch] = useState("");
  // 启停/删除失败的内联报错（启停失败会乐观回滚，需告知用户为何翻回）。
  const [actionError, setActionError] = useState<string | null>(null);
  // 添加弹窗打开时落在哪个页签；null = 关闭。
  const [setupTab, setSetupTab] = useState<"preset" | "json" | null>(null);
  const [editing, setEditing] = useState<ServerDef | null>(null);
  // 导出弹窗预先勾选的服务器；null = 关闭。
  const [exportIds, setExportIds] = useState<string[] | null>(null);

  // 页面级单例 mutation 被所有行的 Switch 复用：用本地 Set 而非 mutation.isPending
  // 跟踪逐行 loading，避免后点的行覆盖先点的行的加载态（TanStack Query 的
  // isPending/variables 只反映最近一次派发的调用）。
  const [pendingIds, setPendingIds] = useState<Set<string>>(new Set());
  const setEnabled = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      ipc.serverSetEnabled(id, enabled),
    // 乐观更新：server_set_enabled 会同步等 Hub 把下游 spawn/重连完成才返回，
    // 期间不做乐观更新的话卡片会一直停在旧状态。先本地翻转 enabled，
    // 失败再回滚；Hub 侧真实运行状态由诊断轮询补上。
    onMutate: async ({ id, enabled }) => {
      // 新一次操作开场即清掉上一次的失败横幅（与下方删除同一策略），
      // 否则旧错误会盖在后续成功操作上一直不散。
      setActionError(null);
      setPendingIds((prev) => new Set(prev).add(id));
      await queryClient.cancelQueries({ queryKey: queryKeys.servers });
      const previous = queryClient.getQueryData<ServerDef[]>(queryKeys.servers);
      queryClient.setQueryData<ServerDef[]>(queryKeys.servers, (old) =>
        old?.map((s) => (s.id === id ? { ...s, enabled } : s)),
      );
      return { previous };
    },
    onError: (error, _vars, context) => {
      if (context?.previous) queryClient.setQueryData(queryKeys.servers, context.previous);
      setActionError(errorText(error));
      // 失败也要拉真值：命令层的 Err 可能是「写库成功但 Hub 未生效」，
      // 此时上面的乐观回滚回错了方向（DB 已是新值），refetch 纠偏。
      refresh();
    },
    onSettled: (_data, _error, { id }) =>
      setPendingIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      }),
    onSuccess: refresh,
  });
  // 远程 MCP 的 OAuth 登录：点击后系统浏览器授权，页面在这里等到 Hub 拿到令牌为止。
  // 按行跟踪等待态，授权期间可随时取消。
  const [loggingIn, setLoggingIn] = useState<Set<string>>(new Set());
  const login = useMutation({
    mutationFn: ipc.serverOauthLogin,
    onMutate: (id) => {
      setActionError(null);
      setLoggingIn((prev) => new Set(prev).add(id));
    },
    onSettled: (_data, _error, id) =>
      setLoggingIn((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      }),
    onSuccess: refresh,
    onError: (error) => {
      const message = errorText(error);
      // 用户自己取消不是错误，不弹红条。
      if (!message.includes("已取消")) setActionError(message);
      refresh();
    },
  });
  const logout = useMutation({
    mutationFn: ipc.serverOauthLogout,
    onMutate: () => setActionError(null),
    onSuccess: refresh,
    onError: (error) => {
      setActionError(errorText(error));
      refresh();
    },
  });
  // 删除同样触发 Hub 侧 ReloadServers 重连，按行跟踪加载态。
  const [pendingDeletes, setPendingDeletes] = useState<Set<string>>(new Set());
  const remove = useMutation({
    mutationFn: ipc.serverDelete,
    onMutate: (id) => {
      setActionError(null);
      setPendingDeletes((prev) => new Set(prev).add(id));
    },
    onSettled: (_data, _error, id) =>
      setPendingDeletes((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      }),
    onSuccess: refresh,
    onError: (error) => {
      setActionError(errorText(error));
      // 同 setEnabled：删除可能已落库、只是 Hub 通知失败，refetch 收敛。
      refresh();
    },
  });
  // 删除不可逆（连同 env / 请求头 / 该 Server 在各工作区的工具治理一并没了），
  // 与 Providers、客户端凭证等处一样过一道 ConfirmDialog，不做一点即删。
  const [confirmDelete, setConfirmDelete] = useState<ServerDef | null>(null);
  const [importResult, setImportResult] = useState<string | null>(null);
  const importClients = useMutation({
    mutationFn: ipc.importFromClients,
    onSuccess: (summary) => {
      setImportResult(importSummaryText(summary, t));
      refresh();
    },
    onError: (mutationError) => setImportResult(errorText(mutationError)),
  });

  const rows = useMemo(() => {
    // slug 恒为小写，检索词也统一小写后再比，否则大写关键词永远搜不到 slug。
    // 检索面刻意排除 env/headers/bearer——凭据值不进搜索索引（对齐 cc-switch）。
    const query = search.toLowerCase();
    return (servers.data ?? []).filter(
      (s) =>
        !query ||
        s.name.toLowerCase().includes(query) ||
        s.slug.includes(query) ||
        (s.command ?? "").toLowerCase().includes(query) ||
        (s.url ?? "").toLowerCase().includes(query),
    );
  }, [servers.data, search]);

  // 行内客户端可见性（deny-list）：页面级一次，所有行共用。
  const visibility = useClientVisibility();
  const allServerIds = (servers.data ?? []).map((s) => s.id);
  const counts = visibility.hubReachable
    ? visibilityCounts(allServerIds, visibility.connectionsByType, visibility.deniedByConnectionId)
    : [];

  return (
    <div>
      <PageHeader
        title={t("nav.mcp-servers")}
        actions={
          <>
            <SearchInput
              className="w-52"
              placeholder={t("servers.searchPlaceholder")}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
            />
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  icon={Download}
                  data-tour="servers-import"
                  loading={importClients.isPending}
                >
                  {t("servers.import")}
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onSelect={() => importClients.mutate()}>
                  <MonitorDown />
                  {t("servers.importFromClients")}
                </DropdownMenuItem>
                <DropdownMenuItem onSelect={() => setSetupTab("json")}>
                  <FileJson />
                  {t("servers.importFromJson")}
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
            <Button
              icon={Upload}
              disabled={!servers.data?.length}
              onClick={() => setExportIds((servers.data ?? []).map((s) => s.id))}
            >
              {t("servers.export")}
            </Button>
            <Button
              variant="primary"
              icon={Plus}
              data-tour="servers-add"
              onClick={() => setSetupTab("preset")}
            >
              {t("servers.add")}
            </Button>
          </>
        }
      />
      {/* Session-Pinned（ADR-004）：客户端在连接时 pin 下当时那份工具目录，
          此后的启停与治理只对新会话生效。界面此前完全没提这件事——用户在这里
          启用一个 Server，回到正跑着的 Claude Code 里看不到，只会以为没生效。 */}
      <p className="mb-4 -mt-1 text-xs text-muted">{t("common.newSessionHint")}</p>
      {importResult ? <p className="mb-4 -mt-1 text-xs text-muted">{importResult}</p> : null}
      <VisibilityCountBar counts={counts} />

      <ErrorBanner
        message={actionError ? t("common.actionFailed", { error: actionError }) : null}
        onDismiss={() => setActionError(null)}
      />
      <ErrorBanner
        message={visibility.error ? t("common.actionFailed", { error: visibility.error }) : null}
        onDismiss={visibility.clearError}
      />

      {confirmDelete ? (
        <ConfirmDialog
          title={t("servers.deleteConfirmTitle")}
          message={t("servers.deleteConfirmMessage", { name: confirmDelete.name })}
          confirmText={t("servers.deleteConfirmAction")}
          onCancel={() => setConfirmDelete(null)}
          onConfirm={() => {
            remove.mutate(confirmDelete.id);
            setConfirmDelete(null);
          }}
        />
      ) : null}

      {setupTab ? (
        <ServerSetupModal
          initialTab={setupTab}
          onClose={() => setSetupTab(null)}
          onImported={setImportResult}
        />
      ) : null}
      {exportIds && servers.data ? (
        <ServerExportModal
          servers={servers.data}
          initialIds={exportIds}
          onClose={() => setExportIds(null)}
        />
      ) : null}
      {editing ? (
        <ServerSetupModal
          editing={editing}
          onClose={() => setEditing(null)}
          onManageWorkspaces={
            onNavigate
              ? () => {
                  setEditing(null);
                  onNavigate("workspaces");
                }
              : undefined
          }
        />
      ) : null}

      {servers.isLoading ? (
        <Card className="p-12 text-center">
          <p className="text-sm text-muted">{t("common.loading")}</p>
        </Card>
      ) : null}

      {servers.isError ? (
        <Card className="p-12 text-center">
          <p className="text-sm text-danger">
            {t("servers.loadError", { error: errorText(servers.error) })}
          </p>
        </Card>
      ) : null}

      {servers.isSuccess && servers.data.length === 0 ? (
        <EmptyState
          title={t("servers.empty")}
          description={t("servers.emptyHint")}
          action={
            <Button size="sm" icon={Plus} onClick={() => setSetupTab("preset")}>
              {t("servers.add")}
            </Button>
          }
        />
      ) : null}

      {/* 有数据但被搜索过滤光：空态条件盯的是 servers.data、渲染用的是 rows，
          两者都不成立时页面只剩一个空 grid。与 Logs 页同口径分开这两种「空」。 */}
      {servers.isSuccess && servers.data.length > 0 && rows.length === 0 ? (
        <Card className="p-12 text-center">
          <p className="text-sm text-muted">{t("servers.noMatch")}</p>
        </Card>
      ) : null}

      {rows.length > 0 ? (
        // 多行合成一张纸：容器收边框，行间只留分隔线。
        <Card className="divide-y divide-line">
          {rows.map((server, rowIndex) => {
            const status = cardStatus(server, downstreams, hubReachable, t);
            const endpoint = endpointSummary(server);
            const toolCount = toolCounts[server.id];
            const crashCount = downstreams?.[server.id]?.crashCount ?? 0;
            const lastError = downstreams?.[server.id]?.lastError ?? null;
            const signedIn = downstreams?.[server.id]?.signedIn === true;
            const needsAuth = server.enabled && downstreams?.[server.id]?.status === "needs-auth";
            const rowVisibility = visibility.hubReachable
              ? clientVisibilityForServer(
                  server.id,
                  visibility.connectionsByType,
                  visibility.deniedByConnectionId,
                )
              : [];
            return (
              <div
                key={server.id}
                // 导览指首行讲"这一行能做什么"；列表为空时这一站自动降级成居中说明。
                data-tour={rowIndex === 0 ? "servers-card" : undefined}
                className="group flex items-center gap-3 px-4 py-3"
              >
                <ServerTransportChip transport={server.transport} />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="truncate font-semibold text-[13px] text-ink">
                      {server.name}
                    </span>
                    <span title={lastError ?? undefined}>
                      <StatusBadge tone={status.tone}>{status.label}</StatusBadge>
                    </span>
                    {needsAuth ? (
                      loggingIn.has(server.id) ? (
                        <>
                          <Button size="sm" variant="secondary" loading disabled>
                            {t("servers.loginWaiting")}
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => ipc.serverOauthCancel(server.id)}
                          >
                            {t("servers.loginCancel")}
                          </Button>
                        </>
                      ) : (
                        <Button size="sm" variant="primary" onClick={() => login.mutate(server.id)}>
                          {t("servers.login")}
                        </Button>
                      )
                    ) : null}
                    {server.enabled && toolCount != null ? (
                      <Tag>{t("servers.toolCount", { count: toolCount })}</Tag>
                    ) : null}
                    {crashCount ? (
                      <span className="text-[11px] font-medium text-danger">
                        {t("server.crashes", { count: crashCount })}
                      </span>
                    ) : null}
                  </div>
                  <p
                    className="mt-0.5 truncate font-mono text-xs text-faint"
                    title={endpoint.full || undefined}
                  >
                    {server.slug}__
                    {endpoint.text ? <span className="text-muted"> · {endpoint.text}</span> : null}
                  </p>
                </div>

                {/* 谁能看到这个 Server——Hub 未运行时这一列空着，不给假信号。 */}
                {visibility.hubReachable ? (
                  <ClientVisibilityIcons
                    serverId={server.id}
                    serverName={server.name}
                    visibility={rowVisibility}
                    isPending={visibility.isPending}
                    onToggle={visibility.toggle}
                  />
                ) : null}

                <Switch
                  checked={server.enabled}
                  onChange={(next) => setEnabled.mutate({ id: server.id, enabled: next })}
                  label={t("servers.toggle", {
                    action: t(server.enabled ? "servers.disable" : "servers.enable"),
                    name: server.name,
                  })}
                  loading={pendingIds.has(server.id)}
                />
                {/* 编辑/删除低频，收进行尾「…」（与客户端页同一形制）。曾用悬停显隐：
                    关掉弹窗后焦点按规范回到行内按钮，focus-within 让它一直挂着不消失。 */}
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <IconButton
                      icon={MoreHorizontal}
                      aria-label={t("servers.moreActions", { name: server.name })}
                      loading={pendingDeletes.has(server.id)}
                    />
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end">
                    <DropdownMenuItem onSelect={() => setEditing(server)}>
                      <Pencil />
                      {t("servers.menuEdit")}
                    </DropdownMenuItem>
                    <DropdownMenuItem onSelect={() => setExportIds([server.id])}>
                      <Upload />
                      {t("servers.menuExport")}
                    </DropdownMenuItem>
                    {signedIn ? (
                      <DropdownMenuItem onSelect={() => logout.mutate(server.id)}>
                        <LogOut />
                        {t("servers.logout")}
                      </DropdownMenuItem>
                    ) : null}
                    <DropdownMenuSeparator />
                    <DropdownMenuItem
                      variant="destructive"
                      onSelect={() => setConfirmDelete(server)}
                    >
                      <Trash2 />
                      {t("servers.menuDelete")}
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </div>
            );
          })}
        </Card>
      ) : null}
    </div>
  );
}

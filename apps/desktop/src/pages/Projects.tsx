import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { Boxes, FolderKanban, Plus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { BrandGlyph, isBrandName } from "../components/BrandIcon";
import { brandTextClass } from "../components/ClientChip";
import { PageHeader } from "../components/PageHeader";
import type { ServerDef } from "../components/ServerSetupModal";
import type { PageKey } from "../components/Shell";
import { Button, Card, ErrorBanner, Field, IconChip, Tag } from "../components/ui";
import { clientDisplayName } from "../lib/clients";
import { type Catalog, ipc } from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { cn, errorText } from "../lib/utils";
import { DEFAULT_WS, useWorkspace, type Workspace } from "../lib/workspace";
import { cardStatus, diagnosticsView } from "./Servers";

/** 状态点配色：与 StatusBadge 同一 tone 语义，点阵/成员行共用。 */
const TONE_DOT: Record<"run" | "stop" | "warn" | "danger", string> = {
  run: "bg-run-dot",
  stop: "bg-faint",
  warn: "bg-warn",
  danger: "bg-danger",
};

const MEMBER_SHOWN = 6;
const DOT_CAP = 24;

interface Member {
  server: ServerDef;
  tone: keyof typeof TONE_DOT;
  label: string;
}

/**
 * 健康指纹：每点一台成员服务器，颜色即实时状态。
 * 多张分组卡并排时，扫一眼点阵即可对比各分组的健康分布。
 */
function HealthDots({ members }: { members: Member[] }) {
  const { t } = useTranslation();
  if (members.length === 0) return null;
  const online = members.filter((m) => m.tone === "run").length;
  return (
    <div
      role="img"
      aria-label={t("projects.onlineDots", { online, total: members.length })}
      className="flex max-w-[132px] flex-wrap items-center justify-end gap-1"
    >
      {members.slice(0, DOT_CAP).map((m) => (
        <span
          key={m.server.id}
          title={`${m.server.name} · ${m.label}`}
          className={cn("h-[7px] w-[7px] rounded-full", TONE_DOT[m.tone])}
        />
      ))}
      {members.length > DOT_CAP ? (
        <span className="text-[10px] text-faint">+{members.length - DOT_CAP}</span>
      ) : null}
    </div>
  );
}

interface ConnectionChip {
  clientConnectionId: string;
  clientType: string;
  /** 已解析的客户端显示名（见 clientDisplayName）。 */
  label: string;
  tokenPrefix: string;
  protocolVersion?: string | null;
}

/** 同一客户端的多条连接合成一个 chip，保持首次出现的顺序。 */
export function groupConnections(connections: ConnectionChip[]) {
  const groups = new Map<string, { clientType: string; label: string; items: ConnectionChip[] }>();
  for (const c of connections) {
    const g = groups.get(c.clientType);
    if (g) g.items.push(c);
    else groups.set(c.clientType, { clientType: c.clientType, label: c.label, items: [c] });
  }
  return [...groups.values()];
}

/** 单个分组卡：身份 → 下游成员（实时健康）→ 暴露面（工具 + 绑定客户端）。 */
function GroupCard({
  dataTour,
  group,
  isDefault,
  isCurrent,
  members,
  extraDisabled,
  catalog,
  connections,
  hubLive,
  onManage,
  onGotoServers,
}: {
  /** 导览锚点，只有首张卡拿到。 */
  dataTour?: string;
  group: Workspace;
  isDefault: boolean;
  isCurrent: boolean;
  members: Member[];
  /** 默认分组专用：未纳入聚合的已停用服务器数。 */
  extraDisabled: number;
  catalog: Catalog | undefined;
  connections: ConnectionChip[];
  hubLive: boolean;
  onManage: () => void;
  onGotoServers?: () => void;
}) {
  const { t } = useTranslation();
  const toolCount = catalog ? catalog.tools.length : null;
  const toolsOf = (serverId: string) =>
    catalog ? catalog.tools.filter((t) => t.serverId === serverId).length : null;
  const shown = members.slice(0, MEMBER_SHOWN);

  return (
    <Card data-tour={dataTour} className="flex flex-col p-5">
      <div className="flex items-start gap-3">
        <IconChip icon={isDefault ? FolderKanban : Boxes} tone="slate" />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <p className="truncate font-semibold text-ink">{group.name}</p>
            {isCurrent ? <Tag tone="blue">{t("projects.current")}</Tag> : null}
          </div>
          {/* 数组 join 而非拼接：分隔符与语言无关，各片段各自是完整短语。 */}
          <p className="mt-0.5 text-xs text-muted">
            {[
              isDefault
                ? t("projects.allEnabled")
                : t("projects.memberCount", { count: members.length }),
              toolCount !== null
                ? t("projects.toolsAggregated", { count: toolCount })
                : hubLive
                  ? null
                  : t("projects.hubDown"),
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
        </div>
        <HealthDots members={members} />
      </div>

      <div className="mt-4 flex-1 space-y-2 border-t border-line pt-4">
        {shown.map(({ server, tone, label }) => {
          const tools = toolsOf(server.id);
          return (
            <div key={server.id} className="flex items-center gap-2.5 text-sm">
              <span title={label} className={cn("h-2 w-2 shrink-0 rounded-full", TONE_DOT[tone])} />
              <span className="min-w-0 flex-1 truncate text-ink">{server.name}</span>
              <span className="shrink-0 font-mono text-xs text-faint">
                {server.transport}
                {tools !== null && tools > 0
                  ? ` · ${t("projects.toolsShort", { count: tools })}`
                  : ""}
              </span>
            </div>
          );
        })}
        {members.length > MEMBER_SHOWN ? (
          <p className="text-xs text-faint">
            {t("projects.moreMembers", { count: members.length - MEMBER_SHOWN })}
          </p>
        ) : null}
        {extraDisabled > 0 ? (
          <p className="text-xs text-faint">
            {t("projects.extraDisabled", { count: extraDisabled })}
          </p>
        ) : null}
        {members.length === 0 ? (
          isDefault ? (
            // 两个完整片段而非「句子 + 内嵌按钮 + 句末标点」：
            // 中英的标点位置不同，拼碎片在任一语言里都会出错。
            <p className="text-sm text-muted">
              {t("projects.noServersEnabled")}{" "}
              {onGotoServers ? (
                <button
                  type="button"
                  onClick={onGotoServers}
                  className="font-medium text-accent hover:underline"
                >
                  {t("projects.gotoServers")}
                </button>
              ) : (
                t("projects.gotoServers")
              )}
            </p>
          ) : (
            <p className="text-sm text-muted">{t("projects.noMembers")}</p>
          )
        ) : null}
      </div>

      <div className="mt-4 flex items-center gap-2 border-t border-line pt-3.5">
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
          {/* 按客户端聚合：chip 上写客户端名（Token 前缀对用户没意义，挪进 title 供排障）；
              同一客户端多条连接合成一个 chip、title 里列出条数与各自前缀。 */}
          {groupConnections(connections).map((g) => (
            <span
              key={g.clientType}
              title={
                t("projects.connectionTitle", {
                  client: g.label,
                  prefix: g.items.map((c) => c.tokenPrefix).join("… / "),
                }) +
                (g.items.length > 1
                  ? t("projects.connectionCount", { count: g.items.length })
                  : "") +
                (g.items[0]?.protocolVersion
                  ? t("projects.connectionProtocol", { version: g.items[0].protocolVersion })
                  : "")
              }
              className="inline-flex items-center gap-1.5 rounded-md bg-app px-2 py-1 text-[11px] font-medium text-muted"
            >
              {isBrandName(g.clientType) ? (
                <span className={brandTextClass(g.clientType)}>
                  <BrandGlyph name={g.clientType} size={11} />
                </span>
              ) : null}
              <span>{g.label}</span>
              {g.items.length > 1 ? <span className="text-faint">×{g.items.length}</span> : null}
            </span>
          ))}
          {connections.length === 0 ? (
            <span className="text-xs text-faint">
              {t(hubLive ? "projects.noConnections" : "projects.connectionsUnknown")}
            </span>
          ) : null}
        </div>
        <button
          type="button"
          data-tour={isDefault ? "projects-manage" : undefined}
          onClick={onManage}
          className="shrink-0 text-xs font-medium text-accent hover:underline"
        >
          {t("projects.manage")}
        </button>
      </div>
    </Card>
  );
}

/**
 * 工作区总览：Server 按工作区分组的总览（P0 由 Workspace 承载分组，ADR-004）。
 * 每个工作区一张卡，三段结构对应聚合器数据流：下游成员 → 工具目录 → 客户端连接。
 * （PageKey 仍是 `projects`，只改展示文案。明细治理在「工作区设置」页。）
 */
export function Projects({ onNavigate }: { onNavigate?: (page: PageKey) => void }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { selectedId, setSelectedId, workspaces } = useWorkspace();
  const [newName, setNewName] = useState("");
  const [actionError, setActionError] = useState<string | null>(null);

  const servers = useQuery({
    queryKey: queryKeys.servers,
    queryFn: ipc.serversList,
    retry: false,
  });
  const hub = useQuery({
    queryKey: queryKeys.hubStatus,
    queryFn: ipc.hubStatus,
    ...livePoll(5000),
  });
  // 只为把连接的 clientType 翻成客户端名；页面级查一次下钻，不在每张卡里各查各的。
  const mcpClients = useQuery({ queryKey: queryKeys.mcpClients, queryFn: ipc.mcpClientsStatus });
  const diagnostics = useQuery({
    queryKey: queryKeys.hubDiagnostics,
    queryFn: ipc.hubDiagnostics,
    ...livePoll(10000),
    enabled: hub.isSuccess,
  });

  const customWs = workspaces.filter((w) => w.id !== DEFAULT_WS);
  const groups: Workspace[] = [{ id: DEFAULT_WS, name: t("workspace.default") }, ...customWs];

  const memberQueries = useQueries({
    queries: customWs.map((ws) => ({
      queryKey: queryKeys.workspaceServers(ws.id),
      queryFn: () => ipc.workspaceServers(ws.id),
      retry: false,
    })),
  });
  // 每个工作区一条 catalog 轮询：工作区多时是 N 条 Hub 往返。总览页的工具计数
  // 不是实时诉求，放宽到 30s（精细治理在工作区设置页，那边仍是 10s）。
  const catalogQueries = useQueries({
    queries: groups.map((ws) => ({
      queryKey: queryKeys.catalog(ws.id),
      queryFn: () => ipc.catalogList(ws.id === DEFAULT_WS ? null : ws.id),
      ...livePoll(30000),
    })),
  });

  const createWorkspace = useMutation({
    mutationFn: (name: string) => {
      setActionError(null);
      return ipc.workspaceCreate(name);
    },
    onSuccess: () => {
      setNewName("");
      queryClient.invalidateQueries({ queryKey: queryKeys.workspaces });
    },
    onError: (error) => setActionError(errorText(error)),
  });

  const hubLive = hub.isSuccess;
  // 与服务器页/总览页同口径：Hub 不可达时丢弃陈旧诊断缓存，不让卡片拿旧 status 报绿。
  const downstreams = hubLive ? diagnosticsView(diagnostics).downstreams : undefined;
  const allServers = servers.data ?? [];
  const enabledServers = allServers.filter((s) => s.enabled);

  const toMember = (server: ServerDef): Member => {
    const status = cardStatus(server, downstreams, hubLive, t);
    return { server, tone: status.tone, label: status.label };
  };
  const membersOf = (group: Workspace, index: number): Member[] => {
    if (group.id === DEFAULT_WS) return enabledServers.map(toMember);
    const ids = new Set(memberQueries[index - 1]?.data ?? []);
    return allServers.filter((s) => ids.has(s.id)).map(toMember);
  };
  const connectionsOf = (group: Workspace): ConnectionChip[] =>
    (hub.data?.connections ?? [])
      .filter((c) => c.enabled && (c.defaultWorkspaceId ?? DEFAULT_WS) === group.id)
      .map((c) => ({ ...c, label: clientDisplayName(c.clientType, mcpClients.data) }));

  const manage = (group: Workspace) => {
    setSelectedId(group.id);
    onNavigate?.("workspaces");
  };

  return (
    <div>
      <PageHeader title={t("projects.title")} />

      {/* 操作失败优先（用户刚点了什么，等着回执）；其次是服务器列表读取失败——
          它是 retry:false，一失败下面每个分组都退化成「还没有启用的服务器」，
          与真的空空如也长得一模一样。读取失败是持续状态，不给关闭按钮。 */}
      <ErrorBanner
        message={
          actionError
            ? t("common.actionFailed", { error: actionError })
            : servers.isError
              ? t("projects.serversError", { error: errorText(servers.error) })
              : null
        }
        onDismiss={actionError ? () => setActionError(null) : undefined}
      />

      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        {groups.map((group, index) => (
          <GroupCard
            key={group.id}
            // 默认分组恒存在，导览指它讲"分组是什么"。
            dataTour={index === 0 ? "projects-card" : undefined}
            group={group}
            isDefault={group.id === DEFAULT_WS}
            isCurrent={group.id === selectedId}
            members={membersOf(group, index)}
            extraDisabled={group.id === DEFAULT_WS ? allServers.length - enabledServers.length : 0}
            catalog={catalogQueries[index]?.data}
            connections={connectionsOf(group)}
            hubLive={hubLive}
            onManage={() => manage(group)}
            onGotoServers={onNavigate ? () => onNavigate("mcp-servers") : undefined}
          />
        ))}

        {/* 新建分组卡：创建入口就放在新卡将要出现的位置 */}
        <div className="flex min-h-[180px] flex-col items-center justify-center gap-3 rounded-xl border border-dashed border-line-strong p-5">
          <p className="text-sm text-muted">
            {t(customWs.length === 0 ? "projects.newGroupFirst" : "projects.newGroup")}
          </p>
          <div className="flex items-center gap-2">
            <Field
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && newName.trim()) createWorkspace.mutate(newName.trim());
              }}
              placeholder={t("projects.groupName")}
              className="w-44"
            />
            <Button
              icon={Plus}
              data-tour="projects-create"
              disabled={!newName.trim()}
              loading={createWorkspace.isPending}
              onClick={() => createWorkspace.mutate(newName.trim())}
            >
              {t("projects.create")}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}

import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { clientDisplayName } from "../lib/clients";
import { type HubConnection, ipc } from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { cn, errorText } from "../lib/utils";
import { BrandGlyph, isBrandName } from "./BrandIcon";
import { brandTextClass } from "./ClientChip";

/**
 * 「这个 Server 对哪些客户端可见」——Hub 的核心价值，此前只藏在客户端页的访问控制
 * 弹窗里。这里把它摆到服务器列表的每一行：按客户端类型一个小品牌图标，亮 = 可见、
 * 灰 = 已拒绝，点一下翻转。
 *
 * 模型：可见性是按**连接**（Token）记的 deny-list。同一客户端通常只有一条连接；
 * 多条时按「任一被拒 = 灰」汇总，点击对该类型下全部连接一起生效——这是"一个
 * Server × 一个客户端"的动作，不是跨 Server 的批量。要逐条连接精细控制仍走
 * 客户端页的访问控制弹窗，两处共用同一批查询键，天然同步。
 */

export interface ClientVisibility {
  clientType: string;
  label: string;
  /** 该类型下的启用连接。 */
  connections: HubConnection[];
  /** 所有连接都未拒绝该 Server。 */
  visible: boolean;
  deniedCount: number;
}

/** 单个 Server 在各已接入客户端类型上的可见性（纯函数，便于单测）。 */
export function clientVisibilityForServer(
  serverId: string,
  connectionsByType: ReadonlyArray<{
    clientType: string;
    label: string;
    connections: HubConnection[];
  }>,
  deniedByConnectionId: Readonly<Record<string, readonly string[] | undefined>>,
): ClientVisibility[] {
  return connectionsByType.map((group) => {
    const deniedCount = group.connections.filter((c) =>
      (deniedByConnectionId[c.clientConnectionId] ?? []).includes(serverId),
    ).length;
    return { ...group, visible: deniedCount === 0, deniedCount };
  });
}

/** 头部计数条用：每个客户端类型可见的 Server 数 / 总数（按全量，不按搜索过滤后）。 */
export function visibilityCounts(
  serverIds: readonly string[],
  connectionsByType: ReadonlyArray<{
    clientType: string;
    label: string;
    connections: HubConnection[];
  }>,
  deniedByConnectionId: Readonly<Record<string, readonly string[] | undefined>>,
): Array<{ clientType: string; label: string; visible: number; total: number }> {
  return connectionsByType.map((group) => {
    const visible = serverIds.filter((serverId) =>
      group.connections.every(
        (c) => !(deniedByConnectionId[c.clientConnectionId] ?? []).includes(serverId),
      ),
    ).length;
    return { clientType: group.clientType, label: group.label, visible, total: serverIds.length };
  });
}

/** 按 clientType 分组的启用连接，顺序按首次出现。 */
export function groupConnectionsByType(
  connections: readonly HubConnection[],
  mcpClients: ReadonlyArray<{ id: string; display: string }> | undefined,
) {
  const groups = new Map<
    string,
    { clientType: string; label: string; connections: HubConnection[] }
  >();
  for (const c of connections) {
    if (!c.enabled) continue;
    const g = groups.get(c.clientType);
    if (g) g.connections.push(c);
    else
      groups.set(c.clientType, {
        clientType: c.clientType,
        label: clientDisplayName(c.clientType, mcpClients),
        connections: [c],
      });
  }
  return [...groups.values()];
}

/** 页面级一次：连接分组 + 逐连接 deny-list + 翻转 mutation。所有行共用。 */
export function useClientVisibility() {
  const queryClient = useQueryClient();
  // 与概览/工作区页同一把轮询：客户端在别处接入后，这一列几秒内自己更新。
  const hub = useQuery({
    queryKey: queryKeys.hubStatus,
    queryFn: ipc.hubStatus,
    ...livePoll(5000),
  });
  const mcpClients = useQuery({
    queryKey: queryKeys.mcpClients,
    queryFn: ipc.mcpClientsStatus,
    retry: false,
  });
  const connections = hub.data?.connections ?? [];
  // 分组与映射都是十几条连接的线性计算，不值得 memo。
  const connectionsByType = groupConnectionsByType(connections, mcpClients.data);
  const enabledConnections = connections.filter((c) => c.enabled);
  const deniedQueries = useQueries({
    queries: enabledConnections.map((c) => ({
      queryKey: queryKeys.connectionDeniedServers(c.clientConnectionId),
      queryFn: () => ipc.connectionDeniedServers(c.clientConnectionId),
      retry: false,
    })),
  });
  const deniedByConnectionId: Record<string, string[] | undefined> = {};
  enabledConnections.forEach((c, i) => {
    deniedByConnectionId[c.clientConnectionId] = deniedQueries[i]?.data;
  });

  const [pending, setPending] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const pendingKey = (serverId: string, clientType: string) => `${serverId}::${clientType}`;
  const toggle = useMutation({
    mutationFn: async ({
      serverId,
      group,
      visible,
    }: {
      serverId: string;
      group: { clientType: string; connections: HubConnection[] };
      visible: boolean;
    }) => {
      await Promise.all(
        group.connections.map((c) =>
          ipc.connectionServerAccessSet(c.clientConnectionId, serverId, visible),
        ),
      );
    },
    // 乐观更新：逐连接翻转 deny-list，失败整组回滚（与 ClientAccessControlModal 同款）。
    onMutate: async ({ serverId, group, visible }) => {
      setError(null);
      setPending((prev) => new Set(prev).add(pendingKey(serverId, group.clientType)));
      const previous: Array<[readonly unknown[], string[] | undefined]> = [];
      for (const c of group.connections) {
        const key = queryKeys.connectionDeniedServers(c.clientConnectionId);
        await queryClient.cancelQueries({ queryKey: key });
        previous.push([key, queryClient.getQueryData<string[]>(key)]);
        queryClient.setQueryData<string[]>(key, (old = []) =>
          visible ? old.filter((id) => id !== serverId) : [...old, serverId],
        );
      }
      return { previous };
    },
    onError: (mutationError, _vars, context) => {
      for (const [key, data] of context?.previous ?? []) queryClient.setQueryData(key, data);
      setError(errorText(mutationError));
    },
    onSettled: (_data, _error, { serverId, group }) => {
      setPending((prev) => {
        const next = new Set(prev);
        next.delete(pendingKey(serverId, group.clientType));
        return next;
      });
      for (const c of group.connections) {
        queryClient.invalidateQueries({
          queryKey: queryKeys.connectionDeniedServers(c.clientConnectionId),
        });
      }
    },
  });

  return {
    /** Hub 状态拿不到（未运行）时整列不可用。 */
    hubReachable: hub.isSuccess,
    connectionsByType,
    deniedByConnectionId,
    /** 已接入但没有一条连接的客户端类型数不在这里——图标组只画已接入的。 */
    toggle: (serverId: string, group: ClientVisibility, visible: boolean) =>
      toggle.mutate({ serverId, group, visible }),
    isPending: (serverId: string, clientType: string) =>
      pending.has(pendingKey(serverId, clientType)),
    error,
    clearError: () => setError(null),
  };
}

/** 一行里的客户端可见性图标组。 */
export function ClientVisibilityIcons({
  serverId,
  serverName,
  visibility,
  isPending,
  onToggle,
}: {
  serverId: string;
  serverName: string;
  visibility: ClientVisibility[];
  isPending: (serverId: string, clientType: string) => boolean;
  onToggle: (serverId: string, group: ClientVisibility, visible: boolean) => void;
}) {
  const { t } = useTranslation();
  if (visibility.length === 0) {
    return <span className="text-[11px] text-faint">{t("servers.visibility.none")}</span>;
  }
  return (
    <fieldset
      className="m-0 flex items-center gap-1 border-0 p-0"
      aria-label={t("servers.visibility.group")}
    >
      {visibility.map((v) => {
        const busy = isPending(serverId, v.clientType);
        const title = v.visible
          ? t("servers.visibility.visible", { client: v.label, server: serverName })
          : t("servers.visibility.hidden", { client: v.label, server: serverName });
        const multi =
          v.connections.length > 1
            ? t("servers.visibility.multi", { count: v.connections.length })
            : "";
        return (
          <button
            key={v.clientType}
            type="button"
            role="switch"
            aria-checked={v.visible}
            aria-label={title}
            aria-busy={busy}
            title={title + multi}
            disabled={busy}
            onClick={() => onToggle(serverId, v, !v.visible)}
            className={cn(
              "flex size-7 items-center justify-center rounded-md border transition-[opacity,background-color,border-color]",
              // 能看到的亮出品牌色，看不到的去色压暗：一排扫过去就分得清。
              v.visible
                ? cn(
                    "border-line bg-card opacity-100 hover:border-accent",
                    brandTextClass(v.clientType),
                  )
                : "border-transparent bg-app opacity-40 grayscale hover:opacity-70",
              busy && "animate-pulse",
            )}
          >
            {isBrandName(v.clientType) ? (
              <BrandGlyph name={v.clientType} size={15} muted={!v.visible} />
            ) : (
              <span className="font-semibold text-[11px]">{v.label.slice(0, 1).toUpperCase()}</span>
            )}
          </button>
        );
      })}
    </fieldset>
  );
}

/** 头部只读计数条：「Claude Code 可见 6/7 · Codex 5/7」。 */
export function VisibilityCountBar({
  counts,
}: {
  counts: Array<{ clientType: string; label: string; visible: number; total: number }>;
}) {
  const { t } = useTranslation();
  if (counts.length === 0) return null;
  return (
    <p className="mb-4 -mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted">
      {counts.map((c) => (
        <span key={c.clientType} className="inline-flex items-center gap-1.5">
          {isBrandName(c.clientType) ? (
            <span className={brandTextClass(c.clientType)}>
              <BrandGlyph name={c.clientType} size={12} />
            </span>
          ) : null}
          {t("servers.visibility.count", { client: c.label, visible: c.visible, total: c.total })}
        </span>
      ))}
    </p>
  );
}

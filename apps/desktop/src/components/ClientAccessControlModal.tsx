import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Cable, KeyRound, Server } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { type ClientTypeId, type HubConnection, ipc, type ServerDef } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { errorText, shortConnectionId } from "../lib/utils";
import { EmptyState, IconChip, Modal, Switch, Tag } from "./ui";

/**
 * 访问控制（Access Control）：按连接（Token）的 Server deny-list 开关。
 * 默认全部允许；关闭后该 Token 在 tools/list 看不到、tools/call 被拒绝，
 * 对活跃会话立即生效（Hub 侧请求时实时校验，不随 Session pin）。
 */
export function ClientAccessControlModal({
  clientId,
  title,
  onClose,
}: {
  clientId: ClientTypeId;
  title: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const hub = useQuery({ queryKey: queryKeys.hubStatus, queryFn: ipc.hubStatus, retry: false });
  const servers = useQuery({
    queryKey: queryKeys.servers,
    queryFn: ipc.serversList,
    retry: false,
  });

  const connections = (hub.data?.connections ?? []).filter(
    (connection) => connection.clientType === clientId,
  );

  return (
    <Modal
      title={t("accessControl.title", { client: title })}
      size="lg"
      onClose={onClose}
      footer={<p className="mr-auto text-xs text-faint">{t("accessControl.footerHint")}</p>}
    >
      {hub.isError ? <p className="text-sm text-danger">{t("accessControl.hubDown")}</p> : null}

      {hub.isSuccess && connections.length === 0 ? (
        <EmptyState title={t("accessControl.noConnections", { client: title })} />
      ) : null}

      {hub.isSuccess && connections.length > 0 && servers.isSuccess && servers.data.length === 0 ? (
        <EmptyState title={t("accessControl.noServers")} />
      ) : null}

      <div className="space-y-4">
        {connections.map((connection) => (
          <ConnectionAccessSection
            key={connection.clientConnectionId}
            connection={connection}
            servers={servers.data ?? []}
          />
        ))}
      </div>
    </Modal>
  );
}

/** 单个连接的 Server 开关列表（每连接独立查询 deny-list，独立乐观更新）。 */
function ConnectionAccessSection({
  connection,
  servers,
}: {
  connection: HubConnection;
  servers: ServerDef[];
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const deniedKey = queryKeys.connectionDeniedServers(connection.clientConnectionId);
  const denied = useQuery({
    queryKey: deniedKey,
    queryFn: () => ipc.connectionDeniedServers(connection.clientConnectionId),
    retry: false,
  });

  // 页面级单例 mutation 被所有行的 Switch 复用：用本地 Set 而非 mutation.isPending
  // 跟踪逐行 loading（同 Servers.tsx 的启停模式）。
  const [pendingIds, setPendingIds] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const setAccess = useMutation({
    mutationFn: ({ serverId, allowed }: { serverId: string; allowed: boolean }) =>
      ipc.connectionServerAccessSet(connection.clientConnectionId, serverId, allowed),
    // 乐观更新：先本地翻转 deny-list，控制面报错（含 Hub 未运行）再回滚。
    onMutate: async ({ serverId, allowed }) => {
      setPendingIds((prev) => new Set(prev).add(serverId));
      setError(null);
      await queryClient.cancelQueries({ queryKey: deniedKey });
      const previous = queryClient.getQueryData<string[]>(deniedKey);
      queryClient.setQueryData<string[]>(deniedKey, (old = []) =>
        allowed ? old.filter((id) => id !== serverId) : [...old, serverId],
      );
      return { previous };
    },
    onError: (mutationError, _vars, context) => {
      if (context?.previous) queryClient.setQueryData(deniedKey, context.previous);
      setError(errorText(mutationError));
    },
    onSettled: (_data, _error, { serverId }) => {
      setPendingIds((prev) => {
        const next = new Set(prev);
        next.delete(serverId);
        return next;
      });
      queryClient.invalidateQueries({ queryKey: deniedKey });
    },
  });

  const deniedIds = denied.data ?? [];

  return (
    <div className="rounded-lg border border-line p-3.5">
      <div className="flex items-center gap-3">
        <IconChip
          icon={connection.enabled ? Cable : KeyRound}
          tone={connection.enabled ? "sky" : "slate"}
        />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span
              className="font-mono text-sm font-semibold text-ink"
              title={connection.clientConnectionId}
            >
              {shortConnectionId(connection.clientConnectionId)}
            </span>
            {!connection.enabled ? <Tag tone="rose">{t("accessControl.revoked")}</Tag> : null}
            {deniedIds.length > 0 ? (
              <Tag tone="amber">{t("accessControl.deniedCount", { n: deniedIds.length })}</Tag>
            ) : null}
          </div>
          <p className="mt-0.5 font-mono text-xs text-faint">{connection.tokenPrefix}…</p>
        </div>
      </div>

      {error ? <p className="mt-2 text-xs text-danger">{error}</p> : null}

      <div className="mt-3 space-y-1.5 border-t border-line pt-3">
        {servers.map((server) => (
          <div key={server.id} className="flex items-center gap-3 rounded-md px-1 py-1.5">
            <IconChip icon={Server} tone={server.enabled ? "teal" : "slate"} size={28} />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm text-ink">{server.name}</span>
                {!server.enabled ? (
                  <Tag tone="slate">{t("accessControl.serverDisabled")}</Tag>
                ) : null}
              </div>
              <p className="font-mono text-xs text-faint">{server.slug}</p>
            </div>
            <Switch
              checked={!deniedIds.includes(server.id)}
              loading={pendingIds.has(server.id)}
              disabled={denied.isLoading || denied.isError}
              label={t(
                deniedIds.includes(server.id)
                  ? "accessControl.allowLabel"
                  : "accessControl.denyLabel",
                { connection: connection.clientConnectionId, server: server.name },
              )}
              onChange={(next) => setAccess.mutate({ serverId: server.id, allowed: next })}
            />
          </div>
        ))}
      </div>
    </div>
  );
}

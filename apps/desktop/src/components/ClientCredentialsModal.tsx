import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Cable, CheckCircle2, KeyRound, Plus, RotateCw, Trash2, X } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { type ClientTypeId, type HubStatus, ipc, type ToolExposureMode } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { errorText, shortConnectionId } from "../lib/utils";
import { DEFAULT_WS, useWorkspace } from "../lib/workspace";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./shadcn/select";
import {
  Button,
  CodeBlock,
  EmptyState,
  ErrorBanner,
  IconButton,
  IconChip,
  Modal,
  SegmentedControl,
  Switch,
  Tag,
} from "./ui";

/**
 * 凭证管理：单个客户端的 Token 发放/轮换/撤销与工作区绑定
 * （Server 级访问控制见 ClientAccessControlModal）。
 * 轮换联动同步（ADR-015 + ADR-016）：inline 形态改写客户端配置，env 形态
 * 重写环境变量存储；仅完全未接入时回退到一次性明文展示（Token 只在响应里
 * 出现一次，展示后不落任何本地状态存储）。
 */
export function ClientCredentialsModal({
  clientId,
  title,
  onClose,
}: {
  clientId: ClientTypeId;
  title: string;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { workspaces } = useWorkspace();
  const hub = useQuery({ queryKey: queryKeys.hubStatus, queryFn: ipc.hubStatus, retry: false });
  // id 为 null = 刚发放的新凭证（尚无连接 ID 可指代）。
  const [oneTimeToken, setOneTimeToken] = useState<{ id: string | null; token: string } | null>(
    null,
  );
  const [synced, setSynced] = useState<{ prefix: string; path: string } | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  // 凭证操作（发放/轮换/撤销/删除/绑定）失败时的内联报错。这些操作会改写
  // 客户端配置或签发/吊销凭据，静默失败会让用户误以为已生效——必须显性提示。
  const [actionError, setActionError] = useState<string | null>(null);

  // 轮换会联动改写客户端配置文件，连同接入状态一起失效重取。
  const refresh = () => {
    queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus });
    queryClient.invalidateQueries({ queryKey: queryKeys.clients });
    queryClient.invalidateQueries({ queryKey: queryKeys.mcpClients });
  };
  // 报错也要刷列表：回执可能在 Hub 已执行完之后才丢（Windows 上真发生过），
  // 不刷新的话用户看到「失败」而列表停在旧状态，会重复点到发出多个 Token。
  const onActionError = (error: unknown) => {
    setActionError(errorText(error));
    refresh();
  };

  const create = useMutation({
    mutationFn: () => {
      setActionError(null);
      return ipc.connectionCreate(title, clientId);
    },
    onSuccess: (result) => {
      if (result.ok && result.token) setOneTimeToken({ id: null, token: result.token });
      refresh();
    },
    onError: onActionError,
  });
  const rotate = useMutation({
    mutationFn: (id: string) => {
      setActionError(null);
      return ipc.connectionRotateSync(id, clientId);
    },
    onSuccess: (result, id) => {
      // Inline 形态改写配置文件；EnvVar 形态由 envsync 重写环境变量存储。
      const syncedPath = result.synced?.targetPath ?? result.envSynced?.credentialStore;
      if (syncedPath) {
        setOneTimeToken(null);
        setSynced({ prefix: result.tokenPrefix, path: syncedPath });
      } else if (result.token) {
        setSynced(null);
        setOneTimeToken({ id, token: result.token });
      }
      refresh();
    },
    onError: onActionError,
  });
  const setEnabled = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) => {
      setActionError(null);
      return ipc.connectionSetEnabled(id, enabled);
    },
    onSuccess: refresh,
    onError: onActionError,
  });
  // 工作区绑定会触发 Hub 侧全量重连下游（load_downstreams），逐行跟踪加载态。
  const [pendingWorkspaceIds, setPendingWorkspaceIds] = useState<Set<string>>(new Set());
  const setWorkspace = useMutation({
    mutationFn: ({ id, workspaceId }: { id: string; workspaceId: string | null }) =>
      ipc.connectionSetWorkspace(id, workspaceId),
    onMutate: ({ id }) => {
      setActionError(null);
      setPendingWorkspaceIds((prev) => new Set(prev).add(id));
    },
    onSettled: (_data, _error, { id }) =>
      setPendingWorkspaceIds((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      }),
    onSuccess: refresh,
    onError: onActionError,
  });
  // 暴露模式走乐观更新：SegmentedControl 高亮直接来自查询数据，不先翻转
  // 本地就会有一拍「点了没反应」的观感；失败用快照回滚并显性报错。
  const setExposure = useMutation({
    mutationFn: ({ id, mode }: { id: string; mode: ToolExposureMode }) =>
      ipc.connectionSetToolExposure(id, mode),
    onMutate: async ({ id, mode }) => {
      setActionError(null);
      await queryClient.cancelQueries({ queryKey: queryKeys.hubStatus });
      const previous = queryClient.getQueryData<HubStatus>(queryKeys.hubStatus);
      queryClient.setQueryData<HubStatus>(queryKeys.hubStatus, (data) =>
        data
          ? {
              ...data,
              connections: data.connections.map((item) =>
                item.clientConnectionId === id ? { ...item, toolExposureMode: mode } : item,
              ),
            }
          : data,
      );
      return { previous };
    },
    onError: (error, _variables, context) => {
      if (context?.previous) queryClient.setQueryData(queryKeys.hubStatus, context.previous);
      onActionError(error);
    },
    onSettled: () => queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus }),
  });
  const remove = useMutation({
    mutationFn: (id: string) => ipc.connectionDelete(id),
    onSuccess: () => {
      setConfirmDeleteId(null);
      refresh();
    },
    onError: (error) => {
      setConfirmDeleteId(null);
      onActionError(error);
    },
  });

  const connections = (hub.data?.connections ?? []).filter(
    (connection) => connection.clientType === clientId,
  );

  return (
    <Modal
      title={t("credentials.title", { title })}
      size="lg"
      onClose={onClose}
      footer={
        <>
          <p className="mr-auto text-xs text-faint">{t("credentials.workspaceHint")}</p>
          <Button
            size="sm"
            icon={Plus}
            loading={create.isPending}
            disabled={hub.isError || create.isPending}
            onClick={() => create.mutate()}
          >
            {t("credentials.issue")}
          </Button>
        </>
      }
    >
      <ErrorBanner
        message={actionError ? t("common.actionFailed", { error: actionError }) : null}
        onDismiss={() => setActionError(null)}
      />

      {synced ? (
        <div className="mb-4 flex items-start gap-2.5 rounded-lg border border-run-line bg-run-soft p-3.5">
          <CheckCircle2
            size={18}
            strokeWidth={2}
            aria-hidden
            className="mt-0.5 shrink-0 text-run"
          />
          <div className="min-w-0 flex-1">
            <p className="flex flex-wrap items-center gap-2 text-sm font-medium text-ink">
              {t("credentials.rotated")}
              <Tag tone="green">{t("credentials.synced")}</Tag>
            </p>
            <p className="mt-0.5 text-xs leading-5 text-muted">
              {t("credentials.syncedDesc", { prefix: synced.prefix })}
            </p>
            <code className="mt-0.5 block break-all font-mono text-xs text-muted">
              {synced.path}
            </code>
            <p className="mt-0.5 text-xs leading-5 text-muted">
              {t("credentials.syncedRestart", { title })}
            </p>
          </div>
          <IconButton
            icon={X}
            aria-label={t("credentials.dismiss")}
            onClick={() => setSynced(null)}
          />
        </div>
      ) : null}

      {oneTimeToken ? (
        <div className="mb-4 rounded-lg border border-warn-line bg-warn-soft p-4">
          <div className="flex flex-wrap items-center gap-2">
            <Tag tone="amber">{t("credentials.onceOnly")}</Tag>
            <p className="text-sm font-medium text-ink">
              {oneTimeToken.id
                ? t("credentials.newTokenFor", { id: shortConnectionId(oneTimeToken.id) })
                : t("credentials.newToken")}
            </p>
          </div>
          <p className="mt-1 text-xs leading-5 text-muted">{t("credentials.oneTimeDesc")}</p>
          <CodeBlock code={oneTimeToken.token} className="mt-2 border-warn-line bg-card" />
          <div className="mt-2.5 flex justify-end">
            <Button size="sm" onClick={() => setOneTimeToken(null)}>
              {t("credentials.savedClose")}
            </Button>
          </div>
        </div>
      ) : null}

      {hub.isError ? <p className="text-sm text-danger">{t("credentials.hubDown")}</p> : null}

      {hub.isSuccess && connections.length === 0 ? (
        <EmptyState title={t("credentials.empty", { title })} />
      ) : null}

      <div className="space-y-2.5">
        {connections.map((connection) => (
          <div key={connection.clientConnectionId} className="rounded-lg border border-line p-3.5">
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
                  {!connection.enabled ? <Tag tone="rose">{t("credentials.revoked")}</Tag> : null}
                </div>
                <p className="mt-0.5 font-mono text-xs text-faint">{connection.tokenPrefix}…</p>
              </div>
              <div className="flex shrink-0 items-center gap-2">
                {/* 「默认工作区」在后端是 null，这里必须借 DEFAULT_WS 当哨兵：
                    Radix 的 SelectItem 不接受空字符串（空串是它内部的「未选中」），
                    照搬原生 select 的 value="" 会直接抛错。 */}
                <Select
                  value={connection.defaultWorkspaceId ?? DEFAULT_WS}
                  onValueChange={(value) =>
                    setWorkspace.mutate({
                      id: connection.clientConnectionId,
                      workspaceId: value === DEFAULT_WS ? null : value,
                    })
                  }
                >
                  <SelectTrigger
                    size="sm"
                    aria-label={t("credentials.workspaceSelect")}
                    loading={pendingWorkspaceIds.has(connection.clientConnectionId)}
                  >
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={DEFAULT_WS}>{t("workspace.default")}</SelectItem>
                    {workspaces
                      .filter((workspace) => workspace.id !== DEFAULT_WS)
                      .map((workspace) => (
                        <SelectItem key={workspace.id} value={workspace.id}>
                          {workspace.name}
                        </SelectItem>
                      ))}
                  </SelectContent>
                </Select>
                <IconButton
                  icon={RotateCw}
                  aria-label={t("credentials.rotateAria", { id: connection.clientConnectionId })}
                  onClick={() => rotate.mutate(connection.clientConnectionId)}
                />
                <Switch
                  checked={connection.enabled}
                  onChange={(next) =>
                    setEnabled.mutate({ id: connection.clientConnectionId, enabled: next })
                  }
                  label={
                    connection.enabled
                      ? t("credentials.revokeAria", { id: connection.clientConnectionId })
                      : t("credentials.restoreAria", { id: connection.clientConnectionId })
                  }
                />
                <IconButton
                  icon={Trash2}
                  aria-label={t("credentials.deleteAria", { id: connection.clientConnectionId })}
                  onClick={() => setConfirmDeleteId(connection.clientConnectionId)}
                />
              </div>
            </div>
            <div className="mt-2.5 flex flex-wrap items-center justify-between gap-2 border-t border-line pt-2.5">
              <div className="min-w-0">
                <p className="text-xs font-medium text-ink">{t("credentials.exposureLabel")}</p>
                <p className="text-xs text-faint">{t("credentials.exposureHint")}</p>
              </div>
              <SegmentedControl
                label={t("credentials.exposureAria", { id: connection.clientConnectionId })}
                value={connection.toolExposureMode ?? "auto"}
                options={[
                  { value: "auto", label: t("credentials.exposureAuto") },
                  { value: "flat", label: t("credentials.exposureFlat") },
                  { value: "gateway", label: t("credentials.exposureGateway") },
                ]}
                onChange={(mode) => setExposure.mutate({ id: connection.clientConnectionId, mode })}
                disabled={setExposure.isPending}
              />
            </div>
          </div>
        ))}
      </div>

      {confirmDeleteId ? (
        <ConfirmDialog
          title={t("credentials.deleteTitle")}
          message={t("credentials.deleteMessage", { id: shortConnectionId(confirmDeleteId) })}
          confirmText={t("credentials.deleteConfirm")}
          onConfirm={() => remove.mutate(confirmDeleteId)}
          onCancel={() => setConfirmDeleteId(null)}
        />
      ) : null}
    </Modal>
  );
}

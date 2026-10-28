import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import {
  BookOpen,
  KeyRound,
  MoreHorizontal,
  Plus,
  RefreshCw,
  Shield,
  Trash2,
  Unplug,
} from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ClientAccessControlModal } from "../components/ClientAccessControlModal";
import { ClientChip } from "../components/ClientChip";
import { ClientCredentialsModal } from "../components/ClientCredentialsModal";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { ConnectCompareModal } from "../components/ConnectCompareModal";
import { CustomMcpClientModal } from "../components/CustomMcpClientModal";
import { HowToUseModal } from "../components/HowToUseModal";
import { PageHeader } from "../components/PageHeader";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "../components/shadcn/dropdown-menu";
import { Button, Card, ErrorBanner, IconButton } from "../components/ui";
import {
  type ClientTypeId,
  type ConnectOutcome,
  type ConnectPreview,
  type CredentialMode,
  ipc,
} from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { cn, errorText } from "../lib/utils";

interface ClientCommands {
  preview: (mode: CredentialMode) => Promise<ConnectPreview>;
  apply: (mode: CredentialMode) => Promise<ConnectOutcome>;
  /** 解绑成功后的非致命警告（如环境变量清理失败，Token 仍留在 shell 配置里）。 */
  disconnect: () => Promise<string[]>;
}

/**
 * 配置路径的短写：主目录缩成 `~`；仍然太长就省略中段、保留开头一段和最后两段。
 * 尾部截断会切掉最有辨识度的那截（`~/Library/Application Support/…` 看不出是谁的）。
 */
function tildePath(path: string): string {
  const short = path.replace(/^(\/Users\/[^/]+|\/home\/[^/]+|[A-Za-z]:\\Users\\[^\\]+)/, "~");
  const sep = short.includes("\\") ? "\\" : "/";
  const parts = short.split(sep);
  if (short.length <= 36 || parts.length <= 4) return short;
  return [parts[0], parts[1], "…", ...parts.slice(-2)].join(sep);
}

/**
 * 客户端行：图标 + 名称 + 版本 + 配置路径 + 一个主动作（连接，仅「可连接」组）+ 「…」菜单
 * （重新连接 / 使用说明 / 凭证 / 访问控制 / 断开或删除）。连接状态由所在分组表达。
 */
function ClientRow({
  id,
  title,
  installed,
  version,
  connected,
  configPath,
  tokenEnv,
  commands,
  onConfigChanged,
  custom,
  onRemove,
}: {
  id: ClientTypeId;
  title: string;
  installed: boolean;
  version?: string | null;
  connected: boolean;
  configPath: string;
  /** null 表示不支持环境变量展开（Cline）。 */
  tokenEnv: string | null;
  commands: ClientCommands;
  onConfigChanged: () => void;
  /**
   * 用户手动添加的客户端：MCPane 读不到也写不了它的配置文件，所以没有
   * 「接入/断开」——发一个 Token、把配置片段贴过去就是全部接入流程。
   */
  custom?: boolean;
  onRemove?: () => void;
}) {
  const { t } = useTranslation();
  const [modal, setModal] = useState<"compare" | "howto" | "credentials" | "access" | null>(null);
  const [preview, setPreview] = useState<ConnectPreview | null>(null);
  const [outcome, setOutcome] = useState<ConnectOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  /**
   * 解绑成功但清理有遗留（如 shell 里的 Token 环境变量删不掉）：解绑本身没失败，
   * 用告警色而非 error 的红色——红字会让人以为整个解绑都没成。
   */
  const [warning, setWarning] = useState<string | null>(null);
  /**
   * 凭据模式（两种都一键完成）：env 默认；Cline 不支持环境变量展开，固定 inline。
   * 默认值从 tokenEnv 派生而非 useState 初始化——卡片可能在客户端检测查询
   * 返回前挂载（此时 tokenEnv 尚为 null），快照进 state 会把默认错定成 inline。
   */
  const [modeOverride, setModeOverride] = useState<CredentialMode | null>(null);
  const mode = modeOverride ?? (tokenEnv ? "env" : "inline");

  const loadPreview = useMutation({
    mutationFn: (nextMode: CredentialMode) => commands.preview(nextMode),
    onSuccess: (data) => {
      setPreview(data);
      setOutcome(null);
      setError(null);
      setWarning(null);
      setModal("compare");
    },
    onError: (mutationError) => setError(errorText(mutationError)),
  });
  const apply = useMutation({
    mutationFn: () => commands.apply(mode),
    onSuccess: (data) => {
      setOutcome(data);
      setError(null);
      setWarning(null);
      onConfigChanged();
    },
    onError: (mutationError) => setError(errorText(mutationError)),
  });
  const switchMode = (nextMode: CredentialMode) => {
    setModeOverride(nextMode);
    loadPreview.mutate(nextMode);
  };
  // 断开会改写客户端配置文件并清理 shell 里的 Token 环境变量，与凭证删除、
  // 服务器删除同级，一样过一道确认——此前是唯一一个一点即执行的破坏性操作。
  const [confirmDisconnect, setConfirmDisconnect] = useState(false);
  const disconnect = useMutation({
    mutationFn: commands.disconnect,
    onSuccess: (warnings) => {
      // 解绑成功但清理有遗留（如 shell 里的 Token 环境变量删不掉）：
      // 必须展示——那是一个仍然有效的明文凭据。但用告警色，别用 error 的红：
      // 解绑本身成功了。
      setError(null);
      setWarning(warnings.length ? warnings.join(" · ") : null);
      onConfigChanged();
    },
    onError: (mutationError) => {
      setWarning(null);
      setError(errorText(mutationError));
    },
  });

  const closeCompare = () => {
    setModal(null);
    setPreview(null);
    setOutcome(null);
  };

  return (
    <div
      data-client={id}
      // 导览指第一行讲"状态怎么看"。Claude Code 恒在列表首位（CLIENTS 顺序固定）。
      data-tour={id === "claude" ? "clients-card" : undefined}
      className="flex items-center gap-3 px-4 py-2.5"
    >
      <ClientChip id={id} title={title} installed={installed || connected} size={28} />

      <div className="min-w-0 flex-1">
        {/* 名称 / 版本 / 配置路径三列对齐：连接状态由所在分组说明，行里不再重复徽标。 */}
        <div className="flex min-w-0 items-center gap-4">
          <h3
            className={cn(
              "w-40 shrink-0 truncate font-medium text-[13px]",
              installed || connected || custom ? "text-ink" : "text-muted",
            )}
          >
            {title}
          </h3>
          <span className="w-20 shrink-0 truncate text-xs text-muted tabular-nums">
            {/* 装没装由分组说明，这里只报版本；探测不到版本就空着，不写「v?」。 */}
            {custom ? t("clients.customBadge") : version ? `v${version}` : ""}
          </span>
          {/* 「连接」写到哪儿：以前只在预览弹窗里看得到。 */}
          <span
            className="min-w-0 flex-1 truncate font-mono text-[11px] text-faint"
            title={custom ? undefined : configPath}
          >
            {custom ? t("clients.descCustom") : configPath ? tildePath(configPath) : ""}
          </span>
        </div>
        {error ? <p className="mt-1 text-xs text-danger">{error}</p> : null}
        {warning ? <p className="mt-1 text-xs text-warn">{warning}</p> : null}
      </div>

      <div
        data-tour={id === "claude" ? "clients-connect" : undefined}
        className="flex shrink-0 items-center gap-1"
      >
        {/* 只有「可连接」这一组有主按钮：已连接的「重新连接」在「…」里，
            没检测到的不挂一颗点不动的按钮。 */}
        {!custom && !connected && installed ? (
          <Button
            size="sm"
            variant="primary"
            disabled={loadPreview.isPending}
            onClick={() => loadPreview.mutate(mode)}
          >
            {loadPreview.isPending ? t("clients.loadingPreview") : t("clients.connect")}
          </Button>
        ) : null}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <IconButton
              icon={MoreHorizontal}
              data-tour={id === "claude" ? "clients-actions" : undefined}
              aria-label={t("clients.moreActions", { name: title })}
              loading={disconnect.isPending}
            />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            {connected ? (
              <DropdownMenuItem
                disabled={loadPreview.isPending}
                onSelect={() => loadPreview.mutate(mode)}
              >
                <RefreshCw />
                {t("clients.reconnect")}
              </DropdownMenuItem>
            ) : null}
            <DropdownMenuItem onSelect={() => setModal("howto")}>
              <BookOpen />
              {t("clients.howTo")}
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => setModal("credentials")}>
              <KeyRound />
              {t("clients.credentials")}
            </DropdownMenuItem>
            <DropdownMenuItem onSelect={() => setModal("access")}>
              <Shield />
              {t("clients.accessControl")}
            </DropdownMenuItem>
            {custom ? (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem variant="destructive" onSelect={onRemove}>
                  <Trash2 />
                  {t("clients.customRemove")}
                </DropdownMenuItem>
              </>
            ) : connected ? (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem variant="destructive" onSelect={() => setConfirmDisconnect(true)}>
                  <Unplug />
                  {t("clients.disconnect")}
                </DropdownMenuItem>
              </>
            ) : null}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      {modal === "compare" && preview ? (
        <ConnectCompareModal
          title={title}
          preview={preview}
          outcome={outcome}
          error={error}
          applying={apply.isPending}
          reloadingPreview={loadPreview.isPending}
          envSupported={tokenEnv !== null}
          onApply={() => apply.mutate()}
          onSwitchMode={switchMode}
          onClose={closeCompare}
        />
      ) : null}
      {modal === "howto" ? (
        <HowToUseModal
          clientId={id}
          title={title}
          configPath={configPath}
          tokenEnv={tokenEnv}
          connected={connected}
          custom={custom}
          onClose={() => setModal(null)}
        />
      ) : null}
      {modal === "credentials" ? (
        <ClientCredentialsModal clientId={id} title={title} onClose={() => setModal(null)} />
      ) : null}
      {modal === "access" ? (
        <ClientAccessControlModal clientId={id} title={title} onClose={() => setModal(null)} />
      ) : null}
      {confirmDisconnect ? (
        <ConfirmDialog
          title={t("clients.disconnectConfirmTitle")}
          message={t("clients.disconnectConfirmMessage", { name: title })}
          confirmText={t("clients.disconnect")}
          onCancel={() => setConfirmDisconnect(false)}
          onConfirm={() => {
            setConfirmDisconnect(false);
            disconnect.mutate();
          }}
        />
      ) : null}
    </div>
  );
}

/**
 * Claude Code / Codex 的专用命令；其余客户端走通用命令。
 * 命令名由 prefix 拼出，没法收进 lib/ipc.ts 的具名封装，故集中在这里一处。
 */
function dedicatedCommands(prefix: "claude" | "codex"): ClientCommands {
  return {
    preview: (mode) => invoke<ConnectPreview>(`${prefix}_connect_preview`, { mode }),
    apply: (mode) => invoke<ConnectOutcome>(`${prefix}_connect_apply`, { mode }),
    disconnect: () => invoke<string[]>(`${prefix}_disconnect`),
  };
}

function genericCommands(client: string): ClientCommands {
  return {
    preview: (mode) => ipc.mcpClientConnectPreview(client, mode),
    apply: (mode) => ipc.mcpClientConnectApply(client, mode),
    disconnect: () => ipc.mcpClientDisconnect(client),
  };
}

export function Clients() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<{ id: string; title: string } | null>(null);
  const clients = useQuery({
    queryKey: queryKeys.clients,
    queryFn: ipc.detectClients,
    retry: false,
  });
  const extraClients = useQuery({
    queryKey: queryKeys.mcpClients,
    queryFn: ipc.mcpClientsStatus,
    retry: false,
  });

  // 接入/断开会改写客户端配置文件，且 inline 接入会自动创建/轮换 Hub 连接，
  // 让状态查询与连接列表一并失效重取。
  // 删除会吊销该客户端名下已发出的全部 Token：静默失败会让用户以为凭据已撤销，
  // 实际那些 Token 还在生效——这类操作必须显性报错（同 ClientCredentialsModal）。
  const [removeError, setRemoveError] = useState<string | null>(null);
  const removeCustom = useMutation({
    mutationFn: (id: string) => ipc.customMcpClientRemove(id),
    onSuccess: () => {
      setRemoveError(null);
      // 卡片没了，它名下的 Token 也一并吊销了：连接列表同样要重取。
      queryClient.invalidateQueries({ queryKey: queryKeys.mcpClients });
      queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus });
    },
    onError: (error) => setRemoveError(errorText(error)),
  });

  const onConfigChanged = () => {
    queryClient.invalidateQueries({ queryKey: queryKeys.clients });
    queryClient.invalidateQueries({ queryKey: queryKeys.mcpClients });
    queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus });
  };

  const dedicated = [
    ["claude", "Claude Code", clients.data?.claude] as const,
    ["codex", "Codex", clients.data?.codex] as const,
  ].map(([id, title, status]) => ({
    id,
    title,
    installed: status?.installed ?? false,
    version: status?.version ?? null,
    connected: status?.connected ?? false,
    configPath: status?.configPath ?? "",
    tokenEnv: status?.tokenEnv ?? null,
    commands: dedicatedCommands(id),
    custom: false,
  }));

  const generic = (extraClients.data ?? []).map((client) => ({
    id: client.id as ClientTypeId,
    title: client.display,
    installed: client.installed,
    version: null,
    connected: client.connected,
    configPath: client.configPath,
    tokenEnv: client.tokenEnv,
    commands: genericCommands(client.id),
    custom: client.custom,
    onRemove: client.custom
      ? () => setRemoving({ id: client.id, title: client.display })
      : undefined,
  }));

  // 按连接状态分组：已连接 → 可连接 → 自定义客户端 → 未检测到（置灰但照样列出）。
  // 组内保持 CLIENTS 原顺序。连着但探测不到安装痕迹的也算已连接——连接是事实。
  const cards = [...dedicated, ...generic];
  const groups = (
    [
      ["connected", cards.filter((c) => !c.custom && c.connected)],
      ["available", cards.filter((c) => !c.custom && !c.connected && c.installed)],
      ["custom", cards.filter((c) => c.custom)],
      ["missing", cards.filter((c) => !c.custom && !c.connected && !c.installed)],
    ] as const
  ).filter(([, rows]) => rows.length > 0);
  const detecting = clients.isLoading || extraClients.isLoading;

  // 检测失败时卡片仍然渲染（探测结果只标灰不隐藏），但必须说明「未安装/未接入」
  // 这些状态此刻不可信——两个查询都是 retry:false，一次失败就定格，
  // 否则用户看到的是一句平静且错误的结论。
  const detectError = clients.isError
    ? t("clients.detectError", { error: errorText(clients.error) })
    : extraClients.isError
      ? t("clients.detectError", { error: errorText(extraClients.error) })
      : null;

  return (
    <div>
      {/* 与服务器页同一形制：标题与页面级操作同一行，下面一行短说明。
          曾另有「AI 编程客户端」小节标题——整页只有这一节，等于把标题说了两遍。 */}
      <PageHeader
        title={t("clients.title")}
        actions={
          // 未内置的 MCP 客户端也能接入：添加后可签发 Token、配置访问控制。
          <Button icon={Plus} onClick={() => setAdding(true)}>
            {t("clients.customAdd")}
          </Button>
        }
      />
      <p className="mb-4 -mt-1 text-xs text-muted">{t("clients.sectionDesc")}</p>

      <ErrorBanner message={detectError} />
      <ErrorBanner message={removeError} />

      {/* 页面按连接状态组织：分组标题一句话说明全局（连了几个、还能连几个），
          行里只留名称 / 版本 / 配置路径三列，主按钮只出现在「可以连接」一组、排成一条线。 */}
      {detecting ? (
        <Card className="px-4 py-6 text-center text-xs text-faint">{t("clients.detecting")}</Card>
      ) : (
        groups.map(([key, rows]) => (
          <section key={key} aria-labelledby={`clients-group-${key}`} className="mb-5">
            <h3
              id={`clients-group-${key}`}
              className="mb-2 flex items-center gap-2 px-1 font-semibold text-[13px] text-ink"
            >
              {key === "connected" ? (
                <span aria-hidden className="h-2 w-2 rounded-full bg-run-dot" />
              ) : null}
              {t(`clients.group.${key}`)}
              <span className="font-normal text-faint text-xs tabular-nums">{rows.length}</span>
            </h3>
            <Card className="divide-y divide-line">
              {rows.map((card) => (
                <ClientRow key={card.id} {...card} onConfigChanged={onConfigChanged} />
              ))}
            </Card>
          </section>
        ))
      )}

      {adding ? (
        <CustomMcpClientModal
          onClose={() => setAdding(false)}
          onSaved={() => {
            queryClient.invalidateQueries({ queryKey: queryKeys.mcpClients });
            setAdding(false);
          }}
        />
      ) : null}

      {removing ? (
        <ConfirmDialog
          title={t("clients.customRemoveTitle")}
          message={t("clients.customRemoveMessage", { name: removing.title })}
          confirmText={t("clients.customRemove")}
          onCancel={() => setRemoving(null)}
          onConfirm={() => {
            removeCustom.mutate(removing.id);
            setRemoving(null);
          }}
        />
      ) : null}
    </div>
  );
}

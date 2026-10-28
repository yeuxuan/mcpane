import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Loader2, Plus, RotateCcw } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { PageHeader } from "../components/PageHeader";
import { ToolAnnotationsModal } from "../components/ToolAnnotationsModal";
import {
  Button,
  Card,
  CopyButton,
  ErrorBanner,
  Field,
  SearchInput,
  Switch,
  Tag,
  Textarea,
} from "../components/ui";
import { type CatalogTool, type DeclaredAnnotations, ipc } from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { cn, errorText } from "../lib/utils";
import { DEFAULT_WS, useWorkspace } from "../lib/workspace";

// 工具全量下发超过这个量级（几十个 schema ≈ 上万 token）就提示一键开启
// 发现模式；只提示不自动切换——tools/list 的行为变化必须用户知情。
const DISCOVERY_HINT_THRESHOLD = 30;

/**
 * Workspace 管理（TASK-504）。默认 Workspace 含全部启用 Server；
 * 自建 Workspace 按成员集过滤；切换对新会话生效（Session-Pinned，ADR-004）。
 * 当前工作区由顶右切换器统一驱动（全局上下文）。
 */
export function Workspaces() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const { selectedId: selected, setSelectedId: setSelected, workspaces } = useWorkspace();
  const [newName, setNewName] = useState("");
  const [search, setSearch] = useState("");
  const [aliasDraft, setAliasDraft] = useState<Record<string, string>>({});
  // 触发提示：展开式编辑（一句话文本，塞进工具行的窄输入框读不了）。
  const [suffixOpen, setSuffixOpen] = useState<string | null>(null);
  const [suffixDraft, setSuffixDraft] = useState<Record<string, string>>({});
  // selected 是变更触发器，不是 effect 体里被读取的值。biome 只看数据流，
  // 建议删成 []——那样它只在挂载时跑一次，上一个工作区的 alias 草稿会残留
  // 到下一个。清空草稿正是这个 effect 存在的理由，依赖必须留着。
  // biome-ignore lint/correctness/useExhaustiveDependencies: 依赖是变更触发器，删掉会让草稿跨工作区残留
  useEffect(() => {
    setAliasDraft({});
    setSuffixDraft({});
    setSuffixOpen(null);
  }, [selected]);
  // 建工作区 / 成员增删 / 工具开关失败的内联报错（都会触发 Hub 重连，静默失败会误导）。
  const [actionError, setActionError] = useState<string | null>(null);
  // 报错之外还要刷新：命令层会在「写库成功但 Hub 未生效」时上抛——此时
  // DB 已是新值，UI 停在动作前的旧状态就是在撒谎，拉一次真值收敛。
  const onActionError = (error: unknown) => {
    setActionError(errorText(error));
    refreshAll();
  };

  const workspaceArg = selected === DEFAULT_WS ? null : selected;
  const catalog = useQuery({
    queryKey: queryKeys.catalog(selected),
    queryFn: () => ipc.catalogList(workspaceArg),
    ...livePoll(10000),
  });
  const overrides = useQuery({
    queryKey: queryKeys.toolOverrides(selected),
    queryFn: () => ipc.workspaceToolOverrides(workspaceArg),
    retry: false,
  });
  const servers = useQuery({
    queryKey: queryKeys.servers,
    queryFn: ipc.serversList,
    retry: false,
  });
  const members = useQuery({
    queryKey: queryKeys.workspaceServers(selected),
    queryFn: () => ipc.workspaceServers(selected),
    enabled: selected !== DEFAULT_WS,
    retry: false,
  });

  const refreshAll = () => {
    queryClient.invalidateQueries({ queryKey: queryKeys.catalog(selected) });
    queryClient.invalidateQueries({ queryKey: queryKeys.toolOverrides(selected) });
    queryClient.invalidateQueries({ queryKey: queryKeys.workspaceServers(selected) });
    queryClient.invalidateQueries({ queryKey: queryKeys.workspaces });
  };

  const createWorkspace = useMutation({
    mutationFn: (name: string) => {
      setActionError(null);
      return ipc.workspaceCreate(name);
    },
    onSuccess: (workspace) => {
      setNewName("");
      setSelected(workspace.id);
      refreshAll();
    },
    onError: onActionError,
  });
  // workspace_server_set / workspace_tool_toggle 都会同步等 Hub 侧 ReloadServers
  // 把下游服务器重连一遍（跟「MCP 服务器」页的启停同一套耗时逻辑），
  // 用本地 Set 而非 mutation.isPending 逐项跟踪，避免后点的覆盖先点的加载态。
  const [pendingMembers, setPendingMembers] = useState<Set<string>>(new Set());
  const setMember = useMutation({
    mutationFn: (input: { serverId: string; enabled: boolean }) =>
      ipc.workspaceServerSet(selected, input.serverId, input.enabled),
    onMutate: ({ serverId }) => {
      setActionError(null);
      setPendingMembers((prev) => new Set(prev).add(serverId));
    },
    onSettled: (_data, _error, { serverId }) =>
      setPendingMembers((prev) => {
        const next = new Set(prev);
        next.delete(serverId);
        return next;
      }),
    onSuccess: refreshAll,
    onError: onActionError,
  });
  // 工具动态发现开关（默认工作区不在 workspaces 列表里，单独查询）。
  const discovery = useQuery({
    queryKey: queryKeys.toolDiscovery(selected),
    queryFn: () => ipc.workspaceToolDiscovery(workspaceArg),
    retry: false,
  });
  const setDiscovery = useMutation({
    mutationFn: (enabled: boolean) => ipc.workspaceToolDiscoverySet(workspaceArg, enabled),
    onMutate: () => setActionError(null),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.toolDiscovery(selected) });
      refreshAll();
    },
    onError: onActionError,
  });
  // 使用准则：注入 instructions 让所有客户端共享（ADR-020 主动性轴）。
  // 草稿记住所属工作区：切换后归属不匹配即视为未编辑（回显查询值），
  // A 工作区的未保存草稿不会串给 B。
  const guidance = useQuery({
    queryKey: queryKeys.usageGuidance(selected),
    queryFn: () => ipc.workspaceUsageGuidance(workspaceArg),
    retry: false,
  });
  const [guidanceDraft, setGuidanceDraft] = useState<{ workspace: string; text: string } | null>(
    null,
  );
  const guidanceDraftText = guidanceDraft?.workspace === selected ? guidanceDraft.text : null;
  const setGuidance = useMutation({
    mutationFn: (text: string) => ipc.workspaceUsageGuidanceSet(workspaceArg, text),
    onMutate: () => setActionError(null),
    onSuccess: () => {
      setGuidanceDraft(null);
      queryClient.invalidateQueries({ queryKey: queryKeys.usageGuidance(selected) });
      queryClient.invalidateQueries({ queryKey: queryKeys.guidancePreview(selected) });
      queryClient.invalidateQueries({ queryKey: queryKeys.guidanceTargets(selected) });
    },
    onError: onActionError,
  });
  const guidanceValue = guidanceDraftText ?? guidance.data ?? "";
  const guidanceDirty = guidanceDraftText !== null && guidanceDraftText !== (guidance.data ?? "");

  // 规则文件投递：同一份准则的第二条通道。逐个客户端勾选，不做一键铺全部——
  // 写的是用户自己的 CLAUDE.md/AGENTS.md，得他知道每一次写到哪儿。
  const deliverTargets = useQuery({
    queryKey: queryKeys.guidanceTargets(selected),
    queryFn: () => ipc.guidanceTargets(workspaceArg),
    retry: false,
  });
  const [pendingDeliver, setPendingDeliver] = useState<string | null>(null);
  const [stepsOpen, setStepsOpen] = useState<string | null>(null);
  // 手动投递复制的是「可直接粘贴的文档」（带小标题、不带托管标记），
  // 与自动投递写进托管区块的内容分开取。
  const deliverPreview = useQuery({
    queryKey: queryKeys.guidancePreview(selected),
    queryFn: () => ipc.guidancePreview(workspaceArg),
    retry: false,
  });
  const deliver = useMutation({
    mutationFn: ({ clientType, on }: { clientType: string; on: boolean }) =>
      on ? ipc.guidanceDeliver(clientType, workspaceArg) : ipc.guidanceRevoke(clientType),
    onMutate: ({ clientType }) => {
      setActionError(null);
      setPendingDeliver(clientType);
    },
    onSettled: () => setPendingDeliver(null),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: queryKeys.guidanceTargets(selected) }),
    onError: onActionError,
  });

  // 编辑框回显已保存的提示——工具行显示的描述已经带上它了，编辑框却空着的话，
  // 用户只会以为没存上、再写一遍。草稿优先，没编辑过就读库里的值。
  const savedSuffix = (serverId: string, originalName: string) =>
    (overrides.data ?? []).find((o) => o.serverId === serverId && o.originalName === originalName)
      ?.descriptionSuffix ?? "";

  const toolKey = (serverId: string, originalName: string) => `${serverId}:${originalName}`;
  const [pendingTools, setPendingTools] = useState<Set<string>>(new Set());
  const toggle = useMutation({
    mutationFn: (input: {
      serverId: string;
      originalName: string;
      enabled: boolean;
      alias?: string | null;
      descriptionSuffix?: string | null;
    }) => ipc.workspaceToolToggle({ workspaceId: workspaceArg, ...input }),
    onMutate: ({ serverId, originalName }) => {
      setActionError(null);
      setPendingTools((prev) => new Set(prev).add(toolKey(serverId, originalName)));
    },
    onSettled: (_data, _error, { serverId, originalName }) =>
      setPendingTools((prev) => {
        const next = new Set(prev);
        next.delete(toolKey(serverId, originalName));
        return next;
      }),
    onSuccess: refreshAll,
    onError: onActionError,
  });

  // 正在编辑注解的工具（null = 弹窗未开）。存整条 tool 而非 id：弹窗要同时
  // 读下游申报值与用户声明值，拿 id 再去列表里捞一遍是白绕一圈。
  const [annotating, setAnnotating] = useState<CatalogTool | null>(null);
  const setAnnotations = useMutation({
    mutationFn: (input: { tool: CatalogTool; annotations: DeclaredAnnotations | null }) =>
      ipc.toolAnnotationsSet({
        serverId: input.tool.serverId,
        originalName: input.tool.originalName,
        annotations: input.annotations,
      }),
    onMutate: () => setActionError(null),
    onSuccess: () => {
      setAnnotating(null);
      refreshAll();
    },
    onError: onActionError,
  });

  // 检索词统一小写后再比（与 Servers / Providers 页同口径）：工具名与 slug 恒为
  // 小写，大写关键词直接比会一条都搜不到。
  const toolQuery = search.trim().toLowerCase();
  const tools = (catalog.data?.tools ?? []).filter(
    (tool) =>
      !toolQuery ||
      tool.exposedName.toLowerCase().includes(toolQuery) ||
      tool.serverSlug.toLowerCase().includes(toolQuery) ||
      (tool.description ?? "").toLowerCase().includes(toolQuery),
  );
  const disabled = (overrides.data ?? []).filter((item) => !item.enabled);
  // 禁用工具行不在 Catalog 里，只有 serverId：借服务器列表换成名称 + slug，
  // 与上方启用工具行同一种呈现（此前直接铺 serverId，对用户是天书）。
  const serverById = new Map((servers.data ?? []).map((s) => [s.id, s]));
  const memberSet = new Set(members.data ?? []);
  // Hub 不可达时这些工具行仍会用上一次成功的缓存渲染出来，但此刻的写操作
  // 只会落库、不会下发到 Hub——命令层写库成功就返回 Ok，通知 Hub 失败只打
  // 日志，前端 onError 永远不触发。不置灰的话，用户看到的是"开关翻转 +
  // 转圈结束"，与真正生效毫无差别，而下游客户端仍按旧配置在调这个工具。
  const hubDown = catalog.isError;
  // 用未过滤的 catalog 总数判断（tools 已被搜索词过滤，会低估）。
  const toolCount = catalog.data?.tools?.length ?? 0;
  const suggestDiscovery =
    !hubDown && discovery.data === false && toolCount >= DISCOVERY_HINT_THRESHOLD;
  // 既没下游申报、用户也没声明的工具数。这类工具在 headless Codex 下会被
  // 直接取消调用（openai/codex#16685），值得在页顶说一次它是什么。
  const unannotatedCount = (catalog.data?.tools ?? []).filter(
    (tool) => !tool.annotations && !tool.declaredAnnotations,
  ).length;

  return (
    <div>
      <PageHeader
        title={t("workspaces.title")}
        actions={
          <Tag>
            {t("workspaces.revision")} <span className="font-mono">{catalog.data?.id ?? "-"}</span>
          </Tag>
        }
      />

      {/* 与 MCP 服务器页同一句话：工具治理是本页的主要动作，而它和那边一样
          只对新会话生效（Session-Pinned，见文件头注释）。此前这条语义只写在
          代码注释里，用户在界面上看不到。 */}
      <p className="mb-4 -mt-1 text-xs text-muted">{t("common.newSessionHint")}</p>

      <ErrorBanner
        message={actionError ? t("common.actionFailed", { error: actionError }) : null}
        onDismiss={() => setActionError(null)}
      />

      <Card className="mb-5 flex flex-wrap items-center gap-3 p-4">
        <div className="flex items-center gap-2">
          <span className="text-xs text-muted">{t("workspaces.current")}</span>
          <span className="rounded-lg bg-accent-soft px-2.5 py-1 text-[13px] font-medium text-ink">
            {selected === DEFAULT_WS
              ? t("workspaces.defaultAll")
              : (workspaces.find((w) => w.id === selected)?.name ?? t("workspace.default"))}
          </span>
          <span className="text-xs text-faint">{t("workspaces.switchHint")}</span>
        </div>
        <span className="mx-1 h-5 w-px bg-line" aria-hidden />
        <Field
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          placeholder={t("workspaces.newName")}
          className="w-44"
        />
        <Button
          icon={Plus}
          data-tour="workspaces-create"
          disabled={!newName.trim()}
          loading={createWorkspace.isPending}
          onClick={() => createWorkspace.mutate(newName.trim())}
        >
          {t("workspaces.create")}
        </Button>
        <span className="mx-1 h-5 w-px bg-line" aria-hidden />
        <span
          data-tour="workspaces-discovery"
          className="flex items-center gap-2 text-xs text-muted"
          title={t("workspaces.toolDiscoveryHint")}
        >
          {t("workspaces.toolDiscovery")}
          <Switch
            checked={discovery.data ?? false}
            disabled={hubDown}
            loading={setDiscovery.isPending || discovery.isLoading}
            label={t("workspaces.toolDiscovery")}
            onChange={(next) => setDiscovery.mutate(next)}
          />
        </span>
      </Card>
      {discovery.data ? (
        <p className="-mt-3 mb-5 text-[11px] text-faint">{t("workspaces.toolDiscoveryOn")}</p>
      ) : suggestDiscovery ? (
        <p className="-mt-3 mb-5 flex flex-wrap items-center gap-2 text-xs text-muted">
          {t("workspaces.discoverySuggest", { count: toolCount })}
          <Button
            size="sm"
            variant="ghost"
            loading={setDiscovery.isPending}
            onClick={() => setDiscovery.mutate(true)}
          >
            {t("workspaces.discoverySuggestAction")}
          </Button>
        </p>
      ) : null}

      <Card className="mb-5 p-5">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0">
            <p className="text-sm font-medium text-ink">{t("workspaces.guidanceTitle")}</p>
            <p className="mt-0.5 text-xs text-faint">{t("workspaces.guidanceHint")}</p>
          </div>
          <Button
            size="sm"
            loading={setGuidance.isPending}
            disabled={hubDown || !guidanceDirty || setGuidance.isPending}
            onClick={() => setGuidance.mutate(guidanceValue)}
          >
            {t("workspaces.guidanceSave")}
          </Button>
        </div>
        <Textarea
          className="mt-2.5"
          rows={3}
          value={guidanceValue}
          placeholder={t("workspaces.guidancePlaceholder")}
          disabled={hubDown || guidance.isLoading || setGuidance.isPending}
          aria-label={t("workspaces.guidanceTitle")}
          onChange={(e) => setGuidanceDraft({ workspace: selected, text: e.target.value })}
        />

        <div className="mt-4 border-line border-t pt-4">
          <p className="text-sm font-medium text-ink">{t("workspaces.deliverTitle")}</p>
          <p className="mt-0.5 text-xs text-faint">{t("workspaces.deliverHint")}</p>
          <div className="mt-3 space-y-2">
            {(deliverTargets.data ?? []).map((target) => (
              <div
                key={target.clientType}
                className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-line px-3 py-2"
              >
                <div className="min-w-0">
                  <p className="text-sm text-ink">{target.label}</p>
                  <p className="mt-0.5 truncate font-mono text-xs text-faint">
                    {target.path ?? t("workspaces.deliverUnsupported")}
                  </p>
                </div>
                {target.supported ? (
                  <div className="flex shrink-0 items-center gap-2">
                    <span className={cn("text-xs", target.stale ? "text-warn" : "text-muted")}>
                      {target.delivered
                        ? target.stale
                          ? t("workspaces.deliverStale")
                          : t("workspaces.deliverOn")
                        : t("workspaces.deliverOff")}
                    </span>
                    <Switch
                      checked={target.delivered}
                      loading={pendingDeliver === target.clientType}
                      disabled={
                        pendingDeliver === target.clientType ||
                        !!target.error ||
                        (!target.delivered && !(guidance.data ?? "").trim())
                      }
                      onChange={(on) => deliver.mutate({ clientType: target.clientType, on })}
                      label={t("workspaces.deliverAction", { label: target.label })}
                    />
                  </div>
                ) : (
                  // 自动投递不了的客户端照样列出来：静默隐藏只会让用户以为
                  // MCPane 漏了它。展开就是照着做的步骤，外加一键复制——
                  // 光给一句「不支持」等于把人丢在半路。
                  <div className="flex shrink-0 items-center gap-2">
                    <CopyButton
                      value={deliverPreview.data ?? ""}
                      label={t("workspaces.deliverCopy")}
                    />
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() =>
                        setStepsOpen((open) =>
                          open === target.clientType ? null : target.clientType,
                        )
                      }
                    >
                      {stepsOpen === target.clientType
                        ? t("workspaces.deliverStepsHide")
                        : t("workspaces.deliverSteps")}
                    </Button>
                  </div>
                )}
                {target.unsupportedReason ? (
                  <p className="w-full text-xs text-faint">{target.unsupportedReason}</p>
                ) : null}
                {/* 落点没实测过就得说出来：勾完开关却什么也没生效，
                    是最难自查的那种失败。 */}
                {target.supported && !target.pathVerified ? (
                  <p className="w-full text-warn text-xs">{t("workspaces.deliverUnverified")}</p>
                ) : null}
                {stepsOpen === target.clientType && target.steps.length > 0 ? (
                  <ol className="w-full list-decimal space-y-1.5 rounded-lg bg-hover py-2.5 pr-3 pl-7 text-xs text-muted">
                    {target.steps.map((step) => (
                      <li key={step}>{step}</li>
                    ))}
                  </ol>
                ) : null}
                {target.error ? <p className="w-full text-danger text-xs">{target.error}</p> : null}
              </div>
            ))}
          </div>
          {!(guidance.data ?? "").trim() ? (
            <p className="mt-2 text-faint text-xs">{t("workspaces.deliverEmpty")}</p>
          ) : null}
        </div>
      </Card>

      {selected !== DEFAULT_WS ? (
        <Card className="mb-5 p-5">
          <p className="mb-3 text-sm font-medium text-ink">{t("workspaces.members")}</p>
          <div className="flex flex-wrap gap-2">
            {(servers.data ?? [])
              .filter((s) => s.enabled)
              .map((s) => {
                const on = memberSet.has(s.id);
                const loading = pendingMembers.has(s.id);
                return (
                  <label
                    key={s.id}
                    aria-busy={loading}
                    className={`flex cursor-pointer items-center gap-2 rounded-lg border px-3 py-1.5 text-sm transition-colors ${
                      on
                        ? "border-accent bg-accent-soft text-ink"
                        : "border-line-strong text-muted hover:bg-hover"
                    } ${loading ? "opacity-70" : ""}`}
                  >
                    <input
                      type="checkbox"
                      className="accent-accent"
                      checked={on}
                      disabled={loading || hubDown}
                      onChange={(e) =>
                        setMember.mutate({ serverId: s.id, enabled: e.target.checked })
                      }
                    />
                    {s.name}
                    {loading ? (
                      <Loader2 size={12} strokeWidth={2.5} aria-hidden className="animate-spin" />
                    ) : null}
                  </label>
                );
              })}
            {servers.data?.filter((s) => s.enabled).length === 0 ? (
              <span className="text-xs text-muted">{t("workspaces.membersEmpty")}</span>
            ) : null}
          </div>
        </Card>
      ) : null}

      {unannotatedCount > 0 ? (
        <p className="mb-4 text-xs text-muted">
          {t("workspaces.unannotatedHint", { count: unannotatedCount })}
        </p>
      ) : null}

      <SearchInput
        data-tour="workspaces-tools"
        className="mb-4 max-w-sm"
        placeholder={t("workspaces.searchTools")}
        value={search}
        onChange={(e) => setSearch(e.target.value)}
      />

      {catalog.isError ? <p className="text-sm text-danger">{t("workspaces.hubDown")}</p> : null}
      {catalog.isLoading ? (
        <p className="text-sm text-muted">{t("workspaces.loadingTools")}</p>
      ) : null}
      {catalog.isSuccess && tools.length === 0 ? (
        <p className="text-sm text-muted">
          {/* 整句而非拼接：中文的「——先…」与英文的 " — …" 断句方式不同，
              拼出来的句子在任一语言里都别扭。 */}
          {/* tools 是过滤后的结果，故「搜不到」与「本来就没有」会撞进同一个分支。
              两者要说的话完全不同——前者该建议换个词，后者该指路去装服务器；
              共用一句「没有任何工具」会让人以为是搜索把工具弄丢了。 */}
          {search
            ? t("workspaces.noToolsMatch", { search })
            : t(
                selected !== DEFAULT_WS
                  ? "workspaces.noToolsInWorkspace"
                  : "workspaces.noToolsDefault",
              )}
        </p>
      ) : null}

      <div className="space-y-2">
        {tools.map((tool) => {
          const loading = pendingTools.has(toolKey(tool.serverId, tool.originalName));
          return (
            <Card key={tool.exposedName} className="px-4 py-3">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <div className="flex items-center gap-2">
                    {tool.icons?.[0]?.src ? (
                      <img
                        src={tool.icons[0].src}
                        alt=""
                        aria-hidden
                        className="h-4 w-4 shrink-0 rounded-sm"
                      />
                    ) : null}
                    <code className="font-mono text-sm text-ink">{tool.exposedName}</code>
                    <Tag>{tool.serverSlug}</Tag>
                    {/* 三态各说各话：下游申报了就不标（正常情况不该刷屏），
                        用户标的要写明是用户标的，两边都没有才是那条警告。 */}
                    {tool.annotations ? null : tool.declaredAnnotations ? (
                      <Tag>{t("workspaces.annotationsByUser")}</Tag>
                    ) : (
                      <Tag tone="amber">{t("workspaces.annotationsMissing")}</Tag>
                    )}
                  </div>
                  {tool.description ? (
                    <p className="mt-1 max-w-xl truncate text-xs text-faint">{tool.description}</p>
                  ) : null}
                </div>
                <div className="flex shrink-0 items-center gap-2">
                  <Field
                    value={aliasDraft[tool.exposedName] ?? ""}
                    onChange={(e) =>
                      setAliasDraft((draft) => ({ ...draft, [tool.exposedName]: e.target.value }))
                    }
                    placeholder={t("common.aliasPlaceholder")}
                    className="h-8 w-28 px-2.5 font-mono text-xs"
                  />
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={hubDown || !aliasDraft[tool.exposedName]}
                    loading={loading}
                    onClick={() =>
                      toggle.mutate({
                        serverId: tool.serverId,
                        originalName: tool.originalName,
                        enabled: true,
                        alias: aliasDraft[tool.exposedName],
                      })
                    }
                  >
                    {t("workspaces.setAlias")}
                  </Button>
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={hubDown}
                    onClick={() => setAnnotating(tool)}
                  >
                    {t("workspaces.annotations")}
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    disabled={hubDown}
                    loading={loading}
                    onClick={() =>
                      toggle.mutate({
                        serverId: tool.serverId,
                        originalName: tool.originalName,
                        enabled: false,
                        alias: null,
                      })
                    }
                  >
                    {t("workspaces.disable")}
                  </Button>
                </div>
              </div>

              {/* 触发提示：告诉模型「何时该用」。下游描述基本只写「能做什么」，
                  而模型选工具时匹配的是触发条件——这条缺口正是工具被冷落的主因。 */}
              <div className="mt-2 flex items-center gap-2">
                <button
                  type="button"
                  className="text-faint text-xs underline-offset-2 hover:text-accent hover:underline"
                  onClick={() =>
                    setSuffixOpen((open) => (open === tool.exposedName ? null : tool.exposedName))
                  }
                >
                  {t("workspaces.suffixTitle")}
                </button>
              </div>
              {suffixOpen === tool.exposedName ? (
                <div className="mt-2 rounded-lg border border-line p-3">
                  <p className="text-xs text-faint">{t("workspaces.suffixHint")}</p>
                  <Textarea
                    className="mt-2"
                    rows={2}
                    value={
                      suffixDraft[tool.exposedName] ?? savedSuffix(tool.serverId, tool.originalName)
                    }
                    placeholder={t("workspaces.suffixPlaceholder")}
                    disabled={hubDown}
                    aria-label={t("workspaces.suffixTitle")}
                    onChange={(e) =>
                      setSuffixDraft((draft) => ({
                        ...draft,
                        [tool.exposedName]: e.target.value,
                      }))
                    }
                  />
                  <div className="mt-2 flex justify-end">
                    <Button
                      size="sm"
                      disabled={hubDown}
                      loading={loading}
                      onClick={() =>
                        toggle.mutate({
                          serverId: tool.serverId,
                          originalName: tool.originalName,
                          enabled: true,
                          alias: aliasDraft[tool.exposedName] || null,
                          descriptionSuffix:
                            suffixDraft[tool.exposedName] ??
                            savedSuffix(tool.serverId, tool.originalName),
                        })
                      }
                    >
                      {t("workspaces.suffixSave")}
                    </Button>
                  </div>
                </div>
              ) : null}
            </Card>
          );
        })}
      </div>

      {annotating ? (
        <ToolAnnotationsModal
          toolName={annotating.exposedName}
          downstream={annotating.annotations ?? null}
          declared={annotating.declaredAnnotations ?? null}
          saving={setAnnotations.isPending}
          onSave={(annotations) => setAnnotations.mutate({ tool: annotating, annotations })}
          onClose={() => setAnnotating(null)}
        />
      ) : null}

      {disabled.length > 0 ? (
        <section className="mt-7">
          <p className="mb-3 text-sm font-medium text-ink">{t("workspaces.disabledTools")}</p>
          <div className="space-y-2">
            {disabled.map((item) => {
              const server = serverById.get(item.serverId);
              return (
                <Card
                  key={`${item.serverId}:${item.originalName}`}
                  className="flex items-center justify-between gap-3 px-4 py-2.5"
                >
                  <div className="flex min-w-0 items-center gap-2">
                    <code className="truncate font-mono text-xs text-ink" title={item.originalName}>
                      {item.originalName}
                    </code>
                    <Tag>{server?.slug ?? item.serverId}</Tag>
                  </div>
                  <Button
                    size="sm"
                    variant="ghost"
                    icon={RotateCcw}
                    disabled={hubDown}
                    loading={pendingTools.has(toolKey(item.serverId, item.originalName))}
                    onClick={() =>
                      toggle.mutate({
                        serverId: item.serverId,
                        originalName: item.originalName,
                        enabled: true,
                      })
                    }
                  >
                    {t("workspaces.restore")}
                  </Button>
                </Card>
              );
            })}
          </div>
        </section>
      ) : null}
    </div>
  );
}

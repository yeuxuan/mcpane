import {
  closestCenter,
  DndContext,
  type DragEndEvent,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import {
  arrayMove,
  SortableContext,
  sortableKeyboardCoordinates,
  verticalListSortingStrategy,
} from "@dnd-kit/sortable";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Download, ExternalLink, Layers, Pencil, Plus, Trash2 } from "lucide-react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BrandGlyph, ClientGlyph, SurfaceBadge } from "../components/BrandIcon";
import { ClaudeDesktopPanel } from "../components/ClaudeDesktopPanel";
import { ConfirmDialog } from "../components/ConfirmDialog";
import { PageHeader } from "../components/PageHeader";
import { ProviderCard, SortableRelayCard } from "../components/ProviderCard";
import { ProviderEditor } from "../components/ProviderEditor";
import { ProviderIcon } from "../components/ProviderIcon";
import { isRoutingApp, RoutingPanel } from "../components/RoutingPanel";
import { UnifiedProviderEditor } from "../components/UnifiedProviderEditor";
import {
  Button,
  Card,
  CodeBlock,
  EmptyState,
  ErrorBanner,
  IconButton,
  SearchInput,
  Tag,
} from "../components/ui";
import { PROVIDER_APPS, readVisibleApps } from "../lib/appVisibility";
import { CLIENT_META, CLIENTS, isAdditiveApp } from "../lib/clients";
import {
  type ClientApp,
  ipc,
  type ProviderProfile,
  type UnifiedProvider,
  unifiedIdOfShadow,
} from "../lib/ipc";
import { invalidateProviderQueries } from "../lib/providerQueries";
import { queryKeys } from "../lib/queryKeys";
import { cn, errorText } from "../lib/utils";

/**
 * 供应商页（排版对齐 cc-switch 主界面）：
 * - 顶部 AppSwitcher：每个客户端一个图标页签 + 统一供应商，一次只看一个视图；
 * - 页眉右侧「添加供应商」：进页内编辑面板（左预设列 + 右表单）；上次选中的客户端记入 localStorage；
 * - 卡片：品牌图标 chip、base_url 外链、hover 显现操作区、拖拽排序、复制、删除确认；
 * - 官方 = 恢复 OAuth 订阅登录，中转站 = 写 base_url + Key；
 * - 统一供应商 = 一份网关配置按 app 开关物化为影子条目（对齐 cc-switch Universal Provider）。
 */

type ProvidersTab = ClientApp | "unified";

const LAST_APP_KEY = "mcpane-providers-last-app";

function loadLastTab(): ProvidersTab {
  const stored = localStorage.getItem(LAST_APP_KEY);
  // 从 CLIENTS 派生，不手写白名单——此前是逐个列 app 字符串，加 Hermes 时漏了它，
  // 结果每次回到供应商页都悄悄回落 Claude，用户的选择根本记不住。
  if (stored === "unified" || PROVIDER_APPS.some((app) => app === stored)) {
    return stored as ProvidersTab;
  }
  return "claude";
}

export function Providers() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const providers = useQuery({
    queryKey: queryKeys.providers,
    queryFn: ipc.providersList,
    retry: false,
  });
  const current = useQuery({
    queryKey: queryKeys.providerCurrent,
    queryFn: ipc.providerCurrent,
    retry: false,
  });
  // 安装态：决定首启默认显示哪些页签，以及没装的客户端要不要给安装引导。
  // 装/卸载不会在应用运行期间频繁发生，缓存久一点，别每次进页面都扫 PATH。
  const installed = useQuery({
    queryKey: queryKeys.clientsInstalled,
    queryFn: ipc.providerClientsInstalled,
    retry: false,
    staleTime: 5 * 60 * 1000,
  });
  // 客户端可见性偏好在挂载时读取一次：隐藏的 tab 不渲染。页面切换会 remount，
  // 用户在设置里改完再回来即读到最新，无需跨页实时同步。
  // 注意：这里只认用户偏好，不按安装态自动隐藏——页签无声消失比多两个页签
  // 更让人困惑；没装的客户端改为标灰 + 进去给安装引导。
  const [visibleApps] = useState(readVisibleApps);
  const [activeTab, setActiveTab] = useState<ProvidersTab>(() => {
    const last = loadLastTab();
    // 上次选中的客户端被隐藏了 → 回落到第一个可见客户端（都隐藏则退到统一供应商）。
    if (last === "unified" || visibleApps[last]) return last;
    return PROVIDER_APPS.find((app) => visibleApps[app]) ?? "unified";
  });
  const unified = useQuery({
    queryKey: queryKeys.providersUnified,
    queryFn: ipc.providerUnifiedList,
    retry: false,
  });
  const [editing, setEditing] = useState<{
    profile?: ProviderProfile;
    nextSortOrder?: number;
  } | null>(null);
  const [editingUnified, setEditingUnified] = useState<{
    unified?: UnifiedProvider;
    nextSortOrder?: number;
  } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<ProviderProfile | null>(null);
  const [confirmDeleteUnified, setConfirmDeleteUnified] = useState<UnifiedProvider | null>(null);
  const [notice, setNotice] = useState<Record<string, string[]>>({});
  const [testResult, setTestResult] = useState<Record<string, string>>({});
  const [testingId, setTestingId] = useState<string | null>(null);
  const [search, setSearch] = useState("");
  const activeApp: ClientApp = activeTab === "unified" ? "claude" : activeTab;

  // additive 客户端（OpenCode）没有"当前生效项"，启用态来自 live 配置里的 id 列表。
  const additive = isAdditiveApp(activeApp);
  const liveIds = useQuery({
    queryKey: queryKeys.providerLiveIds(activeApp),
    queryFn: () => ipc.providerLiveIds(activeApp),
    enabled: additive,
    retry: false,
  });
  // Hermes v12+ providers 字典：Web UI 托管的外部供应商，只读呈现。
  const hermesExternal = useQuery({
    queryKey: queryKeys.hermesExternal,
    queryFn: ipc.providerHermesExternal,
    enabled: activeApp === "hermes",
    retry: false,
  });

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 8 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const switchTab = (tab: ProvidersTab) => {
    if (tab === activeTab) return;
    localStorage.setItem(LAST_APP_KEY, tab);
    setActiveTab(tab);
    // 搜索词是针对当前 app 列表的，切走后残留会莫名过滤新 app（甚至误显"无匹配"）。
    setSearch("");
  };

  const refresh = () => invalidateProviderQueries(queryClient, activeApp);

  // additive：从 live 配置移除（供应商记录保留，随时可再启用）。
  const removeFromLive = useMutation({
    mutationFn: ipc.providerRemoveFromLive,
    onSuccess: refresh,
    onError: (error, id) => {
      setNotice((n) => ({ ...n, [id]: [errorText(error)] }));
    },
  });

  const switchProvider = useMutation({
    mutationFn: ({ app, id }: { app: ClientApp; id: string }) => ipc.providerSwitch(app, id),
    onSuccess: (outcome, { id }) => {
      setNotice((n) => ({ ...n, [id]: outcome.warnings }));
      refresh();
    },
    onError: (error, { id }) => {
      setNotice((n) => ({ ...n, [id]: [t("providers.switchError", { error: errorText(error) })] }));
    },
  });
  // 删除供应商时同步清掉其提示/测速/配额残留，字典不随增删只增不减。
  const clearFeedback = (id: string) => {
    setNotice(({ [id]: _removed, ...rest }) => rest);
    setTestResult(({ [id]: _removed, ...rest }) => rest);
  };
  const remove = useMutation({
    mutationFn: ipc.providerDelete,
    onSuccess: (_data, id) => {
      clearFeedback(id);
      refresh();
    },
    onError: (error, id) => {
      setNotice((n) => ({ ...n, [id]: [errorText(error)] }));
    },
  });
  const removeUnified = useMutation({
    mutationFn: ipc.providerUnifiedDelete,
    onSuccess: (_data, id) => {
      clearFeedback(id);
      refresh();
    },
    onError: (error, id) => {
      setNotice((n) => ({ ...n, [id]: [errorText(error)] }));
    },
  });
  const duplicate = useMutation({
    mutationFn: ({ profile, nextSortOrder }: { profile: ProviderProfile; nextSortOrder: number }) =>
      ipc.providerAdd({
        ...profile,
        id: crypto.randomUUID().replaceAll("-", ""),
        name: t("providers.copyName", { name: profile.name }),
        // Key 在 Keychain 中与原供应商绑定，副本需自行填写；Codex slug 由后端重新生成。
        apiKeyRef: null,
        codexSlug: null,
        sortOrder: nextSortOrder,
      }),
    onSuccess: refresh,
    onError: (error, { profile }) => {
      setNotice((n) => ({ ...n, [profile.id]: [errorText(error)] }));
    },
  });
  // 拖拽后批量回写 sortOrder；先乐观更新缓存避免松手后闪回旧顺序。
  const [reorderError, setReorderError] = useState<string | null>(null);
  const reorder = useMutation({
    mutationFn: async (updates: ProviderProfile[]) => {
      for (const profile of updates) {
        await ipc.providerUpdate(profile);
      }
    },
    onMutate: (updates) => {
      setReorderError(null);
      queryClient.setQueryData<ProviderProfile[]>(queryKeys.providers, (old) => {
        if (!old) return old;
        const byId = new Map(updates.map((p) => [p.id, p]));
        return old.map((p) => byId.get(p.id) ?? p);
      });
    },
    // 回写失败时 onSettled 的刷新会让卡片闪回旧顺序——必须说明原因，
    // 否则用户只看到「拖完又弹回去」的灵异现象。
    onError: (error) => setReorderError(t("providers.reorderFailed", { error: errorText(error) })),
    onSettled: refresh,
  });

  const test = async (id: string) => {
    setTestingId(id);
    setTestResult((r) => ({ ...r, [id]: t("providers.testing") }));
    try {
      const status = await ipc.providerTestConnectivity(id);
      setTestResult((r) => ({ ...r, [id]: t("providers.testOk", { status }) }));
    } catch (error) {
      setTestResult((r) => ({ ...r, [id]: t("providers.testFail", { error: errorText(error) }) }));
    } finally {
      setTestingId(null);
    }
  };

  const list = (providers.data ?? []).filter((p) => p.app === activeApp);
  const official = list.filter((p) => p.category === "official");
  const relays = list
    .filter((p) => p.category === "relay")
    .sort((a, b) => a.sortOrder - b.sortOrder);
  // 中转站按名称 / base_url / 模型模糊过滤（对齐 cc-switch ProviderList 多字段匹配）。
  // 搜索态下禁用拖拽排序：过滤后的序不是真实 sortOrder，拖动会写错序。
  const query = search.trim().toLowerCase();
  const filteredRelays = useMemo(() => {
    if (!query) return relays;
    return relays.filter((p) =>
      [p.name, p.baseUrl, p.model].some((f) => f?.toLowerCase().includes(query)),
    );
  }, [relays, query]);
  const currentId = current.data?.[activeApp] ?? null;
  // 从未切换过 + 客户端确实装了 = 官方订阅事实生效（首启收编保证了"live 其实是
  // 中转"的情况会被置 current）：卡片如实标记，编辑保存也走「当前生效」的写回
  // 路径。没装客户端时不做这个断言——一个装都没装的 CLI 谈不上"正在使用官方"。
  const appInstalled = installed.data?.[activeApp] ?? true;
  const isCurrentProvider = (provider: ProviderProfile) =>
    currentId === provider.id ||
    (currentId === null && provider.category === "official" && appInstalled);

  const onDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const oldIndex = relays.findIndex((p) => p.id === active.id);
    const newIndex = relays.findIndex((p) => p.id === over.id);
    if (oldIndex < 0 || newIndex < 0) return;
    const next = arrayMove(relays, oldIndex, newIndex).map((p, index) => ({
      ...p,
      sortOrder: index,
    }));
    reorder.mutate(next.filter((p, index) => relays[index]?.id !== p.id));
  };

  const cardProps = (provider: ProviderProfile) => {
    const unifiedUid = unifiedIdOfShadow(provider.id);
    const unifiedRecord = unifiedUid
      ? (unified.data ?? []).find((u) => u.id === unifiedUid)
      : undefined;
    return {
      provider,
      app: activeApp,
      isCurrent: isCurrentProvider(provider),
      isSwitching: switchProvider.isPending && switchProvider.variables?.id === provider.id,
      switchPending: switchProvider.isPending,
      isTesting: testingId === provider.id,
      warnings: notice[provider.id] ?? [],
      testResult: testResult[provider.id],
      unifiedManaged: Boolean(unifiedRecord),
      isAdditive: additive,
      isEnabled: additive ? (liveIds.data ?? []).includes(provider.id) : undefined,
      onRemoveFromLive: () => removeFromLive.mutate(provider.id),
      onSwitch: () => switchProvider.mutate({ app: activeApp, id: provider.id }),
      onTest: () => void test(provider.id),
      // 影子条目的编辑路由回统一供应商弹窗（数据源在统一记录里）。
      onEdit: () =>
        unifiedRecord
          ? setEditingUnified({ unified: unifiedRecord })
          : setEditing({ profile: provider }),
      onDuplicate: () => duplicate.mutate({ profile: provider, nextSortOrder: relays.length }),
      onDelete: () => setConfirmDelete(provider),
    };
  };

  // 编辑态整页接管：面板铺开后不再有列表在背后隔着一层遮罩，返回栏负责回到列表。
  if (editingUnified) {
    return (
      <UnifiedProviderEditor
        unified={editingUnified.unified}
        nextSortOrder={editingUnified.nextSortOrder ?? 0}
        onClose={() => setEditingUnified(null)}
      />
    );
  }
  if (editing) {
    return (
      <ProviderEditor
        app={activeApp}
        profile={editing.profile}
        nextSortOrder={editing.nextSortOrder ?? 0}
        isCurrent={editing.profile ? isCurrentProvider(editing.profile) : false}
        onClose={() => setEditing(null)}
      />
    );
  }

  return (
    <div>
      <PageHeader
        title={t("providers.title")}
        actions={
          activeTab === "unified" ? (
            <Button
              variant="primary"
              size="sm"
              icon={Plus}
              onClick={() => setEditingUnified({ nextSortOrder: (unified.data ?? []).length })}
            >
              {t("providers.unified.add")}
            </Button>
          ) : (
            <>
              {/* 中转站多了不好找：搜索放页头与「添加」并排；空列表时无意义，不显示。 */}
              {relays.length > 0 ? (
                <SearchInput
                  size="sm"
                  className="w-56"
                  placeholder={t("providers.searchPlaceholder")}
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                />
              ) : null}
              <Button
                variant="primary"
                size="sm"
                icon={Plus}
                data-tour="providers-add"
                onClick={() => setEditing({ nextSortOrder: relays.length })}
              >
                {t("providers.addRelay")}
              </Button>
            </>
          )
        }
      />

      {/*
        客户端分段切换器（对齐 cc-switch AppSwitcher：灰底容器，选中项白底浮起）。
        只放图标不放名称——客户端还会继续增加，一排文字页签很快就换行了；
        名称走 title + aria-label，鼠标悬停能看到，读屏也照读不误。
      */}
      <div data-tour="providers-tabs" className="mb-4 inline-flex gap-1 rounded-xl bg-hover p-1">
        {CLIENTS.filter(({ app }) => visibleApps[app]).map(({ app, label, color }) => {
          const isActive = activeTab === app;
          // 没检测到安装：页签留在原位但退到弱色（品牌色也去掉），
          // 一眼能看出"这个还没装"，点进去就是安装引导。
          const missing = installed.data ? !installed.data[app] : false;
          return (
            <button
              key={app}
              type="button"
              aria-pressed={isActive}
              aria-label={label}
              title={missing ? t("providers.notInstalledTitle", { client: label }) : label}
              onClick={() => switchTab(app)}
              className={cn(
                "relative inline-flex h-9 w-10 items-center justify-center rounded-lg transition-all duration-200",
                isActive
                  ? "bg-card text-ink shadow-[var(--shadow-card)]"
                  : "text-muted hover:bg-card/60 hover:text-ink",
                missing && !isActive && "opacity-45",
              )}
            >
              <span
                className="relative inline-flex shrink-0"
                style={{ color: missing ? "currentColor" : color }}
                aria-hidden
              >
                <BrandGlyph name={app} size={19} muted={missing} />
                {/* 形态角标：纯图标页签里 Claude 系两格就是同一个图形（对齐 cc-switch
                    AppSwitcher）。底色随选中态走，才像一片浮在页签上的小标。 */}
                <SurfaceBadge
                  app={app}
                  className={isActive ? "border-line bg-card" : "border-card bg-hover"}
                />
              </span>
              {/* 未安装角标：图标化之后没有文字可标灰，用一个小空心点补上信息。
                  放右上——右下让给形态角标，两者在 Claude 系会同时出现。 */}
              {missing ? (
                <span
                  aria-hidden
                  className="absolute top-1 right-1 h-1.5 w-1.5 rounded-full border border-current bg-card"
                />
              ) : null}
            </button>
          );
        })}
        <button
          type="button"
          data-tour="providers-unified"
          aria-pressed={activeTab === "unified"}
          aria-label={t("providers.unified.tab")}
          title={t("providers.unified.tab")}
          onClick={() => switchTab("unified")}
          className={cn(
            "inline-flex h-9 w-10 items-center justify-center rounded-lg transition-all duration-200",
            activeTab === "unified"
              ? "bg-card text-ink shadow-[var(--shadow-card)]"
              : "text-muted hover:bg-card/60 hover:text-ink",
          )}
        >
          <Layers size={18} aria-hidden />
        </button>
      </div>

      {/* 图标页签少了文字，当前视图靠这行标题交代（顺带说明未安装态）。 */}
      <div className="mb-3 flex flex-wrap items-center gap-2">
        <h2 className="text-[15px] font-semibold text-ink">
          {activeTab === "unified"
            ? t("providers.unified.tab")
            : (CLIENT_META[activeTab]?.label ?? "")}
        </h2>
        {activeTab !== "unified" && installed.data && !appInstalled ? (
          <Tag tone="slate">{t("settings.localEnv.missing")}</Tag>
        ) : null}
      </div>

      {providers.isError ? (
        <p className="text-sm text-danger">
          {t("providers.loadError", { error: errorText(providers.error) })}
        </p>
      ) : null}

      {activeTab === "unified" ? (
        <div className="space-y-3">
          {(unified.data ?? []).map((entry) => (
            <UnifiedCard
              key={entry.id}
              unified={entry}
              warnings={notice[entry.id] ?? []}
              onEdit={() => setEditingUnified({ unified: entry })}
              onDelete={() => setConfirmDeleteUnified(entry)}
            />
          ))}
          {(unified.data ?? []).length === 0 ? (
            <EmptyState
              title={t("providers.unified.emptyTitle")}
              description={t("providers.unified.empty")}
              action={
                <Button
                  size="sm"
                  icon={Plus}
                  onClick={() => setEditingUnified({ nextSortOrder: 0 })}
                >
                  {t("providers.unified.add")}
                </Button>
              }
            />
          ) : null}
        </div>
      ) : (
        <div className="space-y-3">
          {/* 没装客户端：先给"怎么装"，而不是让用户对着切不动的卡片猜。 */}
          {installed.data && !appInstalled ? <ClientInstallNotice app={activeApp} /> : null}
          {activeTab === "claude-desktop" ? <ClaudeDesktopPanel /> : null}
          {isRoutingApp(activeTab) ? (
            <RoutingPanel app={activeTab} relays={relays} currentId={currentId} />
          ) : null}
          {official.map((provider, i) => (
            <ProviderCard
              key={provider.id}
              // 官方订阅项内置、恒存在，导览指它讲"卡片上能做什么"最稳。
              data-tour={i === 0 ? "providers-card" : undefined}
              {...cardProps(provider)}
            />
          ))}

          <ErrorBanner message={reorderError} onDismiss={() => setReorderError(null)} />
          {query ? (
            // 搜索态：过滤后的序不是真实 sortOrder，禁用拖拽避免写错序，渲染普通卡片。
            filteredRelays.length > 0 ? (
              filteredRelays.map((provider) => (
                <ProviderCard key={provider.id} {...cardProps(provider)} />
              ))
            ) : (
              <p className="rounded-xl border border-dashed border-line-strong px-5 py-8 text-center text-sm text-muted">
                {t("providers.noMatch")}
              </p>
            )
          ) : (
            <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
              <SortableContext
                items={relays.map((p) => p.id)}
                strategy={verticalListSortingStrategy}
              >
                {relays.map((provider) => (
                  <SortableRelayCard key={provider.id} {...cardProps(provider)} />
                ))}
              </SortableContext>
            </DndContext>
          )}

          {relays.length === 0 ? (
            <EmptyState
              title={t("providers.emptyTitle")}
              description={t(additive ? "providers.emptyAdditive" : "providers.empty")}
              action={
                <Button size="sm" icon={Plus} onClick={() => setEditing({ nextSortOrder: 0 })}>
                  {t("providers.addRelay")}
                </Button>
              }
            />
          ) : null}

          {activeApp === "hermes" && (hermesExternal.data?.length ?? 0) > 0 ? (
            // Hermes 自身（Web UI）托管的供应商：没有 profile、不给任何操作
            // 入口——硬套 ProviderCard 会撒谎（它绑定 profile 语义）。
            <div className="mt-2">
              <p className="mb-2 text-xs font-medium text-muted">
                {t("providers.hermesManagedTitle")}
              </p>
              {(hermesExternal.data ?? []).map((external) => (
                <Card
                  key={external.name}
                  className="mb-2 flex items-center gap-3 px-4 py-3"
                  title={t("providers.hermesManagedHint")}
                >
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-sm text-ink">{external.name}</p>
                    {external.baseUrl ? (
                      <p className="truncate text-xs text-faint" title={external.baseUrl}>
                        {external.baseUrl}
                      </p>
                    ) : null}
                  </div>
                  <span className="shrink-0 rounded-md border border-line px-1.5 py-0.5 text-[11px] text-muted">
                    {t("providers.hermesManagedBadge")}
                  </span>
                </Card>
              ))}
            </div>
          ) : null}
        </div>
      )}

      {confirmDelete ? (
        <ConfirmDialog
          title={
            confirmDelete.category === "official"
              ? t("providers.confirmDeleteOfficialTitle")
              : t("providers.confirmDeleteTitle")
          }
          message={
            confirmDelete.category === "official"
              ? t("providers.confirmDeleteOfficialMessage", { name: confirmDelete.name })
              : t("providers.confirmDeleteMessage", { name: confirmDelete.name })
          }
          confirmText={t("providers.confirmDeleteAction")}
          onConfirm={() => {
            remove.mutate(confirmDelete.id);
            setConfirmDelete(null);
          }}
          onCancel={() => setConfirmDelete(null)}
        />
      ) : null}

      {confirmDeleteUnified ? (
        <ConfirmDialog
          title={t("providers.unified.confirmDeleteTitle")}
          message={t("providers.unified.confirmDeleteMessage", {
            name: confirmDeleteUnified.name,
          })}
          confirmText={t("providers.confirmDeleteAction")}
          onConfirm={() => {
            removeUnified.mutate(confirmDeleteUnified.id);
            setConfirmDeleteUnified(null);
          }}
          onCancel={() => setConfirmDeleteUnified(null)}
        />
      ) : null}
    </div>
  );
}

/**
 * 未检测到该客户端时的安装引导：可复制的官方安装命令 + 官网。
 * 不代跑安装——包管理器锚定（nvm/volta/brew）在 GUI 进程里极易失败，
 * 给一行能直接粘进终端的命令更可靠。
 */
function ClientInstallNotice({ app }: { app: ClientApp }) {
  const { t } = useTranslation();
  const meta = CLIENT_META[app];
  const info = meta.install;
  return (
    <Card className="border-warn/40 bg-warn-soft/40 p-4">
      <div className="flex items-start gap-3">
        <span className="mt-0.5 shrink-0 text-warn" aria-hidden>
          <Download size={16} />
        </span>
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium text-ink">
            {t("providers.notInstalledTitle", { client: meta.label })}
          </p>
          <p className="mt-1 text-xs text-muted">{t("providers.notInstalledHint")}</p>
          {info.command ? <CodeBlock code={info.command} className="mt-2.5" /> : null}
          <div className="mt-2.5 flex items-center gap-2">
            <Button
              size="sm"
              variant="secondary"
              icon={ExternalLink}
              onClick={() => void ipc.openExternal(info.url)}
            >
              {t("providers.notInstalledOpenSite")}
            </Button>
          </div>
        </div>
      </div>
    </Card>
  );
}

/** 统一供应商卡片：名称 + 启用 app 徽章 + 备注/base_url，hover 显编辑/删除。 */
function UnifiedCard({
  unified,
  warnings,
  onEdit,
  onDelete,
}: {
  unified: UnifiedProvider;
  warnings: string[];
  onEdit: () => void;
  onDelete: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Card interactive className="group p-4 transition-colors">
      <div className="flex items-center gap-3">
        <span
          className="flex h-9 w-9 shrink-0 items-center justify-center rounded-[9px] border border-line bg-app transition-transform duration-300 group-hover:scale-105"
          aria-hidden
        >
          <ProviderIcon name={unified.name} size={20} />
        </span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-semibold text-ink">{unified.name}</span>
            {/* 客户端标签不另派颜色：中性底 + 各自品牌 logo，与全站同一口径。 */}
            {(["claude", "codex"] as const)
              .filter((app) => unified.apps[app])
              .map((app) => (
                <Tag key={app}>
                  <span className="mr-1 inline-flex">
                    <ClientGlyph app={app} size={11} />
                  </span>
                  {CLIENT_META[app].label}
                </Tag>
              ))}
            {unified.apiKeyRef ? (
              <Tag tone="green">{t("providers.keyStored")}</Tag>
            ) : (
              <Tag tone="amber">{t("providers.keyMissing")}</Tag>
            )}
          </div>
          <button
            type="button"
            title={unified.websiteUrl ?? unified.baseUrl}
            onClick={() => {
              const url = unified.websiteUrl ?? unified.baseUrl;
              if (url) void ipc.openExternal(url);
            }}
            className={cn(
              "mt-1 block max-w-full truncate text-left text-xs text-accent hover:underline",
              !unified.notes && "font-mono",
            )}
          >
            {unified.notes ?? unified.baseUrl}
          </button>
          {warnings.map((warning) => (
            <p key={warning} className="mt-1 text-xs text-warn">
              {warning}
            </p>
          ))}
        </div>
        <div className="flex shrink-0 items-center gap-1.5 opacity-0 transition-opacity duration-200 group-focus-within:opacity-100 group-hover:opacity-100">
          <IconButton
            icon={Pencil}
            title={t("providers.edit", { name: unified.name })}
            aria-label={t("providers.edit", { name: unified.name })}
            onClick={onEdit}
          />
          <IconButton
            icon={Trash2}
            tone="danger"
            title={t("providers.delete", { name: unified.name })}
            aria-label={t("providers.delete", { name: unified.name })}
            onClick={onDelete}
          />
        </div>
      </div>
    </Card>
  );
}

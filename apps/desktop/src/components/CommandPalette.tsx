import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Loader2, type LucideIcon } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { CLIENT_META, isAdditiveApp, PROVIDER_APPS } from "../lib/clients";
import { type ClientApp, ipc } from "../lib/ipc";
import { invalidateProviderQueries } from "../lib/providerQueries";
import { queryKeys } from "../lib/queryKeys";
import { invalidateServerQueries } from "../lib/serverQueries";
import { cn, errorText } from "../lib/utils";
import { DEFAULT_WS, useWorkspace } from "../lib/workspace";
import { MAIN_NAV, NAV_ICON, type PageKey } from "./Shell";
import { ErrorBanner, Modal, SearchInput } from "./ui";

type Group = "pages" | "providers" | "workspaces" | "servers";

interface PaletteItem {
  id: string;
  group: Group;
  label: string;
  icon?: LucideIcon;
  /** 已是当前项：展示对勾，不可执行（如已生效的供应商/已选中的工作区）。 */
  current?: boolean;
  /** 对应 mutation 正在跑这一项。 */
  pending?: boolean;
  run: () => void;
}

const GROUP_ORDER: Group[] = ["pages", "providers", "workspaces", "servers"];

/**
 * ⌘K 命令面板：跨页面的快速入口——跳页、切供应商、切工作区、启停 MCP Server。
 * 壳用共享 Modal；输入 + 键盘交互对齐 ModelCombobox（大小写不敏感子串过滤，
 * ↑↓ 移动，Enter 执行，Esc 交给 Modal 自己的全局监听关闭）。
 *
 * 用 `activeId`（而非下标）追踪高亮项：过滤/数据刷新会让列表下标频繁挪位，
 * 下标一晃就会指向意料之外的行；id 在数据不变时恒定，只在真正筛掉时才失效。
 */
export function CommandPalette({
  onNavigate,
  onClose,
}: {
  onNavigate: (page: PageKey) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const listId = useId();
  const [query, setQuery] = useState("");
  const [activeId, setActiveId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  const optionRefs = useRef<Map<string, HTMLDivElement>>(new Map());

  const { selectedId, setSelectedId, workspaces } = useWorkspace();
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
  // additive 客户端（OpenCode/OpenClaw/Hermes）没有单一的"当前生效项"，
  // 启用态要各自查 live 配置里的 id 列表（同 Providers 页）。
  const additiveApps = PROVIDER_APPS.filter(isAdditiveApp);
  const liveIdsQueries = useQueries({
    queries: additiveApps.map((app) => ({
      queryKey: queryKeys.providerLiveIds(app),
      queryFn: () => ipc.providerLiveIds(app),
      retry: false,
    })),
  });
  const servers = useQuery({
    queryKey: queryKeys.servers,
    queryFn: ipc.serversList,
    retry: false,
  });

  const switchProvider = useMutation({
    mutationFn: ({ app, id }: { app: ClientApp; id: string }) => ipc.providerSwitch(app, id),
    onMutate: () => {
      setError(null);
      setWarnings([]);
    },
    // 切换带警告（如需重启客户端）时留在面板里给人看完，没有才直接关。
    onSuccess: (outcome) => {
      if (outcome.warnings.length > 0) setWarnings(outcome.warnings);
      else onClose();
    },
    onError: (mutationError) =>
      setError(t("providers.switchError", { error: errorText(mutationError) })),
    // 失败也收敛到后端真值：切换可能写了一半配置。
    onSettled: (_data, _error, { app }) => invalidateProviderQueries(queryClient, app),
  });
  const toggleServer = useMutation({
    mutationFn: ({ id, enabled }: { id: string; enabled: boolean }) =>
      ipc.serverSetEnabled(id, enabled),
    onMutate: () => setError(null),
    onSuccess: () => {
      invalidateServerQueries(queryClient);
      onClose();
    },
    onError: (mutationError) =>
      setError(t("common.actionFailed", { error: errorText(mutationError) })),
  });

  const pageItems: PaletteItem[] = MAIN_NAV.map((key) => ({
    id: `page-${key}`,
    group: "pages",
    label: t(`nav.${key}`),
    icon: NAV_ICON[key],
    run: () => {
      onNavigate(key);
      onClose();
    },
  }));

  const liveIdsByApp = new Map(additiveApps.map((app, i) => [app, liveIdsQueries[i]?.data ?? []]));
  const providerItems: PaletteItem[] = PROVIDER_APPS.flatMap((app) => {
    const meta = CLIENT_META[app];
    return [...(providers.data ?? [])]
      .filter((p) => p.app === app)
      .sort((a, b) => a.sortOrder - b.sortOrder)
      .map((p) => {
        const isCurrent = isAdditiveApp(app)
          ? (liveIdsByApp.get(app) ?? []).includes(p.id)
          : current.data?.[app] === p.id;
        return {
          id: `provider-${p.id}`,
          group: "providers" as const,
          label: t("palette.providerItem", { app: meta.label, name: p.name }),
          current: isCurrent,
          pending: switchProvider.isPending && switchProvider.variables?.id === p.id,
          run: () => switchProvider.mutate({ app, id: p.id }),
        };
      });
  });

  const workspaceItems: PaletteItem[] = [
    { id: DEFAULT_WS, name: t("workspace.default") },
    ...workspaces.filter((ws) => ws.id !== DEFAULT_WS),
  ].map((ws) => ({
    id: `workspace-${ws.id}`,
    group: "workspaces" as const,
    label: t("palette.workspaceItem", { name: ws.name }),
    current: ws.id === selectedId,
    run: () => {
      setSelectedId(ws.id);
      onClose();
    },
  }));

  const serverItems: PaletteItem[] = (servers.data ?? []).map((s) => ({
    id: `server-${s.id}`,
    group: "servers" as const,
    label: t("servers.toggle", {
      action: t(s.enabled ? "servers.disable" : "servers.enable"),
      name: s.name,
    }),
    pending: toggleServer.isPending && toggleServer.variables?.id === s.id,
    run: () => toggleServer.mutate({ id: s.id, enabled: !s.enabled }),
  }));

  const items: PaletteItem[] = [...pageItems, ...providerItems, ...workspaceItems, ...serverItems];
  const q = query.trim().toLowerCase();
  const matches = q ? items.filter((item) => item.label.toLowerCase().includes(q)) : items;
  const matchKey = matches.map((item) => item.id).join("|");

  // 过滤结果变化（含首次数据到达）时，把高亮定位到第一个可执行项；
  // pending/current 等标记翻转不算——那不该抖动焦点。
  // biome-ignore lint/correctness/useExhaustiveDependencies: 只依赖候选 id 集合，matches 每次渲染都是新数组
  useEffect(() => {
    const first = matches.find((item) => !item.current);
    setActiveId(first?.id ?? null);
  }, [matchKey]);

  useEffect(() => {
    if (!activeId) return;
    // jsdom 不实现 scrollIntoView（单测环境），可选调用避免那里直接报错。
    optionRefs.current.get(activeId)?.scrollIntoView?.({ block: "nearest" });
  }, [activeId]);

  const runnable = matches.filter((item) => !item.current);
  const moveActive = (dir: 1 | -1) => {
    if (runnable.length === 0) return;
    const idx = runnable.findIndex((item) => item.id === activeId);
    const next =
      idx < 0
        ? dir === 1
          ? 0
          : runnable.length - 1
        : (idx + dir + runnable.length) % runnable.length;
    const nextItem = runnable[next];
    if (nextItem) setActiveId(nextItem.id);
  };
  const runActive = () => {
    const item = matches.find((i) => i.id === activeId);
    if (!item || item.current || item.pending) return;
    item.run();
  };

  const grouped = GROUP_ORDER.map((group) => ({
    group,
    items: matches.filter((item) => item.group === group),
  })).filter((g) => g.items.length > 0);

  return (
    <Modal title={t("palette.title")} align="top" onClose={onClose}>
      <SearchInput
        role="combobox"
        aria-label={t("palette.title")}
        aria-expanded
        aria-controls={listId}
        aria-activedescendant={activeId ? `${listId}-${activeId}` : undefined}
        aria-autocomplete="list"
        autoComplete="off"
        autoFocus
        placeholder={t("palette.searchPlaceholder")}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown") {
            e.preventDefault();
            moveActive(1);
          } else if (e.key === "ArrowUp") {
            e.preventDefault();
            moveActive(-1);
          } else if (e.key === "Enter") {
            e.preventDefault();
            runActive();
          }
        }}
      />
      <ErrorBanner message={error} onDismiss={() => setError(null)} />
      {warnings.map((line) => (
        <p key={line} className="mb-2 text-xs text-warn">
          {line}
        </p>
      ))}
      <div
        id={listId}
        role="listbox"
        aria-label={t("palette.title")}
        className="mt-3 max-h-96 overflow-y-auto"
      >
        {matches.length === 0 ? (
          <p className="px-2.5 py-6 text-center text-xs text-muted">{t("palette.noResults")}</p>
        ) : (
          grouped.map(({ group, items: groupItems }) => (
            // biome-ignore lint/a11y/useSemanticElements: listbox 内的分组（listbox → group → option），非表单字段组，<fieldset> 语义不对
            <div key={group} role="group" aria-label={t(`palette.group.${group}`)} className="mb-1">
              {/* aria-hidden：分组名已经由上面的 role="group" aria-label 播报，这里只服务视觉。 */}
              <p
                aria-hidden
                className="px-2.5 pt-2 pb-1 text-[11px] font-semibold tracking-wide text-faint uppercase"
              >
                {t(`palette.group.${group}`)}
              </p>
              {groupItems.map((item) => {
                const Icon = item.icon;
                const isActive = item.id === activeId;
                return (
                  <div
                    key={item.id}
                    id={`${listId}-${item.id}`}
                    role="option"
                    tabIndex={-1}
                    aria-selected={isActive}
                    aria-disabled={item.current || item.pending || undefined}
                    ref={(el) => {
                      if (el) optionRefs.current.set(item.id, el);
                      else optionRefs.current.delete(item.id);
                    }}
                    onMouseEnter={() => {
                      if (!item.current) setActiveId(item.id);
                    }}
                    onMouseDown={(e) => {
                      e.preventDefault();
                      if (!item.current && !item.pending) item.run();
                    }}
                    className={cn(
                      "flex cursor-default items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-[13px]",
                      item.current ? "text-faint" : isActive ? "bg-hover text-ink" : "text-ink",
                    )}
                  >
                    {Icon ? (
                      <Icon size={15} strokeWidth={2} aria-hidden className="shrink-0" />
                    ) : null}
                    <span className="min-w-0 flex-1 truncate">{item.label}</span>
                    {item.pending ? (
                      <Loader2
                        size={14}
                        strokeWidth={2}
                        aria-hidden
                        className="shrink-0 animate-spin"
                      />
                    ) : item.current ? (
                      <span className="flex shrink-0 items-center gap-1 text-accent">
                        <Check size={14} strokeWidth={2.5} aria-hidden />
                        <span className="sr-only">{t("palette.current")}</span>
                      </span>
                    ) : null}
                  </div>
                );
              })}
            </div>
          ))
        )}
      </div>
    </Modal>
  );
}

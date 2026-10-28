import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, Plus, RotateCcw, Router, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { isRoutingApp, type RoutingApp } from "../lib/clients";
import { formatTokens } from "../lib/format";
import {
  DEFAULT_ROUTING_TUNING,
  ipc,
  type ProviderProfile,
  type RouteCandidateStatus,
  type RoutingConfig,
  type RoutingTuning,
} from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { RouteRequestLog } from "./RouteRequestLog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "./shadcn/dropdown-menu";
import {
  Button,
  CollapseToggle,
  Field,
  IconButton,
  LabeledField,
  StatusBadge,
  Switch,
  Tag,
} from "./ui";

export { isRoutingApp, type RoutingApp };

type TuningKey = keyof RoutingTuning;
type TuningDraft = Record<TuningKey, string>;

const TUNING_KEYS: TuningKey[] = [
  "firstByteTimeoutSecs",
  "idleTimeoutSecs",
  "failureThreshold",
  "cooldownSecs",
];

/** 与后端校验范围一致（provider_routing_set 的入参检查）。 */
const TUNING_RANGES: Record<TuningKey, [number, number]> = {
  firstByteTimeoutSecs: [10, 600],
  idleTimeoutSecs: [30, 1800],
  failureThreshold: [1, 20],
  cooldownSecs: [10, 3600],
};

function toTuningDraft(tuning: RoutingTuning): TuningDraft {
  return {
    firstByteTimeoutSecs: String(tuning.firstByteTimeoutSecs),
    idleTimeoutSecs: String(tuning.idleTimeoutSecs),
    failureThreshold: String(tuning.failureThreshold),
    cooldownSecs: String(tuning.cooldownSecs),
  };
}

/**
 * 本地路由（ADR-022）：接管开关 + 故障转移顺序 + 候选健康。
 * 顺序第 1 位恒为当前供应商（随切换变化，不可移动）；其后为备用队列。
 */
export function RoutingPanel({
  app,
  relays,
  currentId,
}: {
  app: RoutingApp;
  /** 本 app 的全部中转站（按展示序）。 */
  relays: ProviderProfile[];
  currentId: string | null;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const routing = useQuery({
    queryKey: queryKeys.providerRouting(app),
    queryFn: () => ipc.providerRoutingGet(app),
  });
  const status = useQuery({
    queryKey: queryKeys.routerStatus,
    queryFn: ipc.routerStatus,
    ...livePoll(5000),
  });
  const proxy = useQuery({
    queryKey: queryKeys.desktopProxyStatus,
    queryFn: ipc.desktopProxyStatus,
  });

  const config: RoutingConfig = routing.data ?? {
    enabled: false,
    failoverQueue: [],
    tuning: DEFAULT_ROUTING_TUNING,
    strategy: "failover",
  };
  const current = relays.find((p) => p.id === currentId) ?? null;
  const queue = config.failoverQueue
    .filter((id) => id !== currentId)
    .map((id) => relays.find((p) => p.id === id))
    .filter((p): p is ProviderProfile => p !== undefined);
  const addable = relays.filter((p) => p.id !== currentId && !queue.some((q) => q.id === p.id));
  const appStatus = status.data?.find((s) => s.app === app);
  const health = (id: string) => appStatus?.candidates.find((c) => c.providerId === id);

  // 保存前取存储里的最新配置、只合并本次改动的字段：托盘也能开关路由，窗口里的
  // 缓存可能已过期，整份回写会把托盘的操作撤回。
  const patchRouting = async (patch: Partial<RoutingConfig>) => {
    const latest = await ipc.providerRoutingGet(app);
    return ipc.providerRoutingSet(app, { ...latest, ...patch });
  };
  const save = async (patch: Partial<RoutingConfig>) => {
    setSaving(true);
    setError(null);
    try {
      const saved = await patchRouting(patch);
      queryClient.setQueryData(queryKeys.providerRouting(app), saved);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSaving(false);
      void queryClient.invalidateQueries({ queryKey: queryKeys.routerStatus });
      void queryClient.invalidateQueries({ queryKey: queryKeys.desktopProxyStatus });
    }
  };
  const saveQueue = (ids: string[]) => void save({ failoverQueue: ids });
  const move = (index: number, delta: number) => {
    const a = queue[index];
    const b = queue[index + delta];
    if (!a || !b) return;
    saveQueue(queue.map((p) => (p.id === a.id ? b.id : p.id === b.id ? a.id : p.id)));
  };

  // 高级设置：熔断/超时调参，默认折叠——日常不用管，出问题时才需要动。
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [tuningDraft, setTuningDraft] = useState<TuningDraft>(() => toTuningDraft(config.tuning));
  const [tuningSaving, setTuningSaving] = useState(false);
  const [tuningError, setTuningError] = useState<string | null>(null);
  // config.tuning 只在初次加载或本区块自己保存成功后换引用，回填草稿不会
  // 打断用户在切队列等其他操作触发的重渲染里正在编辑的输入。
  useEffect(() => {
    setTuningDraft(toTuningDraft(config.tuning));
  }, [config.tuning]);
  const tuningNumbers = TUNING_KEYS.reduce<Partial<Record<TuningKey, number>>>((acc, key) => {
    acc[key] = Number(tuningDraft[key]);
    return acc;
  }, {}) as Record<TuningKey, number>;
  const tuningFieldInvalid = (key: TuningKey) => {
    const n = tuningNumbers[key];
    const [min, max] = TUNING_RANGES[key];
    return !Number.isInteger(n) || n < min || n > max;
  };
  const tuningValid = TUNING_KEYS.every((key) => !tuningFieldInvalid(key));
  const saveTuning = async () => {
    if (!tuningValid) return;
    setTuningSaving(true);
    setTuningError(null);
    try {
      const saved = await patchRouting({ tuning: tuningNumbers });
      queryClient.setQueryData(queryKeys.providerRouting(app), saved);
    } catch (e) {
      setTuningError(errorText(e));
    } finally {
      setTuningSaving(false);
    }
  };
  const restoreTuningDefaults = () => {
    setTuningDraft(toTuningDraft(DEFAULT_ROUTING_TUNING));
    setTuningError(null);
  };

  const port = proxy.data?.port ?? proxy.data?.listenPort;
  const hint = !config.enabled
    ? t("routing.offHint")
    : current
      ? t("routing.onHint", { port: port ?? "" })
      : t("routing.officialHint");
  const rows = current ? [current, ...queue] : queue;

  return (
    <div className="mb-4 rounded-lg border border-line bg-card">
      <div className="flex items-start gap-3 px-3 py-2.5">
        <Router
          size={15}
          aria-hidden
          className={
            config.enabled && current && appStatus ? "mt-0.5 text-run" : "mt-0.5 text-faint"
          }
        />
        <div className="min-w-0 flex-1">
          <p className="text-xs font-medium text-ink">{t("routing.title")}</p>
          <p className="mt-0.5 max-w-[68ch] text-xs text-muted">{hint}</p>
          {config.enabled && proxy.data?.lastError ? (
            <p className="mt-1 max-w-[68ch] text-xs text-danger">
              {t("routing.startError", { error: proxy.data.lastError })}
            </p>
          ) : null}
        </div>
        <Switch
          label={t("routing.toggle")}
          checked={config.enabled}
          disabled={routing.isLoading}
          loading={saving}
          onChange={(enabled) => void save({ enabled })}
        />
      </div>

      {config.enabled ? (
        <div className="border-t border-line px-3 py-2">
          <p className="mb-1 text-xs text-faint">{t("routing.order")}</p>
          {appStatus?.sticky ? (
            <p className="mb-1 rounded-md bg-warn-soft px-2 py-1 text-xs text-warn">
              {t("routing.sticky", {
                name:
                  relays.find((p) => p.id === appStatus.sticky?.providerId)?.name ??
                  appStatus.sticky.providerId,
                secs: appStatus.sticky.remainingSecs,
              })}
            </p>
          ) : null}
          <ol className="space-y-0.5">
            {rows.map((provider, index) => {
              const isCurrent = provider.id === currentId;
              const queueIndex = current ? index - 1 : index;
              return (
                <li
                  key={provider.id}
                  className="flex min-h-9 items-center gap-2 rounded-md px-1 hover:bg-hover"
                >
                  <span className="w-4 text-right text-xs tabular-nums text-faint">
                    {index + 1}
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <span className="truncate text-sm text-ink">{provider.name}</span>
                      {isCurrent ? <Tag tone="blue">{t("routing.current")}</Tag> : null}
                      {appStatus?.lastServedBy === provider.id && !isCurrent ? (
                        <Tag tone="green">{t("routing.lastServed")}</Tag>
                      ) : null}
                    </div>
                    <HealthLine health={health(provider.id)} />
                  </div>
                  {health(provider.id)?.state === "open" ? (
                    <Button
                      size="sm"
                      onClick={() => {
                        void ipc
                          .routerResetBreaker(app, provider.id)
                          .then(() =>
                            queryClient.invalidateQueries({ queryKey: queryKeys.routerStatus }),
                          );
                      }}
                    >
                      {t("routing.resume")}
                    </Button>
                  ) : null}
                  {isCurrent ? (
                    // 占位对齐：当前项不可移动/移除。
                    <span className="w-24 shrink-0" />
                  ) : (
                    <span className="flex w-24 shrink-0 justify-end">
                      <IconButton
                        icon={ArrowUp}
                        aria-label={t("routing.moveUp")}
                        title={t("routing.moveUp")}
                        disabled={saving || queueIndex === 0}
                        onClick={() => move(queueIndex, -1)}
                      />
                      <IconButton
                        icon={ArrowDown}
                        aria-label={t("routing.moveDown")}
                        title={t("routing.moveDown")}
                        disabled={saving || queueIndex === queue.length - 1}
                        onClick={() => move(queueIndex, 1)}
                      />
                      <IconButton
                        icon={X}
                        tone="danger"
                        aria-label={t("routing.remove")}
                        title={t("routing.remove")}
                        disabled={saving}
                        onClick={() =>
                          saveQueue(queue.filter((q) => q.id !== provider.id).map((q) => q.id))
                        }
                      />
                    </span>
                  )}
                </li>
              );
            })}
          </ol>
          {queue.length === 0 ? (
            <p className="mt-1 px-1 text-xs text-muted">{t("routing.queueEmpty")}</p>
          ) : null}
          {queue.length > 0 ? (
            <div className="mt-2 flex items-start gap-3 px-1">
              <div className="min-w-0 flex-1">
                <p className="text-xs text-ink">{t("routing.quotaAware")}</p>
                <p className="mt-0.5 max-w-[68ch] text-xs text-muted">
                  {t("routing.quotaAwareHint")}
                </p>
              </div>
              <Switch
                label={t("routing.quotaAware")}
                checked={config.strategy === "quota-aware"}
                disabled={saving}
                onChange={(on) => void save({ strategy: on ? "quota-aware" : "failover" })}
              />
            </div>
          ) : null}
          <div className="mt-2">
            {addable.length > 0 ? (
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <Button size="sm" icon={Plus} disabled={saving}>
                    {t("routing.add")}
                  </Button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="start">
                  {addable.map((p) => (
                    <DropdownMenuItem
                      key={p.id}
                      onSelect={() => saveQueue([...queue.map((q) => q.id), p.id])}
                    >
                      {p.name}
                    </DropdownMenuItem>
                  ))}
                </DropdownMenuContent>
              </DropdownMenu>
            ) : queue.length === 0 ? (
              <p className="px-1 text-xs text-faint">{t("routing.noCandidates")}</p>
            ) : null}
          </div>
        </div>
      ) : null}
      {config.enabled ? (
        <div className="border-t border-line px-3 py-2">
          <CollapseToggle open={advancedOpen} onToggle={() => setAdvancedOpen((v) => !v)}>
            {t("routing.advanced.title")}
          </CollapseToggle>
          {advancedOpen ? (
            <div className="mt-2 space-y-3 pl-1">
              <p className="text-[11px] text-faint">{t("routing.advanced.hint")}</p>
              <div className="grid grid-cols-2 gap-3">
                <LabeledField
                  label={t("routing.advanced.firstByteTimeoutSecs")}
                  error={
                    tuningFieldInvalid("firstByteTimeoutSecs")
                      ? t("routing.advanced.rangeError", {
                          min: TUNING_RANGES.firstByteTimeoutSecs[0],
                          max: TUNING_RANGES.firstByteTimeoutSecs[1],
                        })
                      : undefined
                  }
                  hint={t("routing.advanced.firstByteTimeoutSecsHint")}
                >
                  <Field
                    inputMode="numeric"
                    value={tuningDraft.firstByteTimeoutSecs}
                    onChange={(e) =>
                      setTuningDraft((d) => ({
                        ...d,
                        firstByteTimeoutSecs: e.target.value.replace(/\D/g, ""),
                      }))
                    }
                  />
                </LabeledField>
                <LabeledField
                  label={t("routing.advanced.idleTimeoutSecs")}
                  error={
                    tuningFieldInvalid("idleTimeoutSecs")
                      ? t("routing.advanced.rangeError", {
                          min: TUNING_RANGES.idleTimeoutSecs[0],
                          max: TUNING_RANGES.idleTimeoutSecs[1],
                        })
                      : undefined
                  }
                  hint={t("routing.advanced.idleTimeoutSecsHint")}
                >
                  <Field
                    inputMode="numeric"
                    value={tuningDraft.idleTimeoutSecs}
                    onChange={(e) =>
                      setTuningDraft((d) => ({
                        ...d,
                        idleTimeoutSecs: e.target.value.replace(/\D/g, ""),
                      }))
                    }
                  />
                </LabeledField>
                <LabeledField
                  label={t("routing.advanced.failureThreshold")}
                  error={
                    tuningFieldInvalid("failureThreshold")
                      ? t("routing.advanced.rangeError", {
                          min: TUNING_RANGES.failureThreshold[0],
                          max: TUNING_RANGES.failureThreshold[1],
                        })
                      : undefined
                  }
                  hint={t("routing.advanced.failureThresholdHint")}
                >
                  <Field
                    inputMode="numeric"
                    value={tuningDraft.failureThreshold}
                    onChange={(e) =>
                      setTuningDraft((d) => ({
                        ...d,
                        failureThreshold: e.target.value.replace(/\D/g, ""),
                      }))
                    }
                  />
                </LabeledField>
                <LabeledField
                  label={t("routing.advanced.cooldownSecs")}
                  error={
                    tuningFieldInvalid("cooldownSecs")
                      ? t("routing.advanced.rangeError", {
                          min: TUNING_RANGES.cooldownSecs[0],
                          max: TUNING_RANGES.cooldownSecs[1],
                        })
                      : undefined
                  }
                  hint={t("routing.advanced.cooldownSecsHint")}
                >
                  <Field
                    inputMode="numeric"
                    value={tuningDraft.cooldownSecs}
                    onChange={(e) =>
                      setTuningDraft((d) => ({
                        ...d,
                        cooldownSecs: e.target.value.replace(/\D/g, ""),
                      }))
                    }
                  />
                </LabeledField>
              </div>
              <div className="flex items-center gap-2">
                <Button size="sm" disabled={!tuningValid || tuningSaving} onClick={saveTuning}>
                  {t("ui.save")}
                </Button>
                <Button size="sm" icon={RotateCcw} onClick={restoreTuningDefaults}>
                  {t("routing.advanced.restoreDefaults")}
                </Button>
              </div>
              {tuningError ? <p className="text-xs text-danger">{tuningError}</p> : null}
            </div>
          ) : null}
        </div>
      ) : null}
      {config.enabled ? <RouteRequestLog app={app} /> : null}
      {error ? <p className="border-t border-line px-3 py-2 text-xs text-danger">{error}</p> : null}
    </div>
  );
}

/** 候选健康：未接管（无状态）不显示；熔断显示倒计时与最近错误。 */
function HealthLine({ health }: { health: RouteCandidateStatus | undefined }) {
  const { t } = useTranslation();
  if (!health) return null;
  const badge =
    health.state === "open" ? (
      <StatusBadge tone="danger">
        {t("routing.open", { secs: health.openRemainingSecs ?? 0 })}
      </StatusBadge>
    ) : health.state === "half-open" ? (
      <StatusBadge tone="warn">{t("routing.halfOpen")}</StatusBadge>
    ) : health.consecutiveFailures > 0 ? (
      <StatusBadge tone="warn">
        {t("routing.failures", { count: health.consecutiveFailures })}
      </StatusBadge>
    ) : (
      <StatusBadge tone="run">{t("routing.healthy")}</StatusBadge>
    );
  const exhausted =
    health.quotaExhaustedSecs != null ? (
      <StatusBadge tone="warn">
        {health.quotaExhaustedSecs >= 7200
          ? t("routing.quotaExhaustedHours", { hours: Math.ceil(health.quotaExhaustedSecs / 3600) })
          : t("routing.quotaExhaustedMinutes", {
              minutes: Math.max(1, Math.ceil(health.quotaExhaustedSecs / 60)),
            })}
      </StatusBadge>
    ) : null;
  const counts =
    health.served + health.failed > 0
      ? t("routing.counts", { served: health.served, failed: health.failed })
      : null;
  const tokens =
    health.totals.inputTokens + health.totals.outputTokens > 0
      ? t("routing.tokens", {
          input: formatTokens(health.totals.inputTokens),
          output: formatTokens(health.totals.outputTokens),
        })
      : null;
  return (
    <div className="flex min-w-0 items-center gap-2">
      {badge}
      {exhausted}
      {counts ? <span className="shrink-0 text-xs tabular-nums text-faint">{counts}</span> : null}
      {tokens ? <span className="shrink-0 text-xs tabular-nums text-faint">{tokens}</span> : null}
      {health.lastError && (health.state !== "closed" || health.consecutiveFailures > 0) ? (
        <span className="truncate text-xs text-faint" title={health.lastError}>
          {health.lastError}
        </span>
      ) : null}
    </div>
  );
}

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { BrandGlyph, isBrandName } from "../components/BrandIcon";
import { brandTextClass } from "../components/ClientChip";
import { PageHeader } from "../components/PageHeader";
import { PriceOverrideModal } from "../components/PriceOverrideModal";
import { Button, Card, ErrorBanner, SegmentedControl, Tag } from "../components/ui";
import { formatAt } from "../lib/datetime";
import { formatTokens } from "../lib/format";
import { ipc, type ProviderProfile, type UsageGroupRow, type UsageTrendPoint } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { formatBalance, headroomOf, quotaKind, useProviderQuota } from "../lib/quota";
import { cn, errorText } from "../lib/utils";

// ---------- 展示辅助 ----------

const RANGES = [
  { key: "7d", hours: 7 * 24 },
  { key: "30d", hours: 30 * 24 },
  { key: "90d", hours: 90 * 24 },
] as const;
type RangeKey = (typeof RANGES)[number]["key"];

/** 用量增量同步的跨挂载节流：进出用量页多次只在首次与超时后各跑一次。 */
const USAGE_SYNC_THROTTLE_MS = 5 * 60_000;
let lastUsageSyncAt = 0;

// 日期格式走 lib/datetime（"chartDay"）：这里原本自己 new 了一个
// Intl.DateTimeFormat(undefined)，读系统 locale 而不是界面语言。

/** 成本按量级取位：<0.01 保留 4 位（别把小额抹成 $0.00），常规 2 位。 */
function fmtCost(n: number): string {
  if (n === 0) return "$0";
  return n < 0.01 ? `$${n.toFixed(4)}` : `$${n.toFixed(2)}`;
}

/** Claude 项目目录名（如 -Users-me-proj）的尽力解码：仅供展示，title 保留原值。 */
function decodeProject(key: string): string {
  if (!key.startsWith("-")) return key;
  const parts = key.slice(1).split("-").filter(Boolean);
  return parts.length > 0 ? (parts.at(-1) ?? key) : key;
}

const totalTokens = (r: {
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
}) => r.inputTokens + r.outputTokens + r.cacheReadTokens + r.cacheCreationTokens;

// ---------- 组件 ----------

function StatTile({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <Card className="flex-1 px-4 py-3">
      <p className="text-[11px] text-muted">{label}</p>
      <p className="mt-0.5 text-lg font-semibold text-ink tabular-nums">{value}</p>
      {sub ? <p className="text-[11px] text-faint">{sub}</p> : null}
    </Card>
  );
}

/** recharts Tooltip 内容：贴应用卡片风格（扁平、无渐变）。 */
function TrendTooltip({
  active,
  payload,
  metric,
}: {
  active?: boolean;
  payload?: { payload: UsageTrendPoint }[];
  metric: "cost" | "tokens";
}) {
  const { t, i18n } = useTranslation();
  const point = payload?.[0]?.payload;
  if (!active || !point) return null;
  return (
    <div className="rounded-lg border border-line bg-card px-3 py-2 text-xs shadow-[var(--shadow-card)]">
      <p className="font-medium text-ink">{formatAt(i18n.language, "chartDay", point.bucketTs)}</p>
      <p className="mt-1 text-muted">
        {metric === "cost"
          ? t("usage.trend.tooltipCost", { cost: fmtCost(point.costUsd) })
          : t("usage.trend.tooltipTokens", { tokens: formatTokens(totalTokens(point)) })}
      </p>
      <p className="text-faint">{t("usage.trend.tooltipEvents", { count: point.events })}</p>
    </div>
  );
}

/** 日趋势：单序列柱状（单色相 = 量级），成本/Token 二选一，绝不双轴。 */
function TrendChart({ points, loading }: { points: UsageTrendPoint[]; loading?: boolean }) {
  const { t, i18n } = useTranslation();
  const [metric, setMetric] = useState<"cost" | "tokens">("cost");
  return (
    <Card className="px-4 py-3">
      <div className="mb-1 flex items-center justify-between">
        <p className="text-[11px] font-medium text-muted">{t("usage.trend.title")}</p>
        <div className="inline-flex rounded-md border border-line p-0.5">
          {(["cost", "tokens"] as const).map((key) => (
            <button
              key={key}
              type="button"
              onClick={() => setMetric(key)}
              className={cn(
                "rounded px-2 py-0.5 text-[11px] transition-colors",
                metric === key ? "bg-hover font-medium text-ink" : "text-muted hover:text-ink",
              )}
            >
              {t(`usage.trend.${key}`)}
            </button>
          ))}
        </div>
      </div>
      {/* 加载中与真的没有数据必须分开说：都渲染成「暂无数据」会让人以为这段周期
          确实零调用，从而去排查根本不存在的问题。 */}
      {loading ? (
        <p className="py-8 text-center text-xs text-faint">{t("usage.loading")}</p>
      ) : points.length === 0 ? (
        <p className="py-8 text-center text-xs text-faint">{t("usage.empty")}</p>
      ) : (
        <ResponsiveContainer width="100%" height={180}>
          {/* 柱间留更多空、平时略降不透明度、悬停回满色：30 根满饱和实心柱挤在一起太沉。 */}
          <BarChart
            data={points}
            margin={{ top: 8, right: 4, bottom: 0, left: 4 }}
            barCategoryGap="28%"
          >
            <CartesianGrid vertical={false} stroke="var(--color-line)" strokeDasharray="0" />
            <XAxis
              dataKey="bucketTs"
              tickFormatter={(ts: number) => formatAt(i18n.language, "chartDay", ts)}
              tick={{ fill: "var(--color-faint)", fontSize: 10 }}
              axisLine={false}
              tickLine={false}
              minTickGap={24}
            />
            <YAxis
              tickFormatter={(v: number) => (metric === "cost" ? fmtCost(v) : formatTokens(v))}
              tick={{ fill: "var(--color-faint)", fontSize: 10 }}
              axisLine={false}
              tickLine={false}
              width={52}
            />
            <Tooltip
              cursor={{ fill: "var(--color-hover)" }}
              content={<TrendTooltip metric={metric} />}
            />
            <Bar
              dataKey={metric === "cost" ? "costUsd" : (p: UsageTrendPoint) => totalTokens(p)}
              fill="var(--color-accent)"
              fillOpacity={0.82}
              activeBar={{ fillOpacity: 1 }}
              radius={[4, 4, 0, 0]}
              maxBarSize={28}
            />
          </BarChart>
        </ResponsiveContainer>
      )}
    </Card>
  );
}

/** 分组条形表：名称 + 单色量级条 + 成本/Token 数值（同 Logs 页 GroupBars 形制）。 */
function GroupCard({
  title,
  note,
  rows,
  loading,
  nameOf,
  titleOf,
  brandOf,
  onEditPrice,
}: {
  title: string;
  note?: string;
  rows: UsageGroupRow[];
  loading?: boolean;
  nameOf?: (row: UsageGroupRow) => string;
  titleOf?: (row: UsageGroupRow) => string;
  /** 行对应一个客户端时给出其 id：名字前带 logo、量级条用品牌色、名字不用等宽体。 */
  brandOf?: (row: UsageGroupRow) => string | null;
  /** 只有按模型的分组传：行尾出现「设置价格」（无定价的行更醒目）。 */
  onEditPrice?: (row: UsageGroupRow) => void;
}) {
  const { t } = useTranslation();
  const max = Math.max(1e-9, ...rows.map((r) => r.costUsd));
  const useTokenScale = rows.every((r) => r.costUsd === 0);
  const tokenMax = Math.max(1, ...rows.map(totalTokens));
  return (
    <Card className="flex-1 px-4 py-3">
      <p className="text-[11px] font-medium text-muted">{title}</p>
      {note ? <p className="mb-2 text-[11px] text-faint">{note}</p> : <div className="mb-2" />}
      {loading ? (
        <p className="py-3 text-xs text-faint">{t("usage.loading")}</p>
      ) : rows.length === 0 ? (
        <p className="py-3 text-xs text-faint">{t("usage.empty")}</p>
      ) : (
        <div className="space-y-2">
          {rows.map((row) => {
            const brand = brandOf?.(row) ?? null;
            return (
              <div key={row.key} className="flex items-center gap-2 text-xs">
                <span
                  className={cn(
                    "flex items-center gap-1.5 truncate text-ink",
                    // 行尾多一列「设置价格」时让出宽度，免得条形被挤没。
                    onEditPrice ? "w-32" : "w-40",
                    !brand && "font-mono",
                  )}
                  title={titleOf?.(row) ?? row.key}
                >
                  {brand && isBrandName(brand) ? (
                    <span className={cn("shrink-0", brandTextClass(brand))}>
                      <BrandGlyph name={brand} size={13} />
                    </span>
                  ) : null}
                  <span className="truncate">{nameOf?.(row) ?? row.key}</span>
                </span>
                <span className="h-1.5 min-w-0 flex-1 rounded-full bg-app">
                  <span
                    className={cn(
                      "block h-full rounded-full",
                      // 客户端行的条用它自己的品牌色（与 logo 同一套色调），其余行用主色。
                      brand ? cn("bg-current", brandTextClass(brand)) : "bg-accent",
                    )}
                    style={{
                      width: `${
                        (useTokenScale ? totalTokens(row) / tokenMax : row.costUsd / max) * 100
                      }%`,
                    }}
                  />
                </span>
                <span className="w-16 text-right font-mono text-muted tabular-nums">
                  {formatTokens(totalTokens(row))}
                </span>
                <span className="w-16 text-right font-mono text-ink tabular-nums">
                  {row.priced ? fmtCost(row.costUsd) : "-"}
                </span>
                {onEditPrice ? (
                  // 定宽右对齐：有无「无定价 / 自定义」提示时，前面的 token 与成本列不跟着挪。
                  <span className="flex w-[112px] shrink-0 items-center justify-end gap-1.5 whitespace-nowrap">
                    {!row.priced ? (
                      <span className="text-[11px] text-faint">{t("usage.unpriced")}</span>
                    ) : null}
                    {row.overridden ? <Tag>{t("usage.price.custom")}</Tag> : null}
                    <button
                      type="button"
                      onClick={() => onEditPrice(row)}
                      aria-label={t("usage.price.setNamed", { model: row.key })}
                      className={cn(
                        "shrink-0 rounded px-1.5 py-0.5 text-[11px] transition-colors hover:bg-hover",
                        row.priced ? "text-muted hover:text-ink" : "text-accent",
                      )}
                    >
                      {t("usage.price.set")}
                    </button>
                  </span>
                ) : !row.priced ? (
                  <span className="text-[11px] text-faint">{t("usage.unpriced")}</span>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </Card>
  );
}

/** 额度联动：当前生效供应商若支持额度查询，并列展示「额度剩余 / 余额」与本页「按量估算消费」，
 * 两者口径不同需标注。 */
function QuotaCard({ appCosts }: { appCosts: Map<string, number> }) {
  const { t } = useTranslation();
  const providers = useQuery({ queryKey: queryKeys.providers, queryFn: ipc.providersList });
  const current = useQuery({ queryKey: queryKeys.providerCurrent, queryFn: ipc.providerCurrent });

  const targets = (["claude", "codex"] as const).flatMap((app) => {
    const id = current.data?.[app];
    const profile = providers.data?.find((p) => p.id === id);
    // quotaKind 对官方订阅返回 null，这里只会剩第三方供应商（套餐/余额）。
    return profile && quotaKind(profile) ? [{ app, profile }] : [];
  });
  if (targets.length === 0) return null;

  return (
    <Card className="px-4 py-3">
      <p className="text-[11px] font-medium text-muted">{t("usage.quota.title")}</p>
      <p className="mb-2 text-[11px] text-faint">{t("usage.quota.note")}</p>
      <div className="space-y-2.5">
        {targets.map(({ app, profile }) => (
          <QuotaRow key={app} app={app} profile={profile} cost={appCosts.get(app) ?? 0} />
        ))}
      </div>
    </Card>
  );
}

function QuotaRow({
  app,
  profile,
  cost,
}: {
  app: "claude" | "codex";
  profile: ProviderProfile;
  cost: number;
}) {
  const { t } = useTranslation();
  const { query } = useProviderQuota(profile, { auto: true });
  const quota = query.data;
  return (
    <div className="flex items-center gap-3 text-xs">
      <span className="w-40 truncate text-ink" title={profile.name}>
        {t(`usage.app.${app}`)} · {profile.name}
      </span>
      {quota?.success ? (
        <span className="flex min-w-0 flex-1 flex-wrap items-center gap-x-3 gap-y-0.5">
          {quota.tiers.map((tier) => (
            <span key={tier.window} className="flex items-center gap-1.5">
              <span className="text-faint">{t(`usage.quota.window.${tier.window}`)}</span>
              <span className="font-mono text-ink tabular-nums">
                {t("usage.quota.left", { percent: headroomOf(tier.utilization) })}
              </span>
            </span>
          ))}
          {quota.balances.map((balance) => (
            <span key={balance.currency} className="font-mono text-ink tabular-nums">
              {t("providers.quotaBalance", { amount: formatBalance(balance) })}
            </span>
          ))}
        </span>
      ) : (
        <span className="flex-1 text-faint">
          {quota || query.isError ? t("usage.quota.failed") : t("usage.quota.loading")}
        </span>
      )}
      <span className="font-mono text-muted tabular-nums">
        {t("usage.quota.estimated", { cost: fmtCost(cost) })}
      </span>
    </div>
  );
}

/** 用量页：本地会话文件解析出的 token 用量与估算成本（非官方账单）。 */
export function Usage() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [range, setRange] = useState<RangeKey>("30d");
  const hours = RANGES.find((r) => r.key === range)?.hours ?? 720;
  const [pricingModel, setPricingModel] = useState<string | null>(null);

  // 打开页面自动增量同步一次；完成后失效所有 usage 查询让数据落地
  const sync = useMutation({
    mutationFn: ipc.usageSync,
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.usageAll });
    },
  });
  const syncStartedRef = useRef(false);
  const syncMutate = sync.mutate;
  useEffect(() => {
    // 增量同步会扫一遍全部会话文件：每次进页面都跑一次没必要。同一次挂载内不重复
    // （syncStartedRef），跨挂载按 5 分钟节流（模块级 lastUsageSyncAt）。
    if (syncStartedRef.current) return;
    syncStartedRef.current = true;
    if (Date.now() - lastUsageSyncAt < USAGE_SYNC_THROTTLE_MS) return;
    lastUsageSyncAt = Date.now();
    syncMutate();
  }, [syncMutate]);

  const summary = useQuery({
    queryKey: queryKeys.usageSummary(hours),
    queryFn: () => ipc.usageSummary(hours),
    retry: false,
  });
  const trend = useQuery({
    queryKey: queryKeys.usageTrend(hours),
    queryFn: () => ipc.usageTrend(hours, -new Date().getTimezoneOffset()),
    retry: false,
  });
  const byModel = useQuery({
    queryKey: queryKeys.usageByModel(hours),
    queryFn: () => ipc.usageByModel(hours),
    retry: false,
  });
  const byProject = useQuery({
    queryKey: queryKeys.usageByProject(hours),
    queryFn: () => ipc.usageByProject(hours),
    retry: false,
  });
  const byApp = useQuery({
    queryKey: queryKeys.usageByApp(hours),
    queryFn: () => ipc.usageByProvider(hours),
    retry: false,
  });

  const data = summary.data;
  const appCosts = new Map((byApp.data ?? []).map((row) => [row.key, row.costUsd]));

  // 五个查询共用一条横幅：它们都是 retry:false，任一读失败后下面全部退化成
  // 「-」和空列表，看起来与「这段时间没用过」一模一样。报出第一条失败即可，
  // 多半同源（同一个 db / 同一次 IPC）。
  const failedQuery = [summary, trend, byModel, byProject, byApp].find((q) => q.isError);

  return (
    <div>
      <PageHeader
        title={t("usage.title")}
        actions={
          <div className="flex items-center gap-2">
            {/* 手动同步：节流只挡自动触发；刚跑完一个大会话想立刻看数字，不该等 5 分钟。 */}
            <Button
              size="sm"
              icon={RefreshCw}
              loading={sync.isPending}
              disabled={sync.isPending}
              onClick={() => {
                lastUsageSyncAt = Date.now();
                sync.mutate();
              }}
            >
              {sync.isPending ? t("usage.syncing") : t("usage.syncNow")}
            </Button>
            <SegmentedControl
              data-tour="usage-range"
              // 无障碍名称复用导览里同一控件的标题，不另造一份说同一件事的文案。
              label={t("tour.usage.range.title")}
              value={range}
              options={RANGES.map(({ key }) => ({ value: key, label: t(`usage.range.${key}`) }))}
              onChange={setRange}
            />
          </div>
        }
      />

      <p className="mb-4 text-[11px] text-faint">{t("usage.disclaimer")}</p>

      <ErrorBanner
        message={failedQuery ? t("usage.loadError", { error: errorText(failedQuery.error) }) : null}
      />

      {/* 同步整体失败。部分失败走下面那条「N 个文件解析失败」，唯独全盘失败
          此前没有出口：页面只显示「暂无数据」，分不清是没用过还是解析崩了。 */}
      <ErrorBanner
        message={sync.isError ? t("usage.syncFailed", { error: errorText(sync.error) }) : null}
      />

      {sync.data && sync.data.errors.length > 0 ? (
        <p className="mb-3 text-xs text-muted">
          {t("usage.syncErrors", { count: sync.data.errors.length })}
        </p>
      ) : null}

      <div className="space-y-3">
        <div data-tour="usage-stats" className="flex flex-wrap gap-3">
          <StatTile
            label={t("usage.stat.cost")}
            value={data ? fmtCost(data.costUsd) : "-"}
            sub={
              data && data.unpricedEvents > 0
                ? t("usage.stat.unpricedNote", { count: data.unpricedEvents })
                : t("usage.stat.costNote")
            }
          />
          <StatTile
            label={t("usage.stat.input")}
            value={data ? formatTokens(data.inputTokens) : "-"}
          />
          <StatTile
            label={t("usage.stat.output")}
            value={data ? formatTokens(data.outputTokens) : "-"}
          />
          <StatTile
            label={t("usage.stat.cacheRead")}
            value={data ? formatTokens(data.cacheReadTokens) : "-"}
            sub={
              data && data.cacheCreationTokens > 0
                ? t("usage.stat.cacheCreation", {
                    tokens: formatTokens(data.cacheCreationTokens),
                  })
                : undefined
            }
          />
          <StatTile label={t("usage.stat.events")} value={data ? String(data.events) : "-"} />
        </div>

        <TrendChart points={trend.data ?? []} loading={trend.isLoading} />

        <div data-tour="usage-breakdown" className="flex flex-wrap gap-3">
          <GroupCard
            title={t("usage.byModel")}
            rows={byModel.data ?? []}
            loading={byModel.isLoading}
            nameOf={(row) => row.displayName ?? row.key}
            titleOf={(row) => row.key}
            onEditPrice={(row) => setPricingModel(row.key)}
          />
          <GroupCard
            title={t("usage.byApp")}
            note={t("usage.byAppNote")}
            rows={(byApp.data ?? []).map((row) => ({ ...row, priced: true }))}
            loading={byApp.isLoading}
            nameOf={(row) => t(`usage.app.${row.key}`, { defaultValue: row.key })}
            brandOf={(row) => row.key}
          />
        </div>

        <GroupCard
          title={t("usage.byProject")}
          note={t("usage.byProjectNote")}
          rows={byProject.data ?? []}
          loading={byProject.isLoading}
          nameOf={(row) => decodeProject(row.key)}
        />

        <QuotaCard appCosts={appCosts} />
      </div>

      {pricingModel ? (
        <PriceOverrideModal modelId={pricingModel} onClose={() => setPricingModel(null)} />
      ) : null}
    </div>
  );
}

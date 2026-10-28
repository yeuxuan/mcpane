import { useQuery } from "@tanstack/react-query";
import { Check, ChevronRight, TriangleAlert } from "lucide-react";
import { memo, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { BrandGlyph, isBrandName } from "../components/BrandIcon";
import { brandTextClass } from "../components/ClientChip";
import { HistoryPanel } from "../components/HistoryPanel";
import { PageHeader } from "../components/PageHeader";
import {
  Card,
  CodeBlock,
  JsonView,
  Modal,
  SearchInput,
  SegmentedControl,
  StatusBadge,
  Tag,
} from "../components/ui";
import { formatAt } from "../lib/datetime";
import {
  type HubConnection,
  ipc,
  type LogEntry,
  type LogGroupCount,
  type Workspace,
} from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { cn, shortConnectionId } from "../lib/utils";

type Tab = "requests" | "history";
type StatusFilter = "all" | "ok" | "fail";

/** 工具列要拆出「短名 + 服务器」两截，所以 id→slug 也要备着（见 shortToolName）。 */
type ServerMeta = { name: string; slug: string };

/** 连接在表格里拆成「客户端 + 令牌前缀」两截，在详情里拼回一行。 */
type ConnectionParts = { client: string; token: string | null };

// 时刻格式走 lib/datetime：此前这里自己 new 了一个 Intl.DateTimeFormat(undefined)，
// 读的是系统 locale，于是中文界面里会出现 "08:41 PM"。formatAt 内部按
// 「用途 + 语言」缓存格式化器，模块级复用的性能考量同样成立。

function prettyJson(raw: string | null | undefined): string | null {
  if (!raw) return null;
  try {
    return JSON.stringify(JSON.parse(raw), null, 2);
  } catch {
    return raw;
  }
}

function formatBytes(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  return `${(n / 1024).toFixed(1)} KB`;
}

/**
 * 平铺暴露的工具名是 `<slug>__<tool>`，服务器名又单独占一列——同一个词在一行里
 * 写两遍，还把主列撑到换行。这里剥掉前缀，服务器名降级成工具名后面的弱色注脚。
 * 剥不掉（网关模式的 tools/call、别名）就原样展示，不猜。
 */
function shortToolName(name: string, slug: string | undefined): string {
  if (!slug) return name;
  const prefix = `${slug}__`;
  return name.startsWith(prefix) ? name.slice(prefix.length) : name;
}

/**
 * 表格按自然日分段。时刻列只写 HH:MM:SS（日期逐行重复是噪音），但「最近 500 条」
 * 在日常使用下常常横跨好几天——没有分段，前天的失败和一分钟前的失败长得一模一样。
 * 日期因此只在跨天处出现一次，顺带把当天的调用量和失败数说清楚。
 */
type DayRow = { kind: "day"; key: string; ts: number; count: number; errors: number };
type TableRow = DayRow | { kind: "entry"; entry: LogEntry; first: boolean };

function dayKey(ts: number): string {
  return new Date(ts * 1000).toDateString();
}

function groupByDay(entries: LogEntry[]): TableRow[] {
  const out: TableRow[] = [];
  let current: DayRow | null = null;
  let seen = 0;
  for (const entry of entries) {
    const key = dayKey(entry.ts);
    if (current === null || current.key !== key) {
      current = { kind: "day", key, ts: entry.ts, count: 0, errors: 0 };
      out.push(current);
    }
    current.count += 1;
    if (!entry.success) current.errors += 1;
    // `first` 标出唯一的 Tab 停靠点。不拿 id 比：同文件的 key 已经防御过 id 为空，
    // 真空时每一行都会等于 ""，roving tabindex 一下子退回 500 个停靠点。
    out.push({ kind: "entry", entry, first: seen === 0 });
    seen += 1;
  }
  return out;
}

function connectionParts(id: string | null, connections: HubConnection[]): ConnectionParts | null {
  if (!id) return null;
  const conn = connections.find((c) => c.clientConnectionId === id);
  if (!conn) return { client: shortConnectionId(id), token: null };
  return { client: conn.clientType, token: conn.tokenPrefix };
}

function connectionLabel(id: string | null, connections: HubConnection[]): string {
  const parts = connectionParts(id, connections);
  if (!parts) return "—";
  return parts.token ? `${parts.client} · ${parts.token}…` : parts.client;
}

/** 结果列：成功安静（一个勾），失败才带色块和错误类别——扫表时先看见的必须是异常。 */
function ResultCell({ entry }: { entry: LogEntry }) {
  const { t } = useTranslation();
  if (entry.success) {
    return (
      /* 透明描边 + 同样的内距：失败那颗带框的标签不能让整行比成功行高 2px。 */
      <span
        className="inline-flex items-center border border-transparent px-1.5 py-0.5 text-run"
        title={t("logs.result.ok")}
      >
        <Check size={14} strokeWidth={2.5} aria-hidden />
        <span className="sr-only">{t("logs.result.ok")}</span>
      </span>
    );
  }
  return (
    <span className="inline-flex max-w-full items-center gap-1 rounded-md border border-danger-line bg-danger-soft px-1.5 py-0.5 text-[11px] font-medium text-danger">
      <TriangleAlert size={11} strokeWidth={2.5} aria-hidden className="shrink-0" />
      <span className="truncate">{entry.errorCategory ?? t("logs.result.failed")}</span>
    </span>
  );
}

/**
 * 上下键在数据行之间移动焦点。分组行不带 `data-log-row`，因此天然被跳过——
 * 键盘走位与肉眼扫行的节奏一致，不会在日期标题上停一拍。
 *
 * 同时挪走 roving tabindex：表里只有一个按钮占 Tab 停靠点，否则 Tab 要按 500 次
 * 才能走出这张表。ARIA 的 grid/listbox 都是这个模式——进出用 Tab，内部用方向键。
 */
function moveRowFocus(from: HTMLElement, delta: number): void {
  const rows = from.closest("tbody")?.querySelectorAll<HTMLElement>("[data-log-row]");
  if (!rows) return;
  const list = Array.from(rows);
  const next = list[list.indexOf(from) + delta];
  if (!next) return;
  from.tabIndex = -1;
  next.tabIndex = 0;
  next.focus();
}

/** 行级 memo：500 行的表格在筛选逐字输入 / 5s 轮询时只重渲染变化的行。 */
const LogRow = memo(function LogRow({
  entry,
  server,
  connection,
  slow,
  tabStop,
  selected,
  onOpen,
}: {
  entry: LogEntry;
  server: ServerMeta | null;
  connection: ConnectionParts | null;
  /** 落在本屏耗时分布的尾部（见 slowThreshold）：数字改用告警色，扫表时直接看得见。 */
  slow: boolean;
  /** 本表唯一的 Tab 停靠点（见 moveRowFocus）：只有首行为 true。 */
  tabStop: boolean;
  selected: boolean;
  onOpen: (entry: LogEntry) => void;
}) {
  const { t, i18n } = useTranslation();
  const name = entry.exposedName ?? entry.method;
  return (
    /* 整行可点是鼠标的便利路径，键盘走的是行尾那个真按钮（Tab 进来、上下键换行、
       回车打开）。`tr` 不能顶着 role="button" 冒充控件——那样屏幕阅读器会丢掉
       表格的行列结构，而这张表的列本身就是信息。 */
    <tr
      className={cn(
        "group cursor-pointer border-b border-line transition-colors last:border-0",
        // 全项目唯一一处 `:has()`。已在 WebKit 实测：选择器生效，且方向键换行时
        // `moveRowFocus` 的编程 focus() 仍匹配 :focus-visible（焦点环不会消失）。
        "hover:bg-hover has-[button:focus-visible]:bg-hover",
        selected && "bg-accent-soft",
      )}
      onClick={() => onOpen(entry)}
    >
      {/* 左缘状态条：失败恒为红，其次才让选中态染主色——沿左缘扫一遍就能把
          出错的调用挑出来，不用逐行读最右的结果列。 */}
      <td
        className={cn(
          "whitespace-nowrap border-l-2 py-1.5 pr-3 pl-3.5 font-mono text-[11px] text-faint tabular-nums",
          entry.success
            ? selected
              ? "border-l-accent"
              : "border-l-transparent"
            : "border-l-danger",
        )}
      >
        {formatAt(i18n.language, "clock", entry.ts)}
      </td>
      <td className="w-[44%] px-3 py-1.5">
        <div className="flex items-baseline gap-1.5">
          <span className="truncate font-mono text-[12px] text-ink" title={name}>
            {shortToolName(name, server?.slug)}
          </span>
          {server ? (
            <span className="shrink-0 truncate text-[11px] text-faint" title={server.name}>
              {server.name}
            </span>
          ) : null}
        </div>
      </td>
      {/* 窄窗口下先压令牌前缀、后压客户端名：此前反过来，960px 时会截出
          「cla… 1f3e4025」——把次要的那截留全了，主要的那截读不出来。 */}
      <td className="w-[20%] px-3 py-1.5">
        {connection ? (
          <div className="flex items-baseline gap-1.5">
            <span className="max-w-[62%] shrink-0 truncate text-[12px] text-muted">
              {connection.client}
            </span>
            {connection.token ? (
              <span className="min-w-0 truncate font-mono text-[10px] text-faint">
                {connection.token}
              </span>
            ) : null}
          </div>
        ) : (
          <span className="text-[12px] text-faint">—</span>
        )}
      </td>
      <td
        className={cn(
          "w-[5.5rem] px-3 py-1.5 text-right font-mono text-[11px] tabular-nums",
          slow ? "text-warn" : "text-muted",
        )}
      >
        {entry.durationMs}ms
      </td>
      <td className="w-[6.5rem] px-3 py-1.5">
        <ResultCell entry={entry} />
      </td>
      {/* 箭头常驻（弱色），hover 才变实。此前它 opacity-0 藏着，静止的表格没有
          任何"这行能点"的线索——上一版行有 hover 高亮却没有 onClick，用户已经
          学会了"这里点不动"，光把 onClick 补上还不够。 */}
      <td className="w-8 pr-3">
        <button
          type="button"
          data-log-row
          tabIndex={tabStop ? 0 : -1}
          className="flex h-6 w-6 items-center justify-center rounded-md text-faint/50 transition-colors hover:bg-hover hover:text-ink group-hover:text-ink"
          aria-label={t("logs.detail.open", { name })}
          onClick={(event) => {
            event.stopPropagation();
            onOpen(entry);
          }}
          onKeyDown={(event) => {
            if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
            event.preventDefault();
            moveRowFocus(event.currentTarget, event.key === "ArrowDown" ? 1 : -1);
          }}
        >
          <ChevronRight size={14} strokeWidth={2} aria-hidden />
        </button>
      </td>
    </tr>
  );
});

/** 日期分隔行：整行一格，压住表格节奏；当天有失败时把数目直接写出来。 */
function DayHeaderRow({ row }: { row: DayRow }) {
  const { t, i18n } = useTranslation();
  const now = new Date();
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  const key = row.key;
  const label =
    key === now.toDateString()
      ? t("logs.day.today")
      : key === yesterday.toDateString()
        ? t("logs.day.yesterday")
        : formatAt(i18n.language, "day", row.ts);
  return (
    <tr>
      <td colSpan={6} className="border-y border-line bg-app px-4 py-1.5">
        <span className="text-[11px] font-semibold text-muted">{label}</span>
        <span className="ml-2 text-[11px] text-faint tabular-nums">
          {row.errors > 0
            ? t("logs.group.summaryWithErrors", { count: row.count, errors: row.errors })
            : t("logs.group.summary", { count: row.count })}
        </span>
      </td>
    </tr>
  );
}

/** 统计格：四格共处一张卡，靠竖线分栏——四张独立卡片会在窄窗口下散成两行半。 */
function StatTile({
  label,
  value,
  sub,
  subTone,
}: {
  label: string;
  value: string;
  sub?: string;
  /** 只给注脚上色：96.4% 本身是好消息，整块染红会把量级读反。 */
  subTone?: "danger";
}) {
  return (
    <div className="min-w-0 border-l border-line px-4 py-2.5 first:border-l-0">
      <p className="truncate text-[11px] text-faint">{label}</p>
      <p className="mt-0.5 truncate text-[17px] font-semibold text-ink tabular-nums">{value}</p>
      <p
        className={cn("truncate text-[11px]", subTone === "danger" ? "text-danger" : "text-faint")}
      >
        {sub ?? " "}
      </p>
    </div>
  );
}

/** 分组条形表：名称 + 单色细条（量级，同一度量单一色相）+ 数值文本。 */
function GroupBars({
  title,
  groups,
  nameOf,
  brandOf,
}: {
  title: string;
  groups: LogGroupCount[];
  nameOf?: (key: string) => string;
  /** 行对应一个客户端时给出其类型：名字前带 logo、细条用品牌色（一页只此一栏）。 */
  brandOf?: (key: string) => string | null;
}) {
  const { t } = useTranslation();
  const max = Math.max(1, ...groups.map((g) => g.count));
  return (
    <div className="min-w-0 bg-card px-4 py-2.5">
      <p className="mb-2 text-[11px] font-medium text-muted">{title}</p>
      {groups.length === 0 ? (
        <p className="py-3 text-xs text-faint">{t("logs.stats.none")}</p>
      ) : (
        <div className="space-y-1">
          {groups.map((group) => {
            const brand = brandOf?.(group.key) ?? null;
            return (
              <div key={group.key} className="flex items-center gap-2.5 text-xs">
                <span
                  className="flex w-28 shrink-0 items-center gap-1.5 text-[12px] text-ink"
                  title={nameOf?.(group.key) ?? group.key}
                >
                  {brand && isBrandName(brand) ? (
                    <span className={cn("shrink-0", brandTextClass(brand))}>
                      <BrandGlyph name={brand} size={12} />
                    </span>
                  ) : null}
                  <span className="truncate">{nameOf?.(group.key) ?? group.key}</span>
                </span>
                <span className="h-1.5 min-w-0 flex-1 rounded-full bg-app">
                  <span
                    className={cn(
                      "block h-full rounded-full",
                      brand && isBrandName(brand)
                        ? cn("bg-current", brandTextClass(brand))
                        : "bg-accent",
                    )}
                    style={{ width: `${(group.count / max) * 100}%` }}
                  />
                </span>
                <span className="w-10 shrink-0 text-right font-mono text-[11px] text-muted tabular-nums">
                  {group.count}
                </span>
                <span className="w-12 shrink-0 text-right font-mono text-[11px] text-danger tabular-nums">
                  {group.errors > 0 ? t("logs.stats.errorsSuffix", { count: group.errors }) : ""}
                </span>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

/** 统计时间窗选项（后端 hours clamp 在 1..720）。 */
const STAT_WINDOWS = [
  { key: "1h", hours: 1 },
  { key: "24h", hours: 24 },
  { key: "7d", hours: 7 * 24 },
] as const;
type StatWindow = (typeof STAT_WINDOWS)[number]["key"];

/** 聚合仪表（SQL 侧统计整个窗口，与下方「最近 500 条」表格独立）。 */
function StatsSection({
  serverById,
  connections,
}: {
  serverById: Map<string, ServerMeta>;
  connections: HubConnection[];
}) {
  const { t } = useTranslation();
  const [statWindow, setStatWindow] = useState<StatWindow>("24h");
  const hours = STAT_WINDOWS.find((w) => w.key === statWindow)?.hours ?? 24;
  const stats = useQuery({
    queryKey: queryKeys.logStats(hours),
    queryFn: () => ipc.logsStats(hours),
    ...livePoll(5000),
  });
  const data = stats.data;
  // 首次加载（尚无任何数据）时保持原「无统计不占位」行为；
  // 一旦渲染过，窗口切换器必须常驻——空窗口也要能切回其他窗口。
  if (!data) return null;

  const serverName = (id: string) => serverById.get(id)?.name ?? id;
  // 连接维度落在日志里的是 conn_a1b2c3 这种 id，对用户是天书——
  // 复用表格那边同一个标签函数，扫一眼就知道是哪个客户端。
  const connectionName = (id: string) => connectionLabel(id, connections);
  const successRate = data.total > 0 ? ((data.success / data.total) * 100).toFixed(1) : "0.0";
  const failed = data.total - data.success;
  const windowOptions = STAT_WINDOWS.map(({ key }) => ({
    value: key,
    label: t(`logs.stats.windowOption.${key}`),
  }));
  return (
    <Card data-tour="logs-stats" className="mb-4 overflow-hidden">
      <div className="flex items-center justify-between gap-3 px-4 py-2">
        <p className="text-xs font-medium text-muted">{t("logs.stats.window")}</p>
        <SegmentedControl
          label={t("logs.stats.window")}
          value={statWindow}
          options={windowOptions}
          onChange={setStatWindow}
        />
      </div>
      {data.total === 0 ? (
        <p className="border-t border-line px-4 py-6 text-center text-xs text-faint">
          {t("logs.stats.emptyWindow")}
        </p>
      ) : (
        <>
          <div className="grid grid-cols-4 border-t border-line">
            <StatTile label={t("logs.stats.total")} value={String(data.total)} />
            <StatTile
              label={t("logs.stats.successRate")}
              value={`${successRate}%`}
              sub={failed > 0 ? t("logs.stats.failedCount", { count: failed }) : undefined}
              subTone={failed > 0 ? "danger" : undefined}
            />
            <StatTile label={t("logs.stats.avg")} value={`${Math.round(data.avgDurationMs)}ms`} />
            <StatTile label={t("logs.stats.p95")} value={`${data.p95DurationMs}ms`} />
          </div>
          {/* gap-px + bg-line：两块分组表之间只留一条发丝线，不再各自套一张卡。 */}
          <div className="grid gap-px border-t border-line bg-line md:grid-cols-2">
            <GroupBars
              title={t("logs.stats.byServer")}
              groups={data.byServer}
              nameOf={serverName}
            />
            <GroupBars title={t("logs.stats.byMethod")} groups={data.byMethod} />
            {/* 工具维度回答「接进来的工具到底有没有被用」，连接维度回答
                「哪个客户端在懒调用」——这两问以前只能靠感觉。 */}
            <GroupBars title={t("logs.stats.byTool")} groups={data.byTool} />
            <GroupBars
              title={t("logs.stats.byConnection")}
              groups={data.byConnection}
              nameOf={connectionName}
              brandOf={(id) =>
                connections.find((c) => c.clientConnectionId === id)?.clientType ?? null
              }
            />
          </div>
        </>
      )}
    </Card>
  );
}

function MetaItem({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <p className="text-[11px] text-faint">{label}</p>
      <p className="mt-0.5 truncate font-mono text-xs text-ink" title={value}>
        {value}
      </p>
    </div>
  );
}

function PayloadBlock({
  title,
  raw,
  empty,
  truncatedLabel,
  maxHeight,
}: {
  title: string;
  raw: string | null | undefined;
  empty: string;
  truncatedLabel: string;
  maxHeight: string;
}) {
  const pretty = prettyJson(raw);
  const truncated = Boolean(raw?.includes("…(truncated)") || raw?.includes("…(+"));
  return (
    <section>
      <div className="mb-1.5 flex items-center gap-2">
        <p className="text-[11px] font-medium text-muted">{title}</p>
        {truncated ? <Tag tone="amber">{truncatedLabel}</Tag> : null}
      </div>
      {pretty ? (
        <JsonView code={pretty} maxHeightClassName={maxHeight} />
      ) : (
        <p className="rounded-lg border border-dashed border-line px-3 py-4 text-xs text-faint">
          {empty}
        </p>
      )}
    </section>
  );
}

function LogDetailModal({
  entry,
  servers,
  connections,
  workspaces,
  onClose,
}: {
  entry: LogEntry;
  servers: { id: string; name: string }[];
  connections: HubConnection[];
  workspaces: Workspace[];
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  const detail = useQuery({
    queryKey: queryKeys.logDetail(entry.id),
    queryFn: () => ipc.logsGet(entry.id),
    retry: false,
  });
  const full = detail.data ?? entry;
  const name = full.exposedName ?? full.method;
  const server = servers.find((s) => s.id === (full.serverId ?? ""))?.name ?? full.serverId ?? "—";
  const workspace =
    workspaces.find((w) => w.id === (full.workspaceId ?? ""))?.name ?? full.workspaceId ?? "—";
  const conn = connectionParts(full.clientConnectionId, connections);
  const hasPayload =
    full.requestSummary != null || full.responseSummary != null || full.errorMessage != null;
  const legacy = detail.isSuccess && !hasPayload;

  return (
    <Modal title={t("logs.detail.title")} size="xl" onClose={onClose}>
      <div className="space-y-4">
        {/* 标题行：工具名领读，状态与耗时贴着它——这三样决定用户要不要往下看。 */}
        <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
          <h3 className="font-mono text-[15px] font-semibold break-all text-ink">{name}</h3>
          {full.success ? (
            <StatusBadge tone="run">{t("logs.result.ok")}</StatusBadge>
          ) : (
            <StatusBadge tone="danger">{full.errorCategory ?? t("logs.result.failed")}</StatusBadge>
          )}
          <Tag tone="slate">{full.method}</Tag>
          <span className="font-mono text-xs text-muted tabular-nums">{full.durationMs}ms</span>
          {conn ? (
            <span className="ml-auto inline-flex items-center gap-1.5 text-xs text-muted">
              {/* client 是连接上报的原样字符串（未知客户端为 custom，连接已删时
                  是短 id），认得出的才画 logo。 */}
              {isBrandName(conn.client) ? (
                <span className="text-ink">
                  <BrandGlyph name={conn.client} size={14} />
                </span>
              ) : null}
              {connectionLabel(full.clientConnectionId, connections)}
            </span>
          ) : null}
        </div>

        <div className="grid grid-cols-2 gap-x-4 gap-y-3 rounded-lg border border-line bg-app px-4 py-3 sm:grid-cols-3">
          <MetaItem
            label={t("logs.detail.time")}
            value={formatAt(i18n.language, "full", full.ts)}
          />
          <MetaItem label={t("logs.detail.server")} value={server} />
          {/* 连接不再在这里重复一遍：标题行右侧已经带着客户端 logo 写过一次。 */}
          <MetaItem label={t("logs.detail.workspace")} value={workspace} />
          <MetaItem label={t("logs.detail.requestId")} value={full.requestId} />
          <MetaItem label={t("logs.detail.session")} value={full.sessionId ?? "—"} />
          <MetaItem
            label={t("logs.detail.size")}
            value={t("logs.detail.sizePair", {
              inbound: formatBytes(full.requestSize),
              outbound: formatBytes(full.responseSize),
            })}
          />
          <MetaItem label={t("logs.detail.retry")} value={String(full.retryCount)} />
        </div>

        {detail.isError ? (
          <p className="text-sm text-danger">{t("logs.detail.loadError")}</p>
        ) : null}

        {full.errorMessage ? (
          <section>
            <p className="mb-1.5 text-[11px] font-medium text-danger">{t("logs.detail.error")}</p>
            <CodeBlock
              code={full.errorMessage}
              className="max-h-40 overflow-auto border-danger/30"
            />
          </section>
        ) : null}

        {/* 参数与返回上下堆叠而非左右并排：参数通常几行、返回常有几十行，
            并排会让左栏空一大片，右栏还照样要滚。 */}
        {detail.isPending ? (
          <p className="text-xs text-faint">{t("common.loading")}</p>
        ) : detail.isSuccess && legacy ? (
          <p className="text-xs text-faint">{t("logs.detail.noPayload")}</p>
        ) : detail.isSuccess ? (
          <div className="space-y-4">
            <PayloadBlock
              title={t("logs.detail.arguments")}
              raw={full.requestSummary}
              empty={t("logs.detail.noValue")}
              truncatedLabel={t("logs.detail.truncated")}
              maxHeight="max-h-52"
            />
            <PayloadBlock
              title={t("logs.detail.result")}
              raw={full.responseSummary}
              empty={t("logs.detail.noValue")}
              truncatedLabel={t("logs.detail.truncated")}
              maxHeight="max-h-96"
            />
          </div>
        ) : null}
      </div>
    </Modal>
  );
}

function RequestLogTable() {
  const { t } = useTranslation();
  const [filter, setFilter] = useState("");
  const [status, setStatus] = useState<StatusFilter>("all");
  const [selected, setSelected] = useState<LogEntry | null>(null);
  const logs = useQuery({
    queryKey: queryKeys.hubLogs,
    queryFn: () => ipc.logsRecent(500),
    ...livePoll(5000),
  });
  const servers = useQuery({
    queryKey: queryKeys.servers,
    queryFn: ipc.serversList,
    retry: false,
  });
  const workspaces = useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ipc.workspacesList,
    retry: false,
  });
  const hub = useQuery({
    queryKey: queryKeys.hubStatus,
    queryFn: ipc.hubStatus,
    retry: false,
  });

  const serverById = useMemo(() => {
    const map = new Map<string, ServerMeta>();
    for (const server of servers.data ?? []) {
      map.set(server.id, { name: server.name, slug: server.slug });
    }
    return map;
  }, [servers.data]);

  const rows = useMemo(() => {
    const q = filter.trim().toLowerCase();
    return (logs.data ?? []).filter((entry) => {
      if (status === "ok" && !entry.success) return false;
      if (status === "fail" && entry.success) return false;
      if (!q) return true;
      const mapped = entry.serverId ? (serverById.get(entry.serverId)?.name ?? "") : "";
      const hay = [
        entry.exposedName,
        entry.method,
        entry.clientConnectionId,
        entry.errorCategory,
        entry.requestId,
        entry.serverId,
        mapped,
        entry.errorMessage,
      ]
        .filter(Boolean)
        .join(" ")
        .toLowerCase();
      return hay.includes(q);
    });
  }, [logs.data, filter, status, serverById]);

  // 本屏 p90 当「慢」的门槛：绝对阈值在不同 Server 之间没有可比性（本地文件系统
  // 200ms 已经异常，联网搜索 2s 很正常）。样本太少或整体都快时不标，那时没有尾部可言。
  const slowThreshold = useMemo(() => {
    if (rows.length < 5) return null;
    const sorted = rows.map((r) => r.durationMs).sort((a, b) => a - b);
    const p90 = sorted[Math.floor(sorted.length * 0.9)] ?? 0;
    return p90 >= 1000 ? p90 : null;
  }, [rows]);

  // `?? []` 每次都是新数组，不能直接作 memo 依赖；数组只留给弹窗那种要遍历的地方。
  const hubConnections = hub.data?.connections;
  const connections = hubConnections ?? [];

  // 连接信息按 id 缓存成稳定引用。此前每行每次渲染都现造一个 `{client, token}`，
  // 浅比较必然失败——下面那句 memo 的注释（"只重渲染变化的行"）从来没成立过，
  // 5s 轮询一到就是 500 行全量重渲染。
  const connectionById = useMemo(() => {
    const list = hubConnections ?? [];
    const map = new Map<string, ConnectionParts>();
    for (const entry of rows) {
      const id = entry.clientConnectionId;
      // 先去重再解析：500 行通常只落在几个连接上，`connectionParts` 里的
      // find 因此只跑那几次，逻辑也不必在这里抄第二份。
      if (!id || map.has(id)) continue;
      const parts = connectionParts(id, list);
      if (parts) map.set(id, parts);
    }
    return map;
  }, [rows, hubConnections]);

  const grouped = useMemo(() => groupByDay(rows), [rows]);
  const failedCount = useMemo(() => rows.filter((r) => !r.success).length, [rows]);

  const filtering = filter.trim() !== "" || status !== "all";

  const statusOptions: { value: StatusFilter; label: string }[] = [
    { value: "all", label: t("logs.filterStatus.all") },
    { value: "ok", label: t("logs.filterStatus.ok") },
    { value: "fail", label: t("logs.filterStatus.fail") },
  ];

  return (
    <div>
      <StatsSection serverById={serverById} connections={connections} />
      <div className="mb-3 flex flex-wrap items-center gap-3">
        <SearchInput
          className="w-80"
          placeholder={t("logs.filter")}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <SegmentedControl
          label={t("logs.filterStatus")}
          value={status}
          options={statusOptions}
          onChange={setStatus}
        />
        {/* 「最近 500 条」这个口径写在表旁边，比塞进统计窗口的括号里更找得到。
            失败数跟在后面：要不要切到「失败」页签，看这一眼就够。 */}
        <p className="ml-auto text-[11px] text-faint tabular-nums">
          {filtering
            ? t("logs.countMatch", { count: rows.length, total: logs.data?.length ?? 0 })
            : t("logs.countRecent", { count: rows.length })}
          {failedCount > 0 && status !== "fail" ? (
            <span className="text-danger before:px-1.5 before:text-faint before:content-['·']">
              {t("logs.countFailedSuffix", { count: failedCount })}
            </span>
          ) : null}
        </p>
      </div>

      {/* logs_recent 读的是本地 SQLite（与 Hub 是否在跑无关），isError 是这里
          真出了错（如本地库读取失败），不是"还没数据"——两者文案不能共用，
          否则用户看到的是一句无关痛痒的空态提示，猜不到该重试还是等数据。 */}
      {logs.isError ? <p className="mb-3 text-sm text-danger">{t("logs.loadError")}</p> : null}

      <Card data-tour="logs-table" className="overflow-hidden">
        <div className="overflow-x-auto">
          <table className="w-full table-fixed text-left text-xs">
            <thead>
              {/* 表头不铺底色：bg-app 留给日期分段行专用，两者同色时第一个分段
                  紧贴表头，读起来像多出来的第二行表头。 */}
              <tr className="border-b border-line text-faint">
                {/* pl-4 = 行首 2px 状态条 + pl-3.5：表头文字与时刻列左缘对齐。
                    7rem 是按英文的 12 小时制留的：`05:20:27 PM` 比中文的
                    `17:20:27` 长三个字符，5.5rem 会让它顶进工具列。 */}
                <th className="w-[7rem] py-2 pr-3 pl-4 font-medium">{t("logs.col.time")}</th>
                <th className="w-[44%] px-3 py-2 font-medium">{t("logs.col.tool")}</th>
                <th className="w-[20%] px-3 py-2 font-medium">{t("logs.col.connection")}</th>
                <th className="w-[5.5rem] px-3 py-2 text-right font-medium">
                  {t("logs.col.elapsed")}
                </th>
                <th className="w-[6.5rem] px-3 py-2 font-medium">{t("logs.col.result")}</th>
                <th className="w-8" />
              </tr>
            </thead>
            <tbody>
              {grouped.map((row) =>
                row.kind === "day" ? (
                  <DayHeaderRow key={`day-${row.key}`} row={row} />
                ) : (
                  <LogRow
                    key={row.entry.id || `${row.entry.requestId}-${row.entry.ts}`}
                    entry={row.entry}
                    server={
                      row.entry.serverId ? (serverById.get(row.entry.serverId) ?? null) : null
                    }
                    connection={
                      row.entry.clientConnectionId
                        ? (connectionById.get(row.entry.clientConnectionId) ?? null)
                        : null
                    }
                    slow={slowThreshold != null && row.entry.durationMs >= slowThreshold}
                    tabStop={row.first}
                    selected={selected?.id === row.entry.id}
                    onOpen={setSelected}
                  />
                ),
              )}
              {rows.length === 0 && logs.isSuccess ? (
                <tr>
                  <td colSpan={6} className="px-4 py-12 text-center text-muted">
                    {/* 筛选清空 vs 真没数据是两回事："暂无匹配日志"暗示"换个筛选词
                        试试"，用在真空表上会误导用户以为自己打错了字。 */}
                    {filtering ? t("logs.noMatch") : t("logs.empty")}
                  </td>
                </tr>
              ) : null}
            </tbody>
          </table>
        </div>
      </Card>

      {selected ? (
        <LogDetailModal
          entry={selected}
          servers={servers.data ?? []}
          connections={connections}
          workspaces={workspaces.data ?? []}
          onClose={() => setSelected(null)}
        />
      ) : null}
    </div>
  );
}

/** Hub 请求日志 + 配置操作历史（TASK-506：两条观测流合并到一页两标签）。 */
export function Logs() {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("requests");
  const tabs: { value: Tab; label: string }[] = [
    { value: "requests", label: t("logs.tab.requests") },
    { value: "history", label: t("logs.tab.history") },
  ];

  return (
    <div>
      <PageHeader
        title={t("logs.title")}
        actions={
          <SegmentedControl
            data-tour="logs-tabs"
            // 无障碍名称复用导览里同一控件的标题，不另造一份说同一件事的文案。
            label={t("tour.logs.tabs.title")}
            value={tab}
            options={tabs}
            onChange={setTab}
          />
        }
      />

      {tab === "requests" ? <RequestLogTable /> : <HistoryPanel />}
    </div>
  );
}

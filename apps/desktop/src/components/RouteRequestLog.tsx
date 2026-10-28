import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ChevronDown, ChevronRight, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { formatTokens } from "../lib/format";
import { type ClientApp, ipc, type RouteRequestRecord } from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { ConfirmDialog } from "./ConfirmDialog";
import { IconButton, SegmentedControl, StatusBadge } from "./ui";

type LogFilter = "all" | "failed";

/** 失败 = 无状态码（含进行中）、状态码 ≥400，或带 error 文本。 */
function isFailedRow(row: RouteRequestRecord): boolean {
  return row.status === null || row.status >= 400 || row.error !== null;
}

/**
 * 本地路由「最近请求」：每条请求由谁服务、经过了哪些转移或整流、耗时与用量。
 * 默认收起，展开后才轮询，免得不看的时候白拉数据。
 */
export function RouteRequestLog({ app }: { app: ClientApp }) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState<LogFilter>("all");
  const [confirmClear, setConfirmClear] = useState(false);
  const [clearError, setClearError] = useState<string | null>(null);
  const requests = useQuery({
    queryKey: queryKeys.routerRequests(app),
    queryFn: () => ipc.routerRecentRequests(app, 20),
    enabled: open,
    ...livePoll(5000),
  });
  const rows = (requests.data ?? []).filter((row) => filter === "all" || isFailedRow(row));

  const clear = async () => {
    setConfirmClear(false);
    setClearError(null);
    try {
      await ipc.routerClearRequests(app);
      void queryClient.invalidateQueries({ queryKey: queryKeys.routerRequests(app) });
    } catch (e) {
      setClearError(errorText(e));
    }
  };

  return (
    <div className="border-t border-line px-3 py-2">
      <div className="flex items-center justify-between gap-2">
        <button
          type="button"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
          className="flex items-center gap-1 text-xs text-faint hover:text-ink"
        >
          {open ? <ChevronDown size={13} aria-hidden /> : <ChevronRight size={13} aria-hidden />}
          {t("routing.log.title")}
        </button>
        {open ? (
          <div className="flex items-center gap-2">
            <SegmentedControl
              label={t("routing.log.filter")}
              value={filter}
              options={[
                { value: "all", label: t("routing.log.filterAll") },
                { value: "failed", label: t("routing.log.filterFailed") },
              ]}
              onChange={setFilter}
            />
            <IconButton
              icon={Trash2}
              tone="danger"
              title={t("routing.log.clear")}
              aria-label={t("routing.log.clear")}
              disabled={(requests.data?.length ?? 0) === 0}
              onClick={() => setConfirmClear(true)}
            />
          </div>
        ) : null}
      </div>
      {open ? (
        rows.length === 0 ? (
          <p className="mt-1 px-1 text-xs text-muted">
            {(requests.data?.length ?? 0) === 0
              ? t("routing.log.empty")
              : t("routing.log.emptyFiltered")}
          </p>
        ) : (
          <ul className="mt-1 space-y-1">
            {rows.map((row) => (
              <RequestRow key={row.id} row={row} />
            ))}
          </ul>
        )
      ) : null}
      {clearError ? <p className="mt-1 px-1 text-xs text-danger">{clearError}</p> : null}
      {confirmClear ? (
        <ConfirmDialog
          title={t("routing.log.clearTitle")}
          message={t("routing.log.clearConfirm")}
          confirmText={t("routing.log.clear")}
          onConfirm={() => void clear()}
          onCancel={() => setConfirmClear(false)}
        />
      ) : null}
    </div>
  );
}

function RequestRow({ row }: { row: RouteRequestRecord }) {
  const { t, i18n } = useTranslation();
  const time = new Date(row.at).toLocaleTimeString(
    i18n.language.startsWith("en") ? "en-US" : "zh-CN",
    {
      hour12: false,
    },
  );
  const ok = row.status !== null && row.status < 400;
  const pending = row.status === null;
  const latency =
    row.durationMs !== null
      ? t("routing.log.latency", {
          first: ((row.firstByteMs ?? 0) / 1000).toFixed(1),
          total: (row.durationMs / 1000).toFixed(1),
        })
      : null;
  const path = row.attempts.map((a) => `${a.providerName}（${a.outcome}）`).join(" → ");

  return (
    <li className="rounded-md px-1 py-1 text-xs hover:bg-hover">
      <div className="flex min-w-0 items-center gap-2">
        <span className="shrink-0 tabular-nums text-faint">{time}</span>
        {pending ? (
          <StatusBadge tone="pending">{t("routing.log.pending")}</StatusBadge>
        ) : (
          <StatusBadge tone={ok ? "run" : "danger"}>{row.status}</StatusBadge>
        )}
        <span className="truncate text-ink">{row.providerName ?? "—"}</span>
        {row.model ? <span className="truncate text-muted">{row.model}</span> : null}
        <span className="ml-auto flex shrink-0 items-center gap-2 tabular-nums text-faint">
          {row.usage ? (
            <span>
              {t("routing.log.tokens", {
                input: formatTokens(row.usage.inputTokens),
                output: formatTokens(row.usage.outputTokens),
              })}
            </span>
          ) : null}
          {latency ? <span>{latency}</span> : null}
        </span>
      </div>
      {path ? (
        <p className="mt-0.5 truncate pl-[4.5rem] text-faint" title={path}>
          {t("routing.log.path", { path })}
        </p>
      ) : null}
      {row.error ? (
        <p className="mt-0.5 truncate pl-[4.5rem] text-danger" title={row.error}>
          {row.error}
        </p>
      ) : null}
    </li>
  );
}

/**
 * cc-switch 迁移面板：检测 → 勾选要搬什么 → 导入 → 就地报结果。
 *
 * 两处共用同一块：首启向导的第二步，和设置页「数据」分区那张常驻卡。它们唯一的
 * 差别是外面那层壳，面板本身自洽（自己检测、自己导入、自己展示结果）。
 */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Download, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { CLIENT_META } from "../lib/clients";
import { type CcSwitchImportSummary, ipc } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { invalidateServerQueries } from "../lib/serverQueries";
import { cn, errorText } from "../lib/utils";
import { Button, Card, Switch } from "./ui";

export function CcSwitchImportPanel({ onImported }: { onImported?: () => void } = {}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [withProviders, setWithProviders] = useState(true);
  const [withServers, setWithServers] = useState(true);
  const [result, setResult] = useState<CcSwitchImportSummary | null>(null);

  const detection = useQuery({
    queryKey: queryKeys.ccswitchDetect,
    queryFn: ipc.ccswitchDetect,
    // 探测要把整个 cc-switch 库复制出来再解析（用户的库可能有几十 MB）。
    // 它在一次会话内不会变，别让每次切到设置页的数据分区都重跑一遍。
    staleTime: Number.POSITIVE_INFINITY,
    retry: false,
  });

  const runImport = useMutation({
    mutationFn: () => ipc.ccswitchImport(withProviders, withServers),
    onSuccess: (summary) => {
      setResult(summary);
      onImported?.();
      // 两类都可能进来了，各自失效对应的族——不用整库失效：那会连带把本次
      // 探测结果一起重取（又是一次整库复制），还会白拉一遍用量等无关数据。
      for (const key of [
        queryKeys.providers,
        queryKeys.providerCurrent,
        queryKeys.providersUnified,
      ]) {
        void queryClient.invalidateQueries({ queryKey: key });
      }
      invalidateServerQueries(queryClient);
    },
  });

  if (detection.isLoading) {
    return <p className="text-[13px] text-faint">{t("wizard.migrate.detecting")}</p>;
  }
  if (detection.isError) {
    return (
      <p className="text-[13px] text-danger">
        {t("wizard.migrate.detectFailed", { error: errorText(detection.error) })}
      </p>
    );
  }

  const found = detection.data?.found ?? null;
  if (!found) {
    return (
      <Card className="p-4">
        <p className="text-[13px] font-medium text-ink">{t("wizard.migrate.none")}</p>
        {/* 查过哪儿要说出来：用户才分得清是没装，还是装在了别的目录。 */}
        <p className="mt-1 font-mono text-[11px] break-all text-faint">
          {detection.data?.searched}
        </p>
      </Card>
    );
  }

  // 搬完之后整张卡换成结果：再摆着"找到 N 个…可以搬过来"和两个灰开关，
  // 与下面那句"搬好了"直接打架——用户读完两句还是不知道到底搬没搬。
  if (result) {
    return (
      <Card className="p-4">
        <ImportResult summary={result} />
      </Card>
    );
  }
  return (
    <Card className="p-4">
      <p className="text-[13px] font-medium text-ink">
        {t("wizard.migrate.found", {
          providers: found.providerCount,
          servers: found.serverCount,
        })}
      </p>
      {found.apps.length > 0 ? (
        <p className="mt-1 text-[12px] text-muted">
          {/* 「Claude Code 3」会被读成版本号，中间那个冒号不能省。 */}
          {found.apps
            .map(({ app, count }) =>
              t("wizard.migrate.appCount", {
                app: CLIENT_META[app]?.label ?? app,
                count,
              }),
            )
            .join(" · ")}
        </p>
      ) : null}

      <div className="mt-3 space-y-0.5 border-t border-line pt-3">
        <ToggleRow
          label={t("wizard.migrate.providers", { count: found.providerCount })}
          checked={withProviders}
          disabled={found.providerCount === 0}
          onChange={setWithProviders}
        />
        <ToggleRow
          label={t("wizard.migrate.servers", { count: found.serverCount })}
          checked={withServers}
          disabled={found.serverCount === 0}
          onChange={setWithServers}
        />
      </div>

      {runImport.isError ? (
        <p className="mt-3 text-[13px] text-danger">
          {t("wizard.migrate.importFailed", { error: errorText(runImport.error) })}
        </p>
      ) : null}

      <div className="mt-3 flex justify-end">
        <Button
          variant="primary"
          size="sm"
          icon={Download}
          loading={runImport.isPending}
          disabled={!withProviders && !withServers}
          onClick={() => runImport.mutate()}
        >
          {t("wizard.migrate.import")}
        </Button>
      </div>
    </Card>
  );
}

function ToggleRow({
  label,
  checked,
  disabled,
  onChange,
}: {
  label: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <div className={cn("flex items-center gap-3 py-1.5", disabled && "opacity-50")}>
      <span className="flex-1 text-[13px] text-ink">{label}</span>
      <Switch
        label={label}
        checked={checked && !disabled}
        disabled={disabled}
        onChange={onChange}
      />
    </div>
  );
}

/** 导入结果就地展示：导了几条、跳过几条、哪几条搬不动。不弹 toast——它会飘走。 */
function ImportResult({ summary }: { summary: CcSwitchImportSummary }) {
  const { t } = useTranslation();
  const notes = [...summary.warnings, ...summary.providers.failed];
  const skipped = summary.providers.skipped.length + summary.servers.skipped.length;
  return (
    <div className="mt-3 rounded-lg border border-line bg-hover p-3">
      <p className="flex items-center gap-1.5 text-[13px] font-medium text-ink">
        <Check size={14} strokeWidth={2.5} aria-hidden className="text-on" />
        {t("wizard.migrate.result", {
          providers: summary.providers.imported.length,
          servers: summary.servers.imported.length,
        })}
      </p>
      {summary.failoverMigrated > 0 ? (
        <p className="mt-1 text-[12px] text-muted">
          {t("wizard.migrate.resultFailover", { count: summary.failoverMigrated })}
        </p>
      ) : null}
      {skipped > 0 ? (
        <p className="mt-1 text-[12px] text-muted">
          {t("wizard.migrate.resultSkipped", { count: skipped })}
        </p>
      ) : null}
      {notes.length > 0 ? (
        <ul className="mt-2 space-y-1">
          {notes.map((note) => (
            <li key={note} className="flex items-start gap-1.5 text-[12px] text-muted">
              <TriangleAlert
                size={13}
                strokeWidth={2}
                aria-hidden
                className="mt-0.5 shrink-0 text-warn"
              />
              <span>{note}</span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

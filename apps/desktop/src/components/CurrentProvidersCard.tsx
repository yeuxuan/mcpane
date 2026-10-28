import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { CLIENT_META } from "../lib/clients";
import { readDashboardApps, resolveDashboardApps } from "../lib/dashboardApps";
import { type ClientApp, ipc, type ProviderProfile } from "../lib/ipc";
import { invalidateProviderQueries } from "../lib/providerQueries";
import { queryKeys } from "../lib/queryKeys";
import { cn, errorText } from "../lib/utils";
import { ClientGlyph } from "./BrandIcon";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./shadcn/select";
import { Card } from "./ui";

const DAY_HOURS = 24;

function fmtUsd(n: number): string {
  return n >= 100 ? `$${n.toFixed(0)}` : `$${n.toFixed(2)}`;
}

/**
 * 概览页首卡：至多两个客户端（设置里选）各一格——当前由哪个供应商供电、近 24 小时花费，
 * 并能就地切换。
 */
export function CurrentProvidersCard() {
  const installed = useQuery({
    queryKey: queryKeys.clientsInstalled,
    queryFn: ipc.providerClientsInstalled,
    retry: false,
  });
  const providers = useQuery({ queryKey: queryKeys.providers, queryFn: ipc.providersList });
  const current = useQuery({ queryKey: queryKeys.providerCurrent, queryFn: ipc.providerCurrent });
  const costs = useQuery({
    queryKey: queryKeys.usageByApp(DAY_HOURS),
    queryFn: () => ipc.usageByProvider(DAY_HOURS),
    retry: false,
  });
  // 偏好只在挂载时读：设置页与概览页不会同屏，回到概览必然重新挂载。
  const [preferred] = useState(readDashboardApps);

  if (!installed.data || !providers.data) return null;
  const apps = resolveDashboardApps(preferred, installed.data);
  if (apps.length === 0) return null;

  return (
    // 多格合成一张纸：容器底色当发丝线，格子之间只留 1px 缝。
    <Card className="mb-4 overflow-hidden">
      <div className={cn("grid gap-px bg-line", apps.length > 1 && "md:grid-cols-2")}>
        {apps.map((app) => {
          const cost = costs.data?.find((row) => row.key === app);
          return (
            <ProviderTile
              key={app}
              app={app}
              providers={providers.data
                .filter((p) => p.app === app)
                .sort((a, b) => a.sortOrder - b.sortOrder)}
              currentId={current.data?.[app] ?? null}
              cost={cost?.priced ? cost.costUsd : null}
            />
          );
        })}
      </div>
    </Card>
  );
}

function ProviderTile({
  app,
  providers,
  currentId,
  cost,
}: {
  app: ClientApp;
  providers: ProviderProfile[];
  currentId: string | null;
  cost: number | null;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [notice, setNotice] = useState<string[]>([]);
  const label = CLIENT_META[app].label;

  const switchProvider = useMutation({
    mutationFn: (id: string) => ipc.providerSwitch(app, id),
    onSuccess: (outcome) => setNotice(outcome.warnings),
    onError: (error) => setNotice([t("providers.switchError", { error: errorText(error) })]),
    // 失败也要收敛到后端真值：切换可能写了一半配置。
    onSettled: () => invalidateProviderQueries(queryClient, app),
  });

  return (
    <section aria-label={label} className="flex min-w-0 flex-col bg-card px-5 py-4">
      <header className="flex items-center gap-2.5">
        <ClientGlyph app={app} size={20} />
        <h3 className="min-w-0 flex-1 truncate font-semibold text-[14px] text-ink">{label}</h3>
        <Select
          value={currentId ?? undefined}
          disabled={providers.length === 0}
          onValueChange={(id) => {
            setNotice([]);
            switchProvider.mutate(id);
          }}
        >
          <SelectTrigger
            size="sm"
            loading={switchProvider.isPending}
            // 填充式：白纸上的控件用凹槽底，不再多画一圈边框。
            className="max-w-[60%] border-transparent bg-slot hover:bg-hover"
            aria-label={t("dashboard.current.switchLabel", { app: label })}
          >
            <SelectValue placeholder={t("dashboard.current.none")} />
          </SelectTrigger>
          <SelectContent align="end">
            {providers.map((p) => (
              <SelectItem key={p.id} value={p.id}>
                {p.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </header>

      {notice.map((line) => (
        <p key={line} className="mt-2 text-xs text-warn">
          {line}
        </p>
      ))}

      <p
        className="mt-3 text-[11px] text-faint tabular-nums"
        title={t("dashboard.current.costTitle")}
      >
        {cost === null
          ? t("dashboard.current.costNone")
          : t("dashboard.current.cost", { cost: fmtUsd(cost) })}
      </p>
    </section>
  );
}

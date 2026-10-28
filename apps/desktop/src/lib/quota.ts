import { useQuery } from "@tanstack/react-query";
import { type Balance, ipc, type ProviderProfile } from "./ipc";
import { queryKeys } from "./queryKeys";

/**
 * 额度查询的两种来源（与 Rust `ProviderService::quota` 的分派同规则）。官方订阅一律不查：
 * - plan：Coding Plan 厂商（Kimi / 智谱 / MiniMax），用已存 Key；
 * - balance：按量计费厂商的账户余额，用已存 Key。
 */
export type QuotaKind = "plan" | "balance";

const PLAN_HOSTS = [
  "api.kimi.com/coding",
  "bigmodel.cn",
  "api.z.ai",
  "api.minimaxi.com",
  "api.minimax.io",
];
const BALANCE_HOSTS = [
  "deepseek.com",
  "stepfun.com",
  "stepfun.ai",
  "siliconflow.cn",
  "siliconflow.com",
  "openrouter.ai",
  "novita.ai",
];

export function quotaKind(profile: Pick<ProviderProfile, "id" | "baseUrl">): QuotaKind | null {
  const url = profile.baseUrl?.toLowerCase();
  if (!url) return null;
  if (PLAN_HOSTS.some((host) => url.includes(host))) return "plan";
  if (BALANCE_HOSTS.some((host) => url.includes(host))) return "balance";
  return null;
}

export const QUOTA_REFRESH_MS = 5 * 60_000;

/**
 * 单个供应商的额度查询，供应商卡 / 用量页共用一份缓存。
 * `auto` 为真时按 5 分钟自动刷新；`refresh` 永远可手动触发。
 */
export function useProviderQuota(
  profile: Pick<ProviderProfile, "id" | "baseUrl"> | undefined,
  { auto }: { auto: boolean },
) {
  const kind = profile ? quotaKind(profile) : null;
  const id = profile?.id ?? "";
  const query = useQuery({
    queryKey: queryKeys.providerQuota(id),
    queryFn: () => ipc.providerQuota(id),
    enabled: auto && kind !== null,
    refetchInterval: QUOTA_REFRESH_MS,
    staleTime: 60_000,
    retry: false,
  });
  return { kind, query, refresh: () => void query.refetch() };
}

const CURRENCY_SYMBOL: Record<string, string> = { CNY: "¥", USD: "$" };

export function formatBalance({ amount, currency }: Balance): string {
  const symbol = CURRENCY_SYMBOL[currency.toUpperCase()];
  const value = amount.toFixed(2);
  return symbol ? `${symbol}${value}` : `${value} ${currency}`;
}

/** 窗口剩余百分比（0-100 取整）：各处一律报「还剩多少」，不报「用了多少」。 */
export function headroomOf(utilization: number): number {
  return Math.round(Math.min(Math.max(100 - utilization, 0), 100));
}

/** 余量见底才报警（≤30 橙 / ≤10 红），平时不着色。各处额度展示同一套阈值。 */
export function headroomTone(remaining: number): "danger" | "warn" | "normal" {
  return remaining <= 10 ? "danger" : remaining <= 30 ? "warn" : "normal";
}

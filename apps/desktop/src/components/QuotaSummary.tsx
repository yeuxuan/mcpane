import type { UseQueryResult } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import { formatAt } from "../lib/datetime";
import type { ProviderQuota } from "../lib/ipc";
import { formatBalance, headroomOf, headroomTone } from "../lib/quota";
import { cn, errorText } from "../lib/utils";

/** 额度一行读数：各窗口剩余百分比 + 余额 + 套餐名，供应商卡用。 */
export function QuotaSummary({
  query,
  className,
}: {
  query: UseQueryResult<ProviderQuota>;
  className?: string;
}) {
  const { t, i18n } = useTranslation();
  if (query.isFetching && !query.data) {
    return <p className={cn("text-xs text-muted", className)}>{t("providers.quotaLoading")}</p>;
  }
  if (query.isError) {
    return (
      <p className={cn("text-xs text-warn", className)}>
        {t("providers.quotaFail", { error: errorText(query.error) })}
      </p>
    );
  }
  const quota = query.data;
  if (!quota) return null;
  if (!quota.success) {
    return (
      <p className={cn("text-xs text-warn", className)}>
        {t("providers.quotaFail", { error: quota.error ?? "unknown" })}
      </p>
    );
  }
  return (
    <p className={cn("flex flex-wrap items-center gap-x-3 gap-y-0.5 text-xs", className)}>
      {quota.planLabel ? <span className="text-muted">{quota.planLabel}</span> : null}
      {quota.tiers.map((tier) => {
        const remaining = headroomOf(tier.utilization);
        const tone = headroomTone(remaining);
        return (
          <span
            key={tier.window}
            title={
              tier.resetsAt
                ? t("providers.quotaResets", {
                    // resetsAt 是 Unix 毫秒，formatAt 收秒。
                    time: formatAt(i18n.language, "time", tier.resetsAt / 1000),
                  })
                : undefined
            }
            className={cn(
              "tabular-nums",
              tone === "danger" ? "text-danger" : tone === "warn" ? "text-warn" : "text-ink",
            )}
          >
            {t(`providers.quotaWindow.${tier.window}`)}{" "}
            {t("providers.quotaLeft", { percent: remaining })}
          </span>
        );
      })}
      {quota.balances.map((balance) => (
        <span key={balance.currency} className="text-ink tabular-nums">
          {t("providers.quotaBalance", { amount: formatBalance(balance) })}
        </span>
      ))}
    </p>
  );
}

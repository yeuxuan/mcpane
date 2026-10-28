import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FileClock, Undo2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { formatAt } from "../lib/datetime";
import { ipc, type OperationRecord } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { ConfirmDialog } from "./ConfirmDialog";
import { Button, Card, EmptyState, IconChip, Tag } from "./ui";

const STATUS: Record<
  OperationRecord["status"],
  { labelKey: string; tone: "green" | "slate" | "amber" }
> = {
  applied: { labelKey: "history.statusApplied", tone: "green" },
  "rolled-back": { labelKey: "history.statusRolledBack", tone: "slate" },
  "failed-recovered": { labelKey: "history.statusFailedRecovered", tone: "amber" },
};

/** 记录是否触及 mcpane Hub 条目（凭据随接入/轮换写入，回滚不会复活旧 Token）。 */
function touchesHubEntry(record: OperationRecord): boolean {
  return record.changedPaths.some((path) => path.includes("mcpane"));
}

/** 配置操作历史 + 一键回滚（TASK-506 部分）——作为「日志」页的一个标签。 */
export function HistoryPanel() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  /* 回滚会覆盖磁盘上的配置文件，且历史列表里每行长得都一样——误点的代价与
   * 命中率都不低，和删除备份同级，走同一个确认框。 */
  const [confirmRollback, setConfirmRollback] = useState<OperationRecord | null>(null);
  const history = useQuery({
    queryKey: queryKeys.configHistory,
    queryFn: ipc.configHistoryList,
    retry: false,
  });
  const rollback = useMutation({
    mutationFn: (record: OperationRecord) => ipc.configRollback(record.id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.configHistory });
      queryClient.invalidateQueries({ queryKey: queryKeys.clients });
      queryClient.invalidateQueries({ queryKey: queryKeys.mcpClients });
      queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus });
      queryClient.invalidateQueries({ queryKey: queryKeys.providers });
      queryClient.invalidateQueries({ queryKey: queryKeys.providerCurrent });
    },
  });

  return (
    <div>
      {rollback.isError ? (
        <p className="mb-3 text-sm text-danger">
          {t("history.rollbackFailed", { error: errorText(rollback.error) })}
        </p>
      ) : null}
      {/* 读取失败时 data 是 undefined，空态条件（=== 0）也不成立，整个标签页
          此前一片空白：既没有列表也没有任何说明。查询是 retry:false，不会自愈。 */}
      {history.isError ? (
        <p className="mb-3 text-sm text-danger">
          {t("history.loadFailed", { error: errorText(history.error) })}
        </p>
      ) : null}
      {history.isSuccess && history.data.length === 0 ? (
        <EmptyState title={t("history.emptyTitle")} description={t("history.empty")} />
      ) : null}

      <div className="space-y-2.5">
        {(history.data ?? []).map((record) => (
          <Card key={record.id} interactive className="flex items-center gap-3 p-4">
            <IconChip icon={FileClock} tone="slate" />
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2">
                <span className="text-sm text-ink">{record.description}</span>
                <Tag tone={STATUS[record.status].tone}>{t(STATUS[record.status].labelKey)}</Tag>
              </div>
              <p className="mt-1 truncate font-mono text-xs text-faint">
                {/* 整行走单 key，不拼片段：中英的分隔与语序不同 */}
                {record.changedPaths.length > 0
                  ? t("history.metaWithChanges", {
                      time: formatAt(i18n.language, "time", record.createdUnixSecs),
                      path: record.targetPath,
                      changed: record.changedPaths.join(", "),
                    })
                  : t("history.meta", {
                      time: formatAt(i18n.language, "time", record.createdUnixSecs),
                      path: record.targetPath,
                    })}
              </p>
              {touchesHubEntry(record) ? (
                <p className="mt-1 text-xs text-faint">{t("history.hubTokenNote")}</p>
              ) : null}
            </div>
            {record.snapshot ? (
              <Button
                size="sm"
                variant="ghost"
                icon={Undo2}
                className="shrink-0"
                onClick={() => setConfirmRollback(record)}
              >
                {t("history.rollback")}
              </Button>
            ) : null}
          </Card>
        ))}
      </div>

      {confirmRollback ? (
        <ConfirmDialog
          title={t("history.rollbackTitle")}
          message={t("history.rollbackConfirm", {
            description: confirmRollback.description,
            time: formatAt(i18n.language, "time", confirmRollback.createdUnixSecs),
          })}
          confirmText={t("history.rollback")}
          onConfirm={() => {
            rollback.mutate(confirmRollback);
            setConfirmRollback(null);
          }}
          onCancel={() => setConfirmRollback(null)}
        />
      ) : null}
    </div>
  );
}

import { useMutation } from "@tanstack/react-query";
import { AlertTriangle, RefreshCw } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { formatAt } from "../lib/datetime";
import { type InitErrorInfo, ipc } from "../lib/ipc";
import { errorText } from "../lib/utils";
import { Button, Card, CopyButton } from "./ui";

interface InitRecoveryProps {
  error: InitErrorInfo;
  /** 重试探针（locked 场景用户关掉占用进程后可直接恢复）。 */
  onRetry: () => void;
}

/**
 * 数据库初始化失败的全屏恢复页：解释原因 + 一键从备份换库 + 重试。
 * db 坏时所有页面都没意义，因此不进 Shell，由 App 顶层直接渲染本页。
 */
export function InitRecovery({ error, onRetry }: InitRecoveryProps) {
  const { t, i18n } = useTranslation();
  const [replaced, setReplaced] = useState(false);
  const replaceDb = useMutation({
    mutationFn: ipc.initReplaceDbFromBackup,
    onSuccess: () => setReplaced(true),
  });

  if (replaced) {
    return (
      <div className="flex min-h-screen flex-col items-center justify-center gap-4 bg-app p-8">
        <p className="font-mono text-lg text-ink">❯ {t("initRecovery.replacedTitle")}</p>
        <p className="max-w-md text-center text-sm text-muted">{t("initRecovery.replacedDesc")}</p>
        <Button variant="primary" onClick={() => ipc.updateRestart()}>
          {t("initRecovery.restartNow")}
        </Button>
      </div>
    );
  }

  return (
    <div className="flex min-h-screen flex-col items-center justify-center gap-5 bg-app p-8">
      <div className="flex items-center gap-2 text-ink">
        <AlertTriangle className="h-5 w-5 text-amber-500" aria-hidden />
        <p className="font-mono text-lg">❯ {t("initRecovery.title")}</p>
      </div>
      <p className="max-w-lg text-center text-sm text-muted">
        {t(`initRecovery.kind.${error.kind}`)}
      </p>
      <Card className="w-full max-w-2xl p-4">
        <div className="flex items-start justify-between gap-2">
          <pre className="max-h-40 flex-1 overflow-auto whitespace-pre-wrap break-all text-xs text-muted">
            {error.message}
            {"\n"}
            {error.dbPath}
          </pre>
          <CopyButton value={`${error.message}\n${error.dbPath}`} />
        </div>
      </Card>

      {error.backups.length > 0 ? (
        <Card className="w-full max-w-2xl p-4">
          <p className="mb-2 text-sm font-medium text-ink">{t("initRecovery.backupsTitle")}</p>
          <p className="mb-3 text-xs text-muted">{t("initRecovery.backupsDesc")}</p>
          <ul className="flex flex-col gap-2">
            {error.backups.map((backup) => (
              <li key={backup.fileName} className="flex items-center gap-3">
                <span
                  className="flex-1 truncate font-mono text-xs text-ink"
                  title={backup.fileName}
                >
                  {backup.fileName}
                </span>
                <span className="shrink-0 text-xs text-faint">
                  {formatAt(i18n.language, "time", backup.createdAt)} ·{" "}
                  {Math.max(1, Math.round(backup.sizeBytes / 1024))} KB
                </span>
                <Button
                  // 只让被点的那行转圈；恢复期间其余行禁用防并发换库。
                  loading={replaceDb.isPending && replaceDb.variables === backup.fileName}
                  disabled={replaceDb.isPending}
                  onClick={() => replaceDb.mutate(backup.fileName)}
                >
                  {t("initRecovery.restoreFrom")}
                </Button>
              </li>
            ))}
          </ul>
        </Card>
      ) : (
        <p className="text-xs text-faint">{t("initRecovery.noBackups")}</p>
      )}

      {replaceDb.isError ? (
        <p className="max-w-lg text-center text-xs text-rose-500">
          {t("initRecovery.replaceFailed", { error: errorText(replaceDb.error) })}
        </p>
      ) : null}

      <Button onClick={onRetry}>
        <RefreshCw className="mr-1.5 h-3.5 w-3.5" aria-hidden />
        {t("initRecovery.retry")}
      </Button>
    </div>
  );
}

import { useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Import, Router } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc } from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { ConfirmDialog } from "./ConfirmDialog";
import { Button, Field } from "./ui";

/**
 * Claude Desktop 标签页顶部面板（对齐 cc-switch 的状态检查 + 路由开关）：
 * - 一致性检查横幅：平台不支持 / profile 有非法模型名 / proxy 缺路由 /
 *   token 未生成 / profile 地址被外部改动，逐条提示修复动作；
 * - 本地路由开关：显示网关运行态与端口，可手动启停（启用前确认弹窗，
 *   停止时后端校验当前供应商是否仍依赖网关）。
 */
export function ClaudeDesktopPanel() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [confirmStart, setConfirmStart] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [importResult, setImportResult] = useState<string | null>(null);
  /** 端口编辑草稿；null = 未在编辑。 */
  const [portDraft, setPortDraft] = useState<string | null>(null);

  const status = useQuery({
    queryKey: queryKeys.claudeDesktopStatus,
    queryFn: ipc.claudeDesktopStatus,
    // 仅本 tab 挂载时轮询（对齐 cc-switch 5s 间隔）。
    ...livePoll(5000),
  });
  const proxy = useQuery({
    queryKey: queryKeys.desktopProxyStatus,
    queryFn: ipc.desktopProxyStatus,
    ...livePoll(5000),
  });

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.claudeDesktopStatus });
    void queryClient.invalidateQueries({ queryKey: queryKeys.desktopProxyStatus });
  };

  const runAction = async (action: () => Promise<unknown>) => {
    setActionError(null);
    try {
      await action();
    } catch (error) {
      setActionError(errorText(error));
    } finally {
      refresh();
    }
  };

  const s = status.data;
  const issues: string[] = [];
  if (s) {
    if (!s.supported) issues.push(t("claudeDesktop.statusUnsupported"));
    if (s.staleRawModels) issues.push(t("claudeDesktop.statusStaleRawModels"));
    if (s.missingRouteMappings) issues.push(t("claudeDesktop.statusMissingRouteMappings"));
    if (s.mode === "proxy" && !s.gatewayTokenConfigured)
      issues.push(t("claudeDesktop.statusGatewayTokenMissing"));
    if (s.expectedBaseUrl && s.actualBaseUrl && s.expectedBaseUrl !== s.actualBaseUrl) {
      issues.push(
        t("claudeDesktop.statusBaseUrlMismatch", {
          actual: s.actualBaseUrl,
          expected: s.expectedBaseUrl,
        }),
      );
    }
  }

  const importFromClaude = async () => {
    setActionError(null);
    try {
      const count = await ipc.claudeDesktopImportFromClaude();
      setImportResult(t("claudeDesktop.importResult", { count }));
      void queryClient.invalidateQueries({ queryKey: queryKeys.providers });
    } catch (error) {
      setActionError(errorText(error));
    }
  };

  const savePort = async () => {
    const port = Number(portDraft);
    if (!Number.isInteger(port) || port < 1024 || port > 65535) {
      setActionError(t("claudeDesktop.portInvalid"));
      return;
    }
    await runAction(() => ipc.desktopProxySetPort(port));
    setPortDraft(null);
  };

  const running = proxy.data?.running ?? false;
  const portLabel = running
    ? `127.0.0.1:${proxy.data?.port}`
    : `127.0.0.1:${proxy.data?.listenPort ?? ""}`;

  return (
    <div className="mb-4 space-y-2">
      {issues.length > 0 ? (
        <div className="rounded-lg border border-warn-line bg-warn-soft p-2.5">
          <p className="mb-1 flex items-center gap-1.5 text-xs font-medium text-warn">
            <AlertTriangle size={13} aria-hidden />
            {t("claudeDesktop.statusTitle")}
          </p>
          <ul className="list-inside list-disc space-y-0.5 text-xs text-warn">
            {issues.map((issue) => (
              <li key={issue}>{issue}</li>
            ))}
          </ul>
        </div>
      ) : null}

      <div className="flex items-center justify-between rounded-lg border border-line bg-card px-3 py-2">
        <div className="flex items-center gap-2">
          <Router size={15} aria-hidden className={running ? "text-run" : "text-faint"} />
          <div>
            <p className="text-xs font-medium text-ink">
              {t(running ? "claudeDesktop.routeActive" : "claudeDesktop.routeInactive")}
            </p>
            {portDraft === null ? (
              <button
                type="button"
                // 运行中也可改：新端口起来后后端会把所有指向路由的客户端配置改写过去。
                title={t("claudeDesktop.portEdit")}
                onClick={() => setPortDraft(String(proxy.data?.listenPort ?? ""))}
                className="font-mono text-[11px] text-faint hover:text-ink hover:underline"
              >
                {portLabel}
              </button>
            ) : (
              <span className="mt-0.5 flex items-center gap-1.5">
                <Field
                  className="h-6 w-20 px-1.5 font-mono text-[11px]"
                  value={portDraft}
                  onChange={(e) => setPortDraft(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void savePort();
                    if (e.key === "Escape") setPortDraft(null);
                  }}
                />
                <Button size="sm" onClick={() => void savePort()}>
                  {t("claudeDesktop.portSave")}
                </Button>
              </span>
            )}
          </div>
        </div>
        <div className="flex items-center gap-1.5">
          <Button size="sm" icon={Import} onClick={() => void importFromClaude()}>
            {t("claudeDesktop.importFromClaude")}
          </Button>
          <Button
            size="sm"
            disabled={status.isLoading || proxy.isLoading}
            onClick={() => {
              if (running) {
                void runAction(() => ipc.desktopProxyStop());
              } else {
                setConfirmStart(true);
              }
            }}
          >
            {t(running ? "claudeDesktop.routeStop" : "claudeDesktop.routeStart")}
          </Button>
        </div>
      </div>
      {importResult ? <p className="text-xs text-muted">{importResult}</p> : null}
      {actionError ? <p className="text-xs text-danger">{actionError}</p> : null}

      {confirmStart ? (
        <ConfirmDialog
          title={t("claudeDesktop.routeConfirmTitle")}
          message={t("claudeDesktop.routeConfirmMessage")}
          confirmText={t("claudeDesktop.routeConfirmOk")}
          onConfirm={() => {
            setConfirmStart(false);
            void runAction(() => ipc.desktopProxyStart());
          }}
          onCancel={() => setConfirmStart(false)}
        />
      ) : null}
    </div>
  );
}

import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CLIENT_META } from "../lib/clients";
import { type ImportLinkOutcome, type ImportLinkPreview, ipc } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { Button, Field, Modal } from "./ui";

/**
 * `mcpane://import/provider` 深链的确认框（ADR-023）。链接是不可信输入：后端只暂存草稿，
 * 这里展示目标主机与 Key 掩码，用户点「导入」才落库；导入不切换、不开路由。
 * 冷启动时事件可能早于前端挂载，所以挂载时也主动取一次草稿。
 */
export function ImportLinkDialog() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [preview, setPreview] = useState<ImportLinkPreview | null>(null);
  const [invalid, setInvalid] = useState<string | null>(null);
  const [result, setResult] = useState<ImportLinkOutcome | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const next = await ipc.importLinkPeek();
      if (!next) return;
      setPreview(next);
      setResult(null);
      setInvalid(null);
      setApiKey("");
      setError(null);
    } catch {
      // 取不到预览就当没有待导入项，不打断用户。
    }
  }, []);

  useEffect(() => {
    void load();
    const offRequest = listen("import-link-request", () => void load()).catch(() => () => {});
    const offError = listen<string>("import-link-error", (event) => {
      setPreview(null);
      setResult(null);
      setInvalid(event.payload);
    }).catch(() => () => {});
    return () => {
      void offRequest.then((fn) => fn());
      void offError.then((fn) => fn());
    };
  }, [load]);

  const close = () => {
    if (preview && !result) void ipc.importLinkDiscard();
    setPreview(null);
    setResult(null);
    setInvalid(null);
  };

  const confirm = async () => {
    setBusy(true);
    setError(null);
    try {
      const outcome = await ipc.importLinkConfirm(apiKey.trim() || null);
      setResult(outcome);
      if (outcome.imported.length > 0) {
        void queryClient.invalidateQueries({ queryKey: queryKeys.providers });
        void queryClient.invalidateQueries({ queryKey: queryKeys.providersUnified });
      }
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  if (invalid) {
    return (
      <Modal
        title={t("importLink.invalidTitle")}
        onClose={close}
        footer={
          <Button size="sm" variant="primary" onClick={close}>
            {t("importLink.done")}
          </Button>
        }
      >
        <p className="text-sm text-muted">{t("importLink.invalid", { reason: invalid })}</p>
      </Modal>
    );
  }
  if (!preview) return null;

  if (result) {
    const message = result.imported[0]
      ? t("importLink.imported", { name: result.imported[0] })
      : result.skipped[0]
        ? t("importLink.skipped", { name: result.skipped[0] })
        : t("importLink.failed", { reason: result.failed[0] ?? "" });
    return (
      <Modal
        title={t("importLink.title")}
        onClose={close}
        footer={
          <Button size="sm" variant="primary" onClick={close}>
            {t("importLink.done")}
          </Button>
        }
      >
        <p className="text-sm text-ink">{message}</p>
      </Modal>
    );
  }

  return (
    <Modal
      title={t("importLink.title")}
      onClose={close}
      footer={
        <>
          <Button size="sm" onClick={close} disabled={busy}>
            {t("confirm.cancel")}
          </Button>
          <Button size="sm" variant="primary" loading={busy} onClick={() => void confirm()}>
            {t("importLink.confirm")}
          </Button>
        </>
      }
    >
      <div className="space-y-3">
        <p className="text-sm text-muted">{t("importLink.intro")}</p>
        <dl className="grid grid-cols-[72px_1fr] gap-x-3 gap-y-2 text-sm">
          <dt className="text-faint">{t("importLink.client")}</dt>
          <dd className="text-ink">{CLIENT_META[preview.app].label}</dd>
          <dt className="text-faint">{t("importLink.name")}</dt>
          <dd className="break-words text-ink">{preview.name}</dd>
          <dt className="text-faint">{t("importLink.address")}</dt>
          <dd className="min-w-0">
            <p className="font-medium text-ink">{preview.host}</p>
            <p className="break-all text-xs text-muted">{preview.baseUrl}</p>
          </dd>
          {preview.model ? (
            <>
              <dt className="text-faint">{t("importLink.model")}</dt>
              <dd className="break-all text-ink">{preview.model}</dd>
            </>
          ) : null}
          <dt className="text-faint">{t("importLink.key")}</dt>
          <dd className="min-w-0">
            {preview.keyHint ? (
              <span className="text-ink">
                {t("importLink.keyProvided", { hint: preview.keyHint })}
              </span>
            ) : (
              <>
                <Field
                  type="password"
                  autoComplete="off"
                  spellCheck={false}
                  className="w-full"
                  aria-label={t("importLink.key")}
                  placeholder={t("importLink.keyPlaceholder")}
                  value={apiKey}
                  onChange={(e) => setApiKey(e.target.value)}
                />
                <p className="mt-1 text-xs text-muted">{t("importLink.keyMissing")}</p>
              </>
            )}
          </dd>
        </dl>
        <p className="rounded-md bg-warn-soft px-2 py-1.5 text-xs text-warn">
          {t("importLink.warning")}
        </p>
        {error ? <p className="text-xs text-danger">{error}</p> : null}
      </div>
    </Modal>
  );
}

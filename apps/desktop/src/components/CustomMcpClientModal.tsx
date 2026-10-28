/**
 * 添加自定义 MCP 客户端。
 *
 * 只问名字：MCPane 不读也不写这个客户端的配置文件，接入方式是发一个 Token、
 * 把「使用说明」里的配置片段贴过去。加完之后它就和内置客户端一样有凭证管理与
 * 访问控制——那两处本来就按客户端标识工作。
 */
import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc } from "../lib/ipc";
import { errorText } from "../lib/utils";
import { Input } from "./shadcn/input";
import { Button, ErrorBanner, FormField, Modal } from "./ui";

/** 名称 → 标识：小写、非字母数字转连字符，与后端的字符集校验对齐。 */
function slugify(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9-_]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32);
}

export function CustomMcpClientModal({
  onClose,
  onSaved,
}: {
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation();
  const [display, setDisplay] = useState("");
  // 标识默认从名称派生，用户改过之后就不再跟着名称走——否则他每敲一个字
  // 都会把手填的标识冲掉。
  const [id, setId] = useState("");
  const [idTouched, setIdTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const effectiveId = idTouched ? id : slugify(display);

  const save = useMutation({
    mutationFn: () => ipc.customMcpClientUpsert(effectiveId, display.trim()),
    onSuccess: onSaved,
    onError: (e) => setError(errorText(e)),
  });

  return (
    <Modal
      title={t("clients.customAddTitle")}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>{t("ui.cancel")}</Button>
          <Button
            variant="primary"
            disabled={save.isPending || !display.trim() || !effectiveId}
            onClick={() => {
              setError(null);
              save.mutate();
            }}
          >
            {t("ui.save")}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <ErrorBanner message={error} />
        <p className="text-xs leading-relaxed text-muted">{t("clients.customAddHint")}</p>

        <FormField label={t("clients.customName")} hint={t("clients.customNameHint")}>
          <Input
            autoFocus
            value={display}
            onChange={(e) => setDisplay(e.target.value)}
            placeholder="Trae"
          />
        </FormField>

        <FormField label={t("clients.customId")} hint={t("clients.customIdHint")}>
          <Input
            className="font-mono"
            value={effectiveId}
            onChange={(e) => {
              setIdTouched(true);
              setId(e.target.value.replace(/[^a-zA-Z0-9_-]/g, ""));
            }}
            placeholder="trae"
          />
        </FormField>
      </div>
    </Modal>
  );
}

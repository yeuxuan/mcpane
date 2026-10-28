/**
 * 导出 MCP 服务器：勾选 → 复制 JSON 或保存为文件（ADR-027）。凭据由后端换成占位符，
 * 这里逐条列出替换了哪几处：分享前看得见带出去的是什么、收件人要补填什么。
 */
import { keepPreviousData, useMutation, useQuery } from "@tanstack/react-query";
import { Check, Copy, FileDown, ShieldCheck } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc, type ServerDef } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { ServerBrief } from "./ServerBrief";
import { Checkbox } from "./shadcn/checkbox";
import { Label } from "./shadcn/label";
import { Button, Card, CollapseToggle, InlineNote, JsonView, Modal } from "./ui";

export function ServerExportModal({
  servers,
  initialIds,
  onClose,
}: {
  servers: ServerDef[];
  /** 打开时预先勾选的服务器（页头「导出」= 全部，行尾菜单 = 该行）。 */
  initialIds: string[];
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [selected, setSelected] = useState<Set<string>>(() => new Set(initialIds));
  // 按列表顺序取 id：缓存键稳定，导出顺序也与服务器页一致。
  const ids = servers.filter((server) => selected.has(server.id)).map((server) => server.id);
  const exportQuery = useQuery({
    queryKey: queryKeys.serverExport(ids.join(",")),
    queryFn: () => ipc.serversExport(ids),
    enabled: ids.length > 0,
    retry: false,
    // 勾选变化时先留着上一份，明细与预览不至于闪空。
    placeholderData: keepPreviousData,
    // 服务器编辑后内容会变：关窗即丢，下次打开不先拿旧内容给人复制。
    gcTime: 0,
  });
  const content = ids.length ? exportQuery.data : undefined;
  // 复制的必须是当前勾选对应的那份，换勾选后旧内容还在占位时不放行。
  const ready = ids.length > 0 && exportQuery.isSuccess && !exportQuery.isPlaceholderData;

  const [previewOpen, setPreviewOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState<string | null>(null);
  const copiedTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(copiedTimer.current), []);
  const copy = async () => {
    if (!content) return;
    setCopyError(null);
    try {
      await navigator.clipboard.writeText(content.json);
      setCopied(true);
      clearTimeout(copiedTimer.current);
      copiedTimer.current = setTimeout(() => setCopied(false), 1500);
    } catch (error) {
      // 复制就是这个弹窗的主路径，失败要说出来（还可以改用保存为文件）。
      setCopyError(t("serverExport.copyFailed", { error: errorText(error) }));
    }
  };
  const save = useMutation({
    mutationFn: () => ipc.serversExportSave(ids),
    onMutate: () => setCopyError(null),
  });

  const allChecked = ids.length === servers.length;
  const toggle = (id: string, checked: boolean) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
    save.reset();
  };

  const status = copyError ? (
    <InlineNote tone="error">{copyError}</InlineNote>
  ) : save.isError ? (
    <InlineNote tone="error">
      {t("serverExport.saveFailed", { error: errorText(save.error) })}
    </InlineNote>
  ) : save.isSuccess ? (
    <span title={save.data} className="min-w-0">
      <InlineNote tone="ok">{t("serverExport.saved", { path: save.data })}</InlineNote>
    </span>
  ) : null;

  return (
    <Modal
      title={t("serverExport.title")}
      size="lg"
      onClose={onClose}
      footer={
        <>
          <div className="mr-auto flex min-w-0">{status}</div>
          <Button
            icon={FileDown}
            disabled={!ready}
            loading={save.isPending}
            onClick={() => save.mutate()}
          >
            {t("serverExport.save")}
          </Button>
          <Button variant="primary" icon={copied ? Check : Copy} disabled={!ready} onClick={copy}>
            {copied ? t("ui.copied") : t("serverExport.copy")}
          </Button>
        </>
      }
    >
      <div className="space-y-4">
        <div className="space-y-2">
          <div className="flex items-center justify-between px-3">
            <Label className="cursor-pointer gap-3 font-normal text-[13px] text-ink">
              <Checkbox
                checked={allChecked ? true : ids.length === 0 ? false : "indeterminate"}
                onCheckedChange={() => {
                  setSelected(allChecked ? new Set() : new Set(servers.map((s) => s.id)));
                  save.reset();
                }}
              />
              {t("serverExport.selectAll")}
            </Label>
            <span className="text-xs text-faint tabular-nums">
              {t("serverExport.selectedCount", { selected: ids.length, total: servers.length })}
            </span>
          </div>
          <Card
            role="group"
            aria-label={t("serverExport.listLabel")}
            // 随窗口高度收放：最小窗口下也要让下面的凭据替换明细露出来。
            className="max-h-[28vh] divide-y divide-line overflow-y-auto"
          >
            {servers.map((server) => (
              <div key={server.id} className="flex items-center gap-3 px-3 py-2.5">
                <Checkbox
                  checked={selected.has(server.id)}
                  onCheckedChange={(value) => toggle(server.id, value === true)}
                  aria-label={t("serverExport.select", { name: server.name })}
                />
                <ServerBrief server={server} />
              </div>
            ))}
          </Card>
        </div>

        {ids.length === 0 ? (
          <p className="text-xs text-muted">{t("serverExport.empty")}</p>
        ) : exportQuery.isError ? (
          <p className="text-xs text-danger">
            {t("serverExport.loadFailed", { error: errorText(exportQuery.error) })}
          </p>
        ) : content ? (
          <>
            {content.redactions.length ? (
              <div className="rounded-lg border border-line bg-app p-3">
                <p className="flex items-center gap-1.5 font-medium text-[13px] text-ink">
                  <ShieldCheck size={14} strokeWidth={2} aria-hidden className="text-muted" />
                  {t("serverExport.redactedTitle", { count: content.redactions.length })}
                </p>
                {/* 表格而非逐行 flex：位置与占位符两列要上下对齐，扫一眼就能核对。 */}
                <table className="mt-2 text-xs">
                  <thead className="sr-only">
                    <tr>
                      <th scope="col">{t("serverExport.col.server")}</th>
                      <th scope="col">{t("serverExport.col.field")}</th>
                      <th scope="col">{t("serverExport.col.placeholder")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {content.redactions.map((item) => (
                      <tr key={`${item.server}\u0000${item.variable}`}>
                        <td className="max-w-48 truncate py-0.5 pr-6 text-left text-ink">
                          {item.server}
                        </td>
                        <td className="whitespace-nowrap py-0.5 pr-6 text-muted">
                          {t(`serverExport.field.${item.field}`)}
                        </td>
                        <td className="py-0.5 font-mono text-ink">{`\${${item.variable}}`}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
                <p className="mt-2 text-xs text-muted">{t("serverExport.redactedNote")}</p>
              </div>
            ) : (
              <p className="text-xs text-muted">{t("serverExport.noRedactionNote")}</p>
            )}
            <div className="space-y-2">
              <CollapseToggle open={previewOpen} onToggle={() => setPreviewOpen((open) => !open)}>
                {t("serverExport.preview")}
              </CollapseToggle>
              {previewOpen ? <JsonView code={content.json} maxHeightClassName="max-h-64" /> : null}
            </div>
          </>
        ) : (
          <p className="text-xs text-muted">{t("common.loading")}</p>
        )}
      </div>
    </Modal>
  );
}

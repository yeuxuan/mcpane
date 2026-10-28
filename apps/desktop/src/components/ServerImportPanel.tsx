/**
 * 导入 MCP 服务器：粘贴 JSON 或选文件 → 后端解析出逐条预览 → 勾选、补填占位符后导入（ADR-027）。
 */
import { keepPreviousData, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Download, FolderOpen } from "lucide-react";
import { type ChangeEvent, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc, type ServerImportPreviewEntry, type ServerImportSummary } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { invalidateServerQueries } from "../lib/serverQueries";
import { errorText } from "../lib/utils";
import { JsonEditor } from "./JsonEditor";
import { ServerBrief } from "./ServerBrief";
import { Checkbox } from "./shadcn/checkbox";
import { Button, Card, Field, StatusBadge, Tag } from "./ui";

type Translate = (key: string, options?: Record<string, unknown>) => string;

// 示例用 memory：它是官方仍在维护的 reference server 之一。
// 别用 puppeteer —— 那个包已被官方归档，拿它当范例等于教用户装没人维护的东西
//（收录与排除的完整名单见 lib/serverPresets.ts）。
const JSON_PLACEHOLDER = `{
  "mcpServers": {
    "memory": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-memory"],
      "env": { "MEMORY_FILE_PATH": "/tmp/memory.json" }
    }
  }
}`;

/** 服务器配置不会到 1 MB；超过多半是选错了文件，别把它整个塞进编辑器。 */
const MAX_FILE_BYTES = 1024 * 1024;

/** 导入结果的一句话摘要（服务器页内联提示位；从客户端导入与 JSON 导入共用）。 */
export function importSummaryText(summary: ServerImportSummary, t: Translate): string {
  // 整句而非拼接：中文的「，跳过…」与英文的 ", skipped …" 断句方式不同。
  const base = summary.skipped.length
    ? t("servers.importSummarySkipped", {
        imported: summary.imported.length,
        skipped: summary.skipped.length,
      })
    : t("servers.importSummary", { imported: summary.imported.length });
  const incomplete = summary.incomplete.length
    ? [t("servers.importIncomplete", { count: summary.incomplete.length })]
    : [];
  // 后端 warnings 是完整句子（某份配置读不了/解析失败），原样展示。
  return [base, ...incomplete, ...summary.warnings].join(" · ");
}

/** 原文的短指纹作缓存键：文件可达 1 MB，整段当 queryKey 每次渲染都要序列化哈希。 */
function textFingerprint(text: string): string {
  let hash = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    hash ^= text.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return `${text.length}:${(hash >>> 0).toString(36)}`;
}

/** 条目里还没填的占位符（空白视同未填，与后端口径一致）。 */
function missingValues(
  entry: ServerImportPreviewEntry,
  values: Record<string, Record<string, string>>,
): string[] {
  return entry.variables.filter((name) => !values[entry.server.name]?.[name]?.trim());
}

/**
 * 导入面板的状态与动作。拆成 hook 是因为按钮放在弹窗底栏（Modal footer），
 * 与面板主体不在同一棵子树里。
 */
export function useServerImport(onImported: (message: string) => void) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [text, setText] = useState("");
  // 停笔 300ms 再解析：每敲一个字都打一次 IPC，半截 JSON 的报错会一路闪。
  const [debounced, setDebounced] = useState("");
  useEffect(() => {
    const timer = setTimeout(() => setDebounced(text), 300);
    return () => clearTimeout(timer);
  }, [text]);
  const source = debounced.trim();
  const sourceKey = useMemo(() => textFingerprint(source), [source]);

  const preview = useQuery({
    queryKey: queryKeys.serverImportPreview(sourceKey),
    queryFn: () => ipc.serversImportPreview(source),
    enabled: source !== "",
    retry: false,
    // 改字时先留着上一份预览，免得列表在「解析中」和结果之间来回跳。
    placeholderData: keepPreviousData,
    // 冲突判定取决于库里当时有哪些服务器：关窗即丢，下次打开不拿旧结果充数。
    gcTime: 0,
  });
  const data = source ? preview.data : undefined;

  // 记「取消勾选」而非「勾选」：换一份 JSON 时新条目默认全选，不必同步重置。
  const [unchecked, setUnchecked] = useState<Set<string>>(new Set());
  const [values, setValues] = useState<Record<string, Record<string, string>>>({});
  const entries = data?.entries ?? [];
  // 批内互撞（memory / Memory 归一到同一前缀）随勾选判定：勾着的那条占住前缀，
  // 取消它后后面那条即可选——与后端只对勾选条目去重同口径。
  const batchOwner = new Map<string, string>();
  const claimed = new Map<string, string>();
  for (const entry of entries) {
    if (entry.conflict) continue;
    const owner = claimed.get(entry.server.slug);
    if (owner) batchOwner.set(entry.server.name, owner);
    else if (!unchecked.has(entry.server.name)) claimed.set(entry.server.slug, entry.server.name);
  }
  const conflictOf = (entry: ServerImportPreviewEntry) =>
    entry.conflict ?? batchOwner.get(entry.server.name) ?? null;
  const isChecked = (entry: ServerImportPreviewEntry) =>
    !conflictOf(entry) && !unchecked.has(entry.server.name);
  const selected = entries.filter(isChecked);
  // 屏幕上的预览必须就是将要导入的那份：防抖未落定、仍在解析时都不放行。
  const ready =
    source !== "" && text.trim() === source && preview.isSuccess && !preview.isPlaceholderData;

  const [result, setResult] = useState<string | null>(null);
  const runImport = useMutation({
    mutationFn: () => {
      const names = selected.map((entry) => entry.server.name);
      const filled: Record<string, Record<string, string>> = {};
      for (const name of names) {
        const entryValues = values[name];
        if (entryValues) filled[name] = entryValues;
      }
      return ipc.serversImportJson(source, names, filled);
    },
    onMutate: () => setResult(null),
    onSuccess: (summary) => {
      invalidateServerQueries(queryClient);
      const message = importSummaryText(summary, t);
      // 有条目入库就交给页面展示并关窗；全被跳过时留在原地说明原因，
      // 静默关窗而列表毫无变化，像是误操作。
      if (summary.imported.length > 0) onImported(message);
      else setResult(message);
    },
  });

  const [fileError, setFileError] = useState<string | null>(null);
  const pickFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    // 清空后同一个文件再选一次也会触发 change。
    event.target.value = "";
    if (!file) return;
    setFileError(null);
    if (file.size > MAX_FILE_BYTES) {
      setFileError(t("serverImport.fileTooLarge", { name: file.name }));
      return;
    }
    try {
      const content = await file.text();
      setText(content);
      // 选文件是一次性输入，不必等防抖。
      setDebounced(content);
    } catch (error) {
      setFileError(t("serverImport.fileReadFailed", { name: file.name, error: errorText(error) }));
    }
  };

  return {
    text,
    setText,
    source,
    preview,
    data,
    entries,
    conflictOf,
    isChecked,
    toggle: (name: string, checked: boolean) =>
      setUnchecked((prev) => {
        const next = new Set(prev);
        if (checked) next.delete(name);
        else next.add(name);
        return next;
      }),
    values,
    setValue: (server: string, variable: string, value: string) =>
      setValues((prev) => ({ ...prev, [server]: { ...prev[server], [variable]: value } })),
    selectedCount: selected.length,
    ready,
    runImport,
    result,
    fileError,
    pickFile,
  };
}

export type ServerImporter = ReturnType<typeof useServerImport>;

/** 面板主体：编辑器 + 选择文件 + 逐条预览（含占位符补填）。 */
export function ServerImportPanel({ importer }: { importer: ServerImporter }) {
  const { t } = useTranslation();
  const fileInput = useRef<HTMLInputElement>(null);
  const { data, preview } = importer;

  return (
    <div className="space-y-3">
      <div className="flex items-start justify-between gap-3">
        <p className="text-[13px] text-muted">{t("serverModal.jsonHint")}</p>
        <Button size="sm" icon={FolderOpen} onClick={() => fileInput.current?.click()}>
          {t("serverImport.pickFile")}
        </Button>
        <input
          ref={fileInput}
          type="file"
          accept=".json,application/json"
          className="hidden"
          tabIndex={-1}
          aria-hidden
          onChange={importer.pickFile}
        />
      </div>
      <JsonEditor
        value={importer.text}
        onChange={importer.setText}
        placeholder={JSON_PLACEHOLDER}
        minHeight={180}
        // 默认上限 480 会把预览挤出小窗口的可视区：选完文件只看到原文和一个灰按钮。
        maxHeight={220}
      />
      {importer.fileError ? <p className="text-xs text-danger">{importer.fileError}</p> : null}

      {importer.source === "" ? null : preview.isError ? (
        <p className="text-xs text-danger">{errorText(preview.error)}</p>
      ) : !data ? (
        <p className="text-xs text-muted">{t("serverImport.parsing")}</p>
      ) : data.entries.length === 0 && data.invalid.length === 0 ? (
        <p className="text-xs text-muted">{t("serverImport.noEntries")}</p>
      ) : (
        <div className="space-y-2">
          {data.entries.length ? (
            <Card
              role="group"
              className="divide-y divide-line"
              aria-label={t("serverImport.listLabel")}
            >
              {data.entries.map((entry) => (
                <PreviewRow key={entry.server.name} entry={entry} importer={importer} />
              ))}
            </Card>
          ) : null}
          {data.invalid.length ? (
            <p className="text-xs text-warn">
              {t("serverImport.invalid", { names: data.invalid.join(", ") })}
            </p>
          ) : null}
        </div>
      )}

      {importer.result ? <p className="text-xs text-muted">{importer.result}</p> : null}
      {importer.runImport.isError ? (
        <p className="text-xs text-danger">{errorText(importer.runImport.error)}</p>
      ) : null}
    </div>
  );
}

function PreviewRow({
  entry,
  importer,
}: {
  entry: ServerImportPreviewEntry;
  importer: ServerImporter;
}) {
  const { t } = useTranslation();
  const name = entry.server.name;
  const checked = importer.isChecked(entry);
  const conflict = importer.conflictOf(entry);
  const missing = missingValues(entry, importer.values);
  // 只给例外打标：能正常导入的条目由勾选本身说明，不再挂「新增」。
  const badges = (
    <>
      {conflict ? (
        <StatusBadge tone="stop">
          {conflict === name
            ? t("serverImport.exists")
            : t("serverImport.prefixTaken", { name: conflict })}
        </StatusBadge>
      ) : missing.length ? (
        <StatusBadge tone="warn">
          {t("serverImport.needsValues", { count: missing.length })}
        </StatusBadge>
      ) : null}
      {entry.server.enabled ? null : <Tag>{t("serverImport.disabledInFile")}</Tag>}
    </>
  );
  return (
    <div className="px-3 py-2.5">
      <div className="flex items-center gap-3">
        <Checkbox
          checked={checked}
          disabled={!!conflict}
          onCheckedChange={(value) => importer.toggle(name, value === true)}
          aria-label={t("serverImport.select", { name })}
        />
        <ServerBrief server={entry.server} badges={badges} />
      </div>
      {checked && entry.variables.length ? (
        // 缩进对齐名称列：勾选框 16 + 间距 12 + 图标 26 + 间距 12。
        <div className="mt-2 space-y-1.5 pl-[66px]">
          {entry.variables.map((variable) => (
            <div key={variable} className="flex items-center gap-2">
              <span
                className="w-44 shrink-0 truncate font-mono text-xs text-muted"
                title={variable}
              >
                {variable}
              </span>
              <Field
                type="password"
                className="h-8 min-w-0 flex-1 font-mono text-xs"
                value={importer.values[name]?.[variable] ?? ""}
                onChange={(event) => importer.setValue(name, variable, event.target.value)}
                placeholder={t("serverImport.valuePlaceholder")}
                aria-label={t("serverImport.valueLabel", { name, variable })}
              />
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** 弹窗底栏的导入按钮。 */
export function ServerImportAction({ importer }: { importer: ServerImporter }) {
  const { t } = useTranslation();
  const count = importer.selectedCount;
  return (
    <Button
      variant="primary"
      icon={Download}
      className="w-full"
      disabled={!importer.ready || count === 0}
      loading={importer.runImport.isPending}
      onClick={() => importer.runImport.mutate()}
    >
      {importer.ready && count > 0
        ? t("serverImport.action", { count })
        : t("serverImport.actionIdle")}
    </Button>
  );
}

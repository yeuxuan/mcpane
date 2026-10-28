import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { LucideIcon } from "lucide-react";
import {
  Check,
  Copy,
  FileJson,
  Globe,
  PlugZap,
  Plus,
  SlidersHorizontal,
  Sparkles,
  Trash2,
  Wrench,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  type CatalogTool,
  type DeclaredAnnotations,
  ipc,
  type ServerDef,
  type ServerTestResult,
  type ServerTransport,
  type ToolOverride,
} from "../lib/ipc";
import { livePoll } from "../lib/liveQuery";
import { queryKeys } from "../lib/queryKeys";
import { invalidateServerQueries } from "../lib/serverQueries";
import { useUnsavedGuard } from "../lib/useUnsavedGuard";
import { cn, errorText } from "../lib/utils";
import { DEFAULT_WS, useWorkspace } from "../lib/workspace";
import { ConfirmDialog } from "./ConfirmDialog";
import { JsonEditor } from "./JsonEditor";
import { ServerDiscoverPanel } from "./ServerDiscoverPanel";
import { ServerImportAction, ServerImportPanel, useServerImport } from "./ServerImportPanel";
import { type PresetFill, ServerPresetPicker } from "./ServerPresetPicker";
import { ToolAnnotationsModal } from "./ToolAnnotationsModal";
import {
  Button,
  CollapseToggle,
  Field,
  IconButton,
  LabeledField,
  Modal,
  StatusBadge,
  Switch,
  Tag,
} from "./ui";

// IPC 载荷类型的正主在 lib/ipc.ts，这里 re-export 以维持既有引用路径。
export type { ServerDef, ServerTransport } from "../lib/ipc";

/**
 * 参数输入框的文本 → 参数数组。用双引号把含空格的整体括起来。
 *
 * 朴素的 `split(" ")` 会把 `/Users/me/Library/Application Support` 切成两个
 * 参数——macOS 上这种路径遍地都是，而 filesystem / git 这类 Server 收的正是
 * 路径。切错了 Server 起不来，且错因完全看不出来。
 *
 * **反斜杠是字面量，不是转义符。** 按 POSIX shell 那套让 `\` 转义，
 * `C:\Users\me` 会被吃成 `C:Usersme`——Windows 用户往这个框里贴的正是这种路径，
 * 那比不做引号处理还糟。字面引号改用 CSV 那套：引号内的 `""` 表示一个 `"`。
 * 这样两者互不冲突，`"C:\dir\"` 这类「引号包着以反斜杠结尾的路径」也能原样还原。
 */
export function splitArgs(text: string): string[] {
  const pieces: string[] = [];
  let current = "";
  let quoted = false;
  let started = false; // 区分「空字符串参数 ""」与「没有参数」

  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (char === '"') {
      if (quoted && text[i + 1] === '"') {
        current += '"'; // 引号内的 "" → 一个字面引号
        i += 1;
      } else {
        quoted = !quoted;
      }
      started = true;
    } else if (char !== undefined && /\s/.test(char) && !quoted) {
      if (started) pieces.push(current);
      current = "";
      started = false;
    } else {
      current += char;
      started = true;
    }
  }
  if (started) pieces.push(current);
  return pieces;
}

/**
 * 参数数组 → 输入框文本；含空格或引号的自动加引号。
 *
 * 对非空参数与 [`splitArgs`] 互逆。空串参数是个例外——`joinArgs([""])` 得到
 * `""`，而 `splitArgs("")` 是 `[]`（空输入框本就该解析成「没有参数」，不能为了
 * 往返把它变成「一个空参数」）。空参数在 MCP Server 里没有实际用途，不为它
 * 牺牲空输入框的语义。
 */
export function joinArgs(args: string[]): string {
  return args.map((arg) => (/[\s"]/.test(arg) ? `"${arg.replaceAll('"', '""')}"` : arg)).join(" ");
}

/** 与后端 mcpane_hub_slug 同规则：小写字母数字，其余折叠为连字符。 */
export function mcpaneHubSlug(name: string): string {
  const slug = name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return slug || "server";
}

export interface KeyValueRow {
  rowId: number;
  key: string;
  value: string;
}

let rowSeq = 0;
const makeRow = (key = "", value = ""): KeyValueRow => ({ rowId: rowSeq++, key, value });

/** 键值行 → Record（跳过空 key，key 去首尾空白）。 */
export function rowsToRecord(rows: KeyValueRow[]): Record<string, string> {
  return Object.fromEntries(
    rows.filter((row) => row.key.trim()).map((row) => [row.key.trim(), row.value]),
  );
}

export function recordToRows(record: Record<string, string>): KeyValueRow[] {
  return Object.entries(record).map(([key, value]) => makeRow(key, value));
}

export interface ToolRow {
  originalName: string;
  exposedName: string;
  description: string | null;
  enabled: boolean;
  /** 下游申报的注解；null = 没申报（禁用行也一律为 null——它不在 Catalog 里）。 */
  annotations: Record<string, unknown> | null;
  /** 用户声明的注解（全局，非按工作区）。 */
  declaredAnnotations: DeclaredAnnotations | null;
}

/**
 * 合并某 Server 的工具视图：Catalog 里的都是启用中的；
 * overrides 里 enabled=false 的补成禁用行（exposedName 按默认规则推导）。
 */
export function mergeServerTools(
  catalogTools: CatalogTool[],
  overrides: ToolOverride[],
  serverId: string,
  slug: string,
): ToolRow[] {
  const active: ToolRow[] = catalogTools
    .filter((tool) => tool.serverId === serverId)
    .map((tool) => ({
      originalName: tool.originalName,
      exposedName: tool.exposedName,
      description: tool.description,
      enabled: true,
      annotations: tool.annotations ?? null,
      declaredAnnotations: tool.declaredAnnotations ?? null,
    }));
  const activeNames = new Set(active.map((row) => row.originalName));
  const disabled: ToolRow[] = overrides
    .filter((item) => item.serverId === serverId && !item.enabled)
    .filter((item) => !activeNames.has(item.originalName))
    .map((item) => ({
      originalName: item.originalName,
      exposedName: `${slug}__${item.originalName}`,
      description: null,
      enabled: false,
      // 禁用行不在 Catalog 里，拿不到下游申报值——注解入口对它也没意义
      // （工具都不暴露了，注解发给谁）。
      annotations: null,
      declaredAnnotations: null,
    }));
  return [...active, ...disabled].sort((a, b) => a.originalName.localeCompare(b.originalName));
}

export interface SnippetForm {
  name: string;
  transport: ServerTransport;
  command: string;
  args: string;
  env: Record<string, string>;
  cwd: string;
  url: string;
  bearer: string;
  bearerEnv: string;
  headers: Record<string, string>;
}

/** 表单 → mcpServers JSON 片段（编辑弹窗 JSON 模式 / 一键复制共用）。 */
export function buildServerSnippet(form: SnippetForm): string {
  const entry: Record<string, unknown> = {};
  if (form.transport === "stdio") {
    entry.command = form.command;
    const args = splitArgs(form.args);
    if (args.length) entry.args = args;
    if (Object.keys(form.env).length) entry.env = form.env;
    if (form.cwd.trim()) entry.cwd = form.cwd.trim();
  } else {
    entry.url = form.url;
    // SSE 必须显式标注（与 Claude Code 等客户端配置的 "type": "sse" 写法一致）；
    // 不标注的远程 URL 一律按 Streamable HTTP 解析。
    if (form.transport === "sse") entry.type = "sse";
    if (form.bearer) entry.bearer = form.bearer;
    if (form.bearerEnv) entry.bearerEnv = form.bearerEnv;
    if (Object.keys(form.headers).length) entry.headers = form.headers;
    if (Object.keys(form.env).length) entry.env = form.env;
  }
  return JSON.stringify({ mcpServers: { [form.name || "server"]: entry } }, null, 2);
}

/**
 * mcpServers JSON 片段 → 表单（取首个条目；支持 mcpServers 包裹层与裸 map）。
 * transport 由 url/command 推导；解析失败抛 Error，其 message 会直接展示给用户。
 *
 * 收 `t` 而非抛错误码：调用方拿到就 setError 展示，中间再加一层码→文案的映射
 * 只是把同一件事拆到两处。
 */
export function parseServerSnippet(
  text: string,
  t: (key: string, options?: Record<string, unknown>) => string,
): SnippetForm {
  let value: unknown;
  try {
    value = JSON.parse(text.trim());
  } catch (error) {
    throw new Error(
      t("serverModal.parse.jsonFailed", {
        error: errorText(error),
      }),
    );
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(t("serverModal.parse.notObject"));
  }
  const root = value as Record<string, unknown>;
  const map = (
    typeof root.mcpServers === "object" &&
    root.mcpServers !== null &&
    !Array.isArray(root.mcpServers)
      ? root.mcpServers
      : root
  ) as Record<string, unknown>;
  const [name, rawEntry] = Object.entries(map)[0] ?? [];
  if (!name || typeof rawEntry !== "object" || rawEntry === null) {
    throw new Error(t("serverModal.parse.noEntry"));
  }
  const entry = rawEntry as Record<string, unknown>;
  const str = (key: string) => (typeof entry[key] === "string" ? (entry[key] as string) : "");
  const strMap = (key: string): Record<string, string> => {
    const raw = entry[key];
    if (typeof raw !== "object" || raw === null || Array.isArray(raw)) return {};
    return Object.fromEntries(
      Object.entries(raw).filter(([, v]) => typeof v === "string"),
    ) as Record<string, string>;
  };
  const url = str("url");
  const command = str("command");
  if (!url && !command) throw new Error(t("serverModal.parse.noTransport"));
  const declaredType = str("type") || str("transport");
  return {
    name,
    transport: url ? (declaredType === "sse" ? "sse" : "http") : "stdio",
    command,
    args: Array.isArray(entry.args)
      ? joinArgs(entry.args.filter((item): item is string => typeof item === "string"))
      : "",
    env: strMap("env"),
    cwd: str("cwd"),
    url,
    bearer: str("bearer"),
    bearerEnv: str("bearerEnv") || str("bearer_env"),
    headers: strMap("headers"),
  };
}

type SetupTab = "preset" | "json" | "stdio" | "http";
type EditTab = "general" | "tools";

/** 模块级常量取不到 t，故存 key，由 TabBar 的调用方翻译。 */
const TABS: { id: SetupTab; labelKey: string; icon: LucideIcon }[] = [
  { id: "preset", labelKey: "serverModal.tab.preset", icon: Sparkles },
  { id: "json", labelKey: "serverModal.tab.json", icon: FileJson },
  { id: "stdio", labelKey: "serverModal.tab.stdio", icon: SlidersHorizontal },
  { id: "http", labelKey: "serverModal.tab.http", icon: Globe },
];

/**
 * 下游状态 → 徽标（与 hub state.rs ServerStatus kebab-case 序列化对应）。
 *
 * 存 i18n key 而非文案：这套状态概览页与服务器页共用，各页各译必然漂移。
 */
export const STATUS_BADGE: Record<string, { tone: "run" | "warn" | "danger"; key: string }> = {
  ready: { tone: "run", key: "server.status.ready" },
  starting: { tone: "warn", key: "server.status.starting" },
  degraded: { tone: "warn", key: "server.status.degraded" },
  failed: { tone: "danger", key: "server.status.failed" },
  // 不是故障：远程服务器要求 OAuth 登录，等用户点「登录」。
  "needs-auth": { tone: "warn", key: "server.status.needsAuth" },
};

/** Tab 头（新建三 tab 与编辑双 tab 共用的下划线样式）。 */
function TabBar<T extends string>({
  tabs,
  active,
  onSelect,
}: {
  tabs: { id: T; label: string; icon: LucideIcon }[];
  active: T;
  onSelect: (id: T) => void;
}) {
  return (
    <div className="mb-5 flex gap-1 border-b border-line" role="tablist">
      {tabs.map(({ id, label, icon: Icon }) => (
        <button
          key={id}
          type="button"
          role="tab"
          aria-selected={active === id}
          onClick={() => onSelect(id)}
          className={cn(
            "-mb-px flex items-center gap-1.5 border-b-2 px-3 py-2 text-[13px] font-medium transition-colors",
            active === id
              ? "border-accent text-ink"
              : "border-transparent text-muted hover:text-ink",
          )}
        >
          <Icon size={15} strokeWidth={2} aria-hidden />
          {label}
        </button>
      ))}
    </div>
  );
}

/* ---------------- 键值行区块（环境变量 / 请求头共用） ---------------- */
function KeyValueSection({
  label,
  hint,
  addLabel,
  keyPlaceholder,
  valuePlaceholder,
  secretValue,
  rows,
  onChange,
}: {
  label: string;
  hint?: string;
  addLabel: string;
  keyPlaceholder: string;
  valuePlaceholder: string;
  /** 值按密文渲染（请求头里常见 token）。 */
  secretValue?: boolean;
  rows: KeyValueRow[];
  onChange: (rows: KeyValueRow[]) => void;
}) {
  const { t } = useTranslation();
  const filled = rows.filter((row) => row.key.trim()).length;
  return (
    <div>
      <div className="flex items-center justify-between">
        <span className="flex items-center gap-1.5 text-xs font-medium text-muted">
          {label}
          {filled > 0 ? <Tag>{t("serverModal.kv.filled", { count: filled })}</Tag> : null}
        </span>
        <Button size="sm" icon={Plus} onClick={() => onChange([...rows, makeRow()])}>
          {addLabel}
        </Button>
      </div>
      {rows.length === 0 ? (
        <p className="mt-2 text-[11px] text-faint">{hint ?? t("serverModal.kv.none")}</p>
      ) : (
        <div className="mt-2 space-y-2">
          {rows.map((row) => (
            <div key={row.rowId} className="flex items-center gap-2">
              <Field
                className="w-56 font-mono"
                value={row.key}
                onChange={(e) =>
                  onChange(
                    rows.map((r) => (r.rowId === row.rowId ? { ...r, key: e.target.value } : r)),
                  )
                }
                placeholder={keyPlaceholder}
              />
              <Field
                className="min-w-0 flex-1 font-mono"
                type={secretValue ? "password" : "text"}
                value={row.value}
                onChange={(e) =>
                  onChange(
                    rows.map((r) => (r.rowId === row.rowId ? { ...r, value: e.target.value } : r)),
                  )
                }
                placeholder={valuePlaceholder}
              />
              <IconButton
                icon={Trash2}
                tone="danger"
                aria-label={t("serverModal.kv.remove", {
                  label,
                  name: row.key || t("serverModal.kv.unnamed"),
                })}
                onClick={() => onChange(rows.filter((r) => r.rowId !== row.rowId))}
              />
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * 接入 MCP 服务器弹窗。
 * - 新建：导入 JSON / 手动创建（stdio）/ 远程服务器（HTTP）三 tab；
 * - 编辑：常规设置（表单/JSON 双模式 + 运行状态 + 启用开关 + 工作区归属 + 测试连接）
 *   与工具（当前工作区作用域的逐工具启停/别名/批量）双 tab。
 */
export function ServerSetupModal({
  editing,
  initialTab = "preset",
  onClose,
  onImported,
  onManageWorkspaces,
}: {
  editing?: ServerDef | null;
  /** 新建时落在哪个页签：页头「从文件或 JSON 导入」直接打开导入页签。 */
  initialTab?: "preset" | "json";
  onClose: () => void;
  /** 导入 JSON 成功后弹窗自动关闭，结果摘要交由页面内联展示。 */
  onImported?: (message: string) => void;
  /** 「管理工作区」跳转（由页面注入导航并负责关闭弹窗）。 */
  onManageWorkspaces?: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 这是全应用输入量最大的表单：点遮罩 / ESC 一次误触就全没。改过内容再关先弹确认。
  const guard = useUnsavedGuard(onClose);
  const refresh = () => invalidateServerQueries(queryClient);

  // 新建默认落在预设：多数人要装的就是那几个官方 Server，让最省事的路排在最前
  //（此前默认是「导入 JSON」，等于先要求用户从别处弄到一段配置）。
  // SSE 与 HTTP 共用「远程服务器」页签，由 remoteProtocol 细分。
  const [tab, setTab] = useState<SetupTab>(
    editing ? (editing.transport === "sse" ? "http" : editing.transport) : initialTab,
  );
  const [remoteProtocol, setRemoteProtocol] = useState<"http" | "sse">(
    editing?.transport === "sse" ? "sse" : "http",
  );
  const [editTab, setEditTab] = useState<EditTab>("general");
  const [error, setError] = useState<string | null>(null);

  // 手动创建 / 远程服务器 / 编辑 共用的表单状态
  const [name, setName] = useState(editing?.name ?? "");
  const [command, setCommand] = useState(editing?.command ?? "");
  const [args, setArgs] = useState(editing ? joinArgs(editing.args) : "");
  const [cwd, setCwd] = useState(editing?.cwd ?? "");
  const [url, setUrl] = useState(editing?.url ?? "");
  const [bearer, setBearer] = useState(editing?.bearer ?? "");
  const [bearerEnv, setBearerEnv] = useState(editing?.bearerEnv ?? "");
  /* 已经配过就默认展开：折起来会让人以为这台服务器没设过令牌环境变量，
   * 进而重新填一遍。折叠是收纳空白项，不是藏起已有的值。 */
  const [bearerEnvOpen, setBearerEnvOpen] = useState(() => Boolean(editing?.bearerEnv));
  const [enabled, setEnabled] = useState(editing?.enabled ?? true);
  const [envRows, setEnvRows] = useState<KeyValueRow[]>(() => recordToRows(editing?.env ?? {}));
  const [headerRows, setHeaderRows] = useState<KeyValueRow[]>(() =>
    recordToRows(editing?.headers ?? {}),
  );

  // 导入 JSON（新建）：面板与底栏按钮共用一份状态。导入成功即关窗，结果摘要交给页面。
  const importer = useServerImport((message) => {
    onImported?.(message);
    onClose();
  });

  /**
   * 预设填表：把值灌进手动表单并切到对应 transport 页签。
   *
   * 刻意**不直接保存**——落到表单上用户才看得见自己到底装了什么，也才能先按
   * 「测试连通」再决定。预设只是省去背包名和参数，不接管创建流程。
   */
  const applyPresetFill = (fill: PresetFill) => {
    setName(fill.name);
    setCommand(fill.command);
    setArgs(joinArgs(fill.args));
    setUrl(fill.url);
    setBearer(fill.bearer);
    setEnvRows(recordToRows(fill.env));
    // 预设的语义是「从这个预设重新开始」，故连预设不涉及的字段一并清空。
    // 漏清会串味：先在手动页签填过工作目录、再回来选 filesystem，Server 会跑在
    // 那个残留目录下；先在远程页签填过请求头、再选 github，那些头会被发给
    // GitHub API。这几个字段 serverInput() 只按 tab 过滤，过滤不掉同 transport
    // 的残留。
    setCwd("");
    setBearerEnv("");
    setHeaderRows(recordToRows(fill.headers ?? {}));
    setError(null);
    setTestResult(null);
    if (fill.transport === "stdio") {
      setTab("stdio");
    } else {
      setRemoteProtocol(fill.transport);
      setTab("http");
    }
  };

  // 编辑 JSON 模式状态
  const [jsonMode, setJsonMode] = useState(false);
  const [jsonDraft, setJsonDraft] = useState("");
  const [copied, setCopied] = useState(false);

  /** `tab` 还有 "preset"/"json" 两个取值，这里归一成真实 transport。 */
  const effectiveTransport = (): ServerTransport => (tab === "http" ? remoteProtocol : "stdio");

  const snippetForm = (): SnippetForm => ({
    name,
    transport: effectiveTransport(),
    command,
    args,
    env: rowsToRecord(envRows),
    cwd,
    url,
    bearer,
    bearerEnv,
    headers: rowsToRecord(headerRows),
  });

  const applyForm = (form: SnippetForm) => {
    setName(form.name);
    setCommand(form.command);
    setArgs(form.args);
    setCwd(form.cwd);
    setUrl(form.url);
    setBearer(form.bearer);
    setBearerEnv(form.bearerEnv);
    setEnvRows(recordToRows(form.env));
    setHeaderRows(recordToRows(form.headers));
    if (form.transport !== "stdio") setRemoteProtocol(form.transport);
  };

  const enterJsonMode = () => {
    setJsonDraft(buildServerSnippet(snippetForm()));
    setJsonMode(true);
    setError(null);
  };

  const applyJsonDraft = (): boolean => {
    try {
      const form = parseServerSnippet(jsonDraft, t);
      if (editing && form.transport !== editing.transport) {
        throw new Error(
          t("serverModal.transportLocked", { from: editing.transport, to: form.transport }),
        );
      }
      applyForm(form);
      setJsonMode(false);
      setError(null);
      return true;
    } catch (parseError) {
      setError(errorText(parseError));
      return false;
    }
  };

  const serverInput = () => ({
    id: editing?.id ?? null,
    name,
    // 与 snippetForm() 同一套归一：`tab` 还有 "preset" / "json" 两个取值，
    // 直接透传就会把它们写进库。footer 会在那两个页签上藏掉保存按钮，但那是
    // 用 UI 布局兜类型安全——布局哪天改错，坏数据就落库了。这里收死。
    transport: effectiveTransport(),
    command: tab === "stdio" ? command : null,
    args: tab === "stdio" ? splitArgs(args) : [],
    env: rowsToRecord(envRows),
    cwd: tab === "stdio" && cwd.trim() ? cwd.trim() : null,
    url: tab === "http" ? url : null,
    bearer: tab === "http" && bearer ? bearer : null,
    bearerEnv: tab === "http" && bearerEnv ? bearerEnv : null,
    headers: tab === "http" ? rowsToRecord(headerRows) : {},
    enabled: editing ? enabled : null,
  });

  const save = useMutation({
    mutationFn: () => ipc.serverUpsert(serverInput()),
    onSuccess: () => {
      refresh();
      onClose();
    },
    onError: (mutationError) => {
      setError(errorText(mutationError));
      // Err 可能是「已落库但 Hub 未生效」：弹窗留在原地展示错误，
      // 背后的列表刷新到 DB 真值。
      refresh();
    },
  });

  // 测试连接：不落库，经 Hub connect + tools/list（解析环境与真实运行一致）。
  const [testResult, setTestResult] = useState<ServerTestResult | null>(null);
  const test = useMutation({
    mutationFn: () => {
      // JSON 模式下先应用草稿，保证测的是屏幕上的内容。
      if (jsonMode && !applyJsonDraft())
        return Promise.reject(new Error(t("serverModal.fixJsonFirst")));
      return ipc.serverTest(serverInput());
    },
    onMutate: () => setTestResult(null),
    onSuccess: (result) => setTestResult(result),
    onError: (mutationError) => setTestResult({ ok: false, error: errorText(mutationError) }),
  });

  // 编辑态：Hub 诊断里的运行状态。
  const diagnostics = useQuery({
    queryKey: queryKeys.hubDiagnostics,
    queryFn: ipc.hubDiagnostics,
    enabled: !!editing,
    retry: false,
  });
  // 查询 error 说明 Hub 不可达，此时 data 是最后一次成功的陈旧缓存，
  // 直接用会把早已死掉的下游显示成「运行中」（与服务器页 diagnosticsView 同口径；
  // 不直接复用它是因为页面模块 import 本组件，反向引用会成环）。
  const downstream =
    editing && !diagnostics.isError
      ? (
          diagnostics.data?.downstreams as
            | Record<string, { status?: string; crashCount?: number; lastError?: string | null }>
            | undefined
        )?.[editing.id]
      : undefined;
  const statusBadge = downstream?.status ? STATUS_BADGE[downstream.status] : undefined;

  const slugPreview = editing ? editing.slug : mcpaneHubSlug(name);
  const canSave = !!name && (tab === "stdio" ? !!command : !!url);

  const saveAction = () => {
    if (jsonMode && !applyJsonDraft()) return;
    save.mutate();
  };

  const copySnippet = async () => {
    await navigator.clipboard.writeText(jsonMode ? jsonDraft : buildServerSnippet(snippetForm()));
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  const nameField = (
    <LabeledField
      label={t("serverModal.name")}
      required
      hint={name || editing ? t("serverModal.toolPrefix", { slug: slugPreview }) : undefined}
    >
      <Field
        // 弹窗打开即聚焦首字段，用户可直接输入（Modal 已守卫不抢子元素焦点）
        autoFocus
        value={name}
        onChange={(e) => setName(e.target.value)}
        placeholder={t("serverModal.namePlaceholder")}
      />
    </LabeledField>
  );

  const envSection = (
    <KeyValueSection
      label={t("serverModal.env")}
      hint={t("serverModal.envHint")}
      addLabel={t("serverModal.envAdd")}
      keyPlaceholder={t("serverModal.envKeyPlaceholder")}
      valuePlaceholder={t("serverModal.valuePlaceholder")}
      rows={envRows}
      onChange={setEnvRows}
    />
  );

  const testResultLine = testResult ? (
    testResult.ok ? (
      <p className="text-xs font-medium text-run">
        {t("serverModal.testOk", { count: testResult.toolCount })}
        {testResult.elapsedMs != null ? `（${testResult.elapsedMs}ms）` : ""}
        {testResult.tools?.length ? (
          <span className="mt-1 block max-h-16 overflow-y-auto font-mono font-normal text-faint">
            {testResult.tools.join("、")}
          </span>
        ) : null}
      </p>
    ) : (
      <p className="text-xs text-danger">
        {t("serverModal.testFail", { error: testResult.error })}
      </p>
    )
  ) : null;

  const stdioForm = (
    <div className="space-y-4">
      {nameField}
      <LabeledField label={t("serverModal.command")} required hint={t("serverModal.commandHint")}>
        <Field
          className="font-mono"
          value={command}
          onChange={(e) => setCommand(e.target.value)}
          placeholder="npx"
        />
      </LabeledField>
      <LabeledField label={t("serverModal.args")} hint={t("serverModal.argsHint")}>
        <Field
          className="font-mono"
          value={args}
          onChange={(e) => setArgs(e.target.value)}
          placeholder="-y @modelcontextprotocol/server-memory"
        />
      </LabeledField>
      {envSection}
      <LabeledField label={t("serverModal.cwd")}>
        <Field
          className="font-mono"
          value={cwd}
          onChange={(e) => setCwd(e.target.value)}
          placeholder="/path/to/project"
        />
      </LabeledField>
    </div>
  );

  // 编辑态锁定协议（与 transportLocked 同理：改传输等于换了个 Server）。
  const protocolToggle = (
    <LabeledField
      label={t("serverModal.protocol")}
      hint={remoteProtocol === "sse" ? t("serverModal.protocolSseHint") : undefined}
    >
      {/* toggle-button 组（aria-pressed）：radio 语义会被 a11y lint 要求换
          原生 input，而这里的分段按钮样式依赖 button。 */}
      <div className="flex gap-1 self-start rounded-lg border border-line p-0.5">
        {(["http", "sse"] as const).map((protocol) => (
          <button
            key={protocol}
            type="button"
            aria-pressed={remoteProtocol === protocol}
            disabled={!!editing}
            onClick={() => setRemoteProtocol(protocol)}
            className={cn(
              "rounded-md px-2.5 py-1 text-xs font-medium transition-colors",
              remoteProtocol === protocol ? "bg-accent-soft text-ink" : "text-muted hover:text-ink",
              editing && "cursor-not-allowed opacity-60",
            )}
          >
            {t(protocol === "http" ? "serverModal.protocolHttp" : "serverModal.protocolSse")}
          </button>
        ))}
      </div>
    </LabeledField>
  );

  const httpForm = (
    <div className="space-y-4">
      {nameField}
      {protocolToggle}
      <LabeledField
        label="URL"
        required
        hint={t(remoteProtocol === "sse" ? "serverModal.sseUrlHint" : "serverModal.urlHint")}
      >
        <Field
          className="font-mono"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder={
            remoteProtocol === "sse" ? "https://example.com/sse" : "https://example.com/mcp"
          }
        />
      </LabeledField>
      <LabeledField label={t("serverModal.bearer")} hint={t("serverModal.bearerHint")}>
        <Field
          className="font-mono"
          type="password"
          value={bearer}
          onChange={(e) => setBearer(e.target.value)}
          placeholder={t("serverModal.bearerPlaceholder")}
        />
      </LabeledField>
      <KeyValueSection
        label={t("serverModal.headers")}
        hint={t("serverModal.headersHint")}
        addLabel={t("serverModal.headersAdd")}
        keyPlaceholder={t("serverModal.headerKeyPlaceholder")}
        valuePlaceholder={t("serverModal.valuePlaceholder")}
        secretValue
        rows={headerRows}
        onChange={setHeaderRows}
      />
      {envSection}
      {/* 全项目唯一一处原生 details：三角标、字体、展开动画都是浏览器自带的那套，
          与旁边所有折叠段（都走 CollapseToggle / Accordion）对不上。 */}
      <div>
        <CollapseToggle open={bearerEnvOpen} onToggle={() => setBearerEnvOpen((v) => !v)}>
          {t("serverModal.bearerEnvToggle")}
        </CollapseToggle>
        {bearerEnvOpen ? (
          <div className="mt-3">
            <LabeledField label={t("serverModal.bearerEnv")} hint={t("serverModal.bearerEnvHint")}>
              <Field
                className="font-mono"
                value={bearerEnv}
                onChange={(e) => setBearerEnv(e.target.value)}
                placeholder="MY_MCP_TOKEN"
              />
            </LabeledField>
          </div>
        ) : null}
      </div>
    </div>
  );

  /* ---------------- 编辑态：工作区归属 ---------------- */
  const { selectedId, workspaces } = useWorkspace();
  const customWorkspaces = workspaces.filter((ws) => ws.id !== DEFAULT_WS);
  const membership = useQuery({
    queryKey: queryKeys.workspaceMembership(customWorkspaces.map((ws) => ws.id)),
    queryFn: async () => {
      const entries = await Promise.all(
        customWorkspaces.map(async (ws) => [ws.id, await ipc.workspaceServers(ws.id)]),
      );
      return Object.fromEntries(entries) as Record<string, string[]>;
    },
    enabled: !!editing && customWorkspaces.length > 0,
    retry: false,
  });
  const [pendingWs, setPendingWs] = useState<Set<string>>(new Set());
  const setMember = useMutation({
    mutationFn: (input: { workspaceId: string; serverId: string; enabled: boolean }) =>
      ipc.workspaceServerSet(input.workspaceId, input.serverId, input.enabled),
    onMutate: ({ workspaceId }) => {
      setError(null);
      setPendingWs((prev) => new Set(prev).add(workspaceId));
    },
    onSettled: (_data, _error, { workspaceId }) =>
      setPendingWs((prev) => {
        const next = new Set(prev);
        next.delete(workspaceId);
        return next;
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.workspaceMembershipAll });
      queryClient.invalidateQueries({ queryKey: queryKeys.workspaceServersAll });
      queryClient.invalidateQueries({ queryKey: queryKeys.catalogAll });
    },
    onError: (mutationError) => setError(errorText(mutationError)),
  });

  const workspaceSection = editing ? (
    <div>
      <div className="flex items-center justify-between">
        <span className="text-xs font-medium text-muted">{t("serverModal.workspaces")}</span>
        {onManageWorkspaces ? (
          <Button size="sm" variant="ghost" onClick={onManageWorkspaces}>
            {t("serverModal.manageWorkspaces")}
          </Button>
        ) : null}
      </div>
      <div className="mt-2 flex flex-wrap gap-2">
        <span className="flex items-center gap-2 rounded-lg border border-line bg-app px-3 py-1.5 text-sm text-muted">
          {t("workspace.default")}
          <Tag>{t("serverModal.alwaysIncluded")}</Tag>
        </span>
        {customWorkspaces.map((ws) => {
          const on = (membership.data?.[ws.id] ?? []).includes(editing.id);
          const loading = pendingWs.has(ws.id);
          return (
            <label
              key={ws.id}
              aria-busy={loading}
              className={cn(
                "flex cursor-pointer items-center gap-2 rounded-lg border px-3 py-1.5 text-sm transition-colors",
                on
                  ? "border-accent bg-accent-soft text-ink"
                  : "border-line-strong text-muted hover:bg-hover",
                loading && "opacity-70",
              )}
            >
              <input
                type="checkbox"
                className="accent-accent"
                checked={on}
                disabled={loading || membership.isLoading}
                onChange={(e) =>
                  setMember.mutate({
                    workspaceId: ws.id,
                    serverId: editing.id,
                    enabled: e.target.checked,
                  })
                }
              />
              {ws.name}
            </label>
          );
        })}
        {customWorkspaces.length === 0 ? (
          <span className="self-center text-[11px] text-faint">
            {t("serverModal.noCustomWorkspaces")}
          </span>
        ) : null}
      </div>
      <p className="mt-1.5 text-[11px] text-faint">{t("serverModal.workspaceHint")}</p>
    </div>
  ) : null;

  /* ---------------- 编辑态：工具 tab（当前工作区作用域） ---------------- */
  const catalog = useQuery({
    queryKey: queryKeys.catalog(selectedId),
    queryFn: () => ipc.catalogList(selectedId === DEFAULT_WS ? null : selectedId),
    enabled: !!editing,
    // 与 Workspaces 页对齐的轮询：hubDown 取自 catalog.isError，不轮询的话
    // 它只是弹窗打开那一刻的快照——弹窗开着时 Hub 挂掉，置灰永远不会生效。
    ...livePoll(10000),
  });
  const overrides = useQuery({
    queryKey: queryKeys.toolOverrides(selectedId),
    queryFn: () => ipc.workspaceToolOverrides(selectedId === DEFAULT_WS ? null : selectedId),
    enabled: !!editing,
    retry: false,
  });
  const itemWs = selectedId === DEFAULT_WS ? null : selectedId;
  const promptOverrides = useQuery({
    queryKey: queryKeys.itemOverrides("prompt", selectedId),
    queryFn: () => ipc.workspaceItemOverrides("prompt", itemWs),
    enabled: !!editing,
    retry: false,
  });
  const resourceOverrides = useQuery({
    queryKey: queryKeys.itemOverrides("resource", selectedId),
    queryFn: () => ipc.workspaceItemOverrides("resource", itemWs),
    enabled: !!editing,
    retry: false,
  });
  const toolRows = useMemo(
    () =>
      editing
        ? mergeServerTools(
            catalog.data?.tools ?? [],
            overrides.data ?? [],
            editing.id,
            editing.slug,
          )
        : [],
    [catalog.data, overrides.data, editing],
  );
  // 本 Server 有多少启用中的工具既没下游申报、也没用户声明。
  const unannotatedCount = toolRows.filter(
    (row) => row.enabled && !row.annotations && !row.declaredAnnotations,
  ).length;
  // Prompts/Resources 治理行（Catalog 现役 + overrides 里被禁用的缺席项）。
  const promptRows = useMemo(() => {
    if (!editing) return [];
    const active = (catalog.data?.prompts ?? [])
      .filter((p) => p.serverId === editing.id)
      .map((p) => ({
        originalName: p.originalName,
        exposedName: p.exposedName,
        description: p.raw.description ?? null,
        enabled: true,
      }));
    const activeNames = new Set(active.map((r) => r.originalName));
    const disabled = (promptOverrides.data ?? [])
      .filter((o) => o.serverId === editing.id && !o.enabled && !activeNames.has(o.originalName))
      .map((o) => ({
        originalName: o.originalName,
        exposedName: `${editing.slug}__${o.originalName}`,
        description: null,
        enabled: false,
      }));
    return [...active, ...disabled].sort((a, b) => a.originalName.localeCompare(b.originalName));
  }, [catalog.data, promptOverrides.data, editing]);
  const resourceRows = useMemo(() => {
    if (!editing) return [];
    const active = (catalog.data?.resources ?? [])
      .filter((r) => r.serverId === editing.id)
      .map((r) => ({ uri: r.uri, name: r.raw.name ?? null, enabled: true }));
    const activeUris = new Set(active.map((r) => r.uri));
    const disabled = (resourceOverrides.data ?? [])
      .filter((o) => o.serverId === editing.id && !o.enabled && !activeUris.has(o.originalName))
      .map((o) => ({ uri: o.originalName, name: null, enabled: false }));
    return [...active, ...disabled].sort((a, b) => a.uri.localeCompare(b.uri));
  }, [catalog.data, resourceOverrides.data, editing]);
  const [pendingItems, setPendingItems] = useState<Set<string>>(new Set());
  const toggleItem = useMutation({
    mutationFn: (input: { kind: "prompt" | "resource"; originalName: string; enabled: boolean }) =>
      ipc.workspaceItemToggle({
        kind: input.kind,
        workspaceId: itemWs,
        serverId: editing?.id,
        originalName: input.originalName,
        enabled: input.enabled,
        alias: null,
      }),
    onMutate: ({ kind, originalName }) => {
      setError(null);
      setPendingItems((prev) => new Set(prev).add(`${kind}:${originalName}`));
    },
    onSettled: (_data, _error, { kind, originalName }) =>
      setPendingItems((prev) => {
        const next = new Set(prev);
        next.delete(`${kind}:${originalName}`);
        return next;
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: queryKeys.itemOverrides("prompt", selectedId) });
      queryClient.invalidateQueries({ queryKey: queryKeys.itemOverrides("resource", selectedId) });
      refreshTools();
    },
    onError: (mutationError) => {
      setError(errorText(mutationError));
      // 同 toggleTool：Err 可能已落库，刷新到 DB 真值。
      refreshTools();
    },
  });

  const [toolSearch, setToolSearch] = useState("");
  const [aliasDraft, setAliasDraft] = useState<Record<string, string>>({});
  // 同 Workspaces.tsx：这两个依赖是变更触发器，不是 effect 体里被读取的值。
  // 删成 [] 会让 alias 草稿在切换工作区/服务器时残留下来。
  // biome-ignore lint/correctness/useExhaustiveDependencies: 依赖是变更触发器，删掉会让草稿跨切换残留
  useEffect(() => {
    setAliasDraft({});
  }, [selectedId, editing?.id]);
  const visibleTools = toolRows.filter(
    (row) =>
      !toolSearch ||
      row.originalName.includes(toolSearch) ||
      row.exposedName.includes(toolSearch) ||
      (row.description ?? "").includes(toolSearch),
  );
  // Hub 不可达时列表仍会用上一次成功的缓存渲染。命令层如今会在「写库成功但
  // Hub 未生效」时上抛（onError 能触发了），但置灰仍然值得：与其让用户翻转
  // 开关后收到一条报错，不如一开始就把注定不生效的操作挡在门外。
  const hubDown = catalog.isError;
  const refreshTools = () => {
    queryClient.invalidateQueries({ queryKey: queryKeys.catalog(selectedId) });
    queryClient.invalidateQueries({ queryKey: queryKeys.toolOverrides(selectedId) });
    queryClient.invalidateQueries({ queryKey: queryKeys.hubDiagnostics });
  };
  const toolToggleInput = (originalName: string, nextEnabled: boolean, alias?: string | null) => ({
    workspaceId: selectedId === DEFAULT_WS ? null : selectedId,
    serverId: editing?.id,
    originalName,
    enabled: nextEnabled,
    alias: alias ?? null,
  });
  const [pendingTools, setPendingTools] = useState<Set<string>>(new Set());
  const toggleTool = useMutation({
    mutationFn: (input: { originalName: string; enabled: boolean; alias?: string | null }) =>
      ipc.workspaceToolToggle(toolToggleInput(input.originalName, input.enabled, input.alias)),
    onMutate: ({ originalName }) => {
      setError(null);
      setPendingTools((prev) => new Set(prev).add(originalName));
    },
    onSettled: (_data, _error, { originalName }) =>
      setPendingTools((prev) => {
        const next = new Set(prev);
        next.delete(originalName);
        return next;
      }),
    onSuccess: refreshTools,
    onError: (mutationError) => {
      setError(errorText(mutationError));
      // Err 可能是「已落库但 Hub 未生效」，刷新收敛，别停在旧快照。
      refreshTools();
    },
  });
  // 注解声明（全局，非按工作区）——与启停/别名分属两张表，互不影响。
  const [annotating, setAnnotating] = useState<ToolRow | null>(null);
  const setAnnotations = useMutation({
    mutationFn: (input: { row: ToolRow; annotations: DeclaredAnnotations | null }) =>
      ipc.toolAnnotationsSet({
        serverId: editing?.id ?? "",
        originalName: input.row.originalName,
        annotations: input.annotations,
      }),
    onMutate: () => setError(null),
    onSuccess: () => {
      setAnnotating(null);
      refreshTools();
    },
    onError: (mutationError) => {
      setError(errorText(mutationError));
      refreshTools();
    },
  });

  const [batchPending, setBatchPending] = useState(false);
  const batchToggle = async (enabledAll: boolean) => {
    setBatchPending(true);
    setError(null);
    try {
      await Promise.all(
        toolRows
          .filter((row) => row.enabled !== enabledAll)
          .map((row) => ipc.workspaceToolToggle(toolToggleInput(row.originalName, enabledAll))),
      );
      refreshTools();
    } catch (batchError) {
      // 批量开关任一项失败：Promise.all 会整体 reject，此前无 catch 会变成
      // 未处理 rejection，用户既看不到报错、也不知道部分工具没切成。
      setError(errorText(batchError));
      // 部分开关可能已成功落库：刷新列表让 UI 收敛到真实状态，而不是停在旧快照。
      refreshTools();
    } finally {
      setBatchPending(false);
    }
  };

  const workspaceName =
    selectedId === DEFAULT_WS
      ? t("workspace.default")
      : (workspaces.find((ws) => ws.id === selectedId)?.name ?? t("workspace.default"));

  const toolsTab = editing ? (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Field
          type="search"
          className="w-52"
          value={toolSearch}
          onChange={(e) => setToolSearch(e.target.value)}
          placeholder={t("serverModal.searchTools")}
        />
        <Tag>
          {t("serverModal.toolsEnabled", {
            enabled: toolRows.filter((row) => row.enabled).length,
            total: toolRows.length,
          })}
        </Tag>
        <span className="flex-1" />
        <Button
          size="sm"
          loading={batchPending}
          disabled={hubDown || toolRows.every((row) => row.enabled)}
          onClick={() => batchToggle(true)}
        >
          {t("serverModal.enableAll")}
        </Button>
        <Button
          size="sm"
          loading={batchPending}
          disabled={hubDown || toolRows.every((row) => !row.enabled)}
          onClick={() => batchToggle(false)}
        >
          {t("serverModal.disableAll")}
        </Button>
      </div>
      <p className="text-[11px] text-faint">
        {t("serverModal.toolsScope", { workspace: workspaceName })}
      </p>
      {/* "这个服务器一个注解都不申报"是服务器级的事实，不是逐个工具的巧合——
          刚装完服务器正是发现它的时机，比等到 codex exec 报错才知道强。
          只说明不代劳：逐个工具确认，不给一键铺全部。 */}
      {unannotatedCount > 0 ? (
        <p className="text-xs text-warn">
          {t("serverModal.unannotated", { count: unannotatedCount })}
        </p>
      ) : null}

      {catalog.isError ? (
        <p className="text-sm text-danger">{t("serverModal.toolsHubDown")}</p>
      ) : null}
      {catalog.isSuccess && toolRows.length === 0 ? (
        <p className="text-sm text-muted">
          {editing.enabled ? t("serverModal.toolsNone") : t("serverModal.toolsDisabled")}
        </p>
      ) : null}

      <div className="space-y-2">
        {visibleTools.map((row) => {
          const loading = pendingTools.has(row.originalName);
          return (
            <div
              key={row.originalName}
              className="flex items-center justify-between gap-3 rounded-lg border border-line px-3 py-2.5"
            >
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <code className="truncate font-mono text-[13px] text-ink">
                    {row.originalName}
                  </code>
                  {row.enabled ? (
                    <code className="truncate font-mono text-[11px] text-faint">
                      → {row.exposedName}
                    </code>
                  ) : (
                    <Tag>{t("serverModal.toolDisabled")}</Tag>
                  )}
                  {/* 下游申报了就不标（正常情况不该刷屏）；用户标的写明是用户标的。 */}
                  {!row.enabled || row.annotations ? null : row.declaredAnnotations ? (
                    <Tag>{t("workspaces.annotationsByUser")}</Tag>
                  ) : (
                    <Tag tone="amber">{t("workspaces.annotationsMissing")}</Tag>
                  )}
                </div>
                {row.description ? (
                  <p className="mt-0.5 max-w-md truncate text-xs text-faint">{row.description}</p>
                ) : null}
              </div>
              <div className="flex shrink-0 items-center gap-2">
                {row.enabled ? (
                  <>
                    <Field
                      value={aliasDraft[row.originalName] ?? ""}
                      onChange={(e) =>
                        setAliasDraft((draft) => ({
                          ...draft,
                          [row.originalName]: e.target.value,
                        }))
                      }
                      placeholder={t("common.aliasPlaceholder")}
                      className="h-8 w-28 px-2.5 font-mono text-xs"
                    />
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={hubDown || !aliasDraft[row.originalName]}
                      loading={loading}
                      onClick={() =>
                        toggleTool.mutate({
                          originalName: row.originalName,
                          enabled: true,
                          alias: aliasDraft[row.originalName],
                        })
                      }
                    >
                      {t("serverModal.setAlias")}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={hubDown}
                      onClick={() => setAnnotating(row)}
                    >
                      {t("workspaces.annotations")}
                    </Button>
                  </>
                ) : null}
                <Switch
                  checked={row.enabled}
                  disabled={hubDown}
                  loading={loading}
                  label={t("serverModal.toggleTool", {
                    action: t(row.enabled ? "serverModal.disableAll" : "serverModal.enableAll"),
                    name: row.originalName,
                  })}
                  onChange={(next) =>
                    toggleTool.mutate({ originalName: row.originalName, enabled: next })
                  }
                />
              </div>
            </div>
          );
        })}
      </div>

      {annotating ? (
        <ToolAnnotationsModal
          toolName={annotating.exposedName}
          downstream={annotating.annotations}
          declared={annotating.declaredAnnotations}
          saving={setAnnotations.isPending}
          onSave={(annotations) => setAnnotations.mutate({ row: annotating, annotations })}
          onClose={() => setAnnotating(null)}
        />
      ) : null}

      {promptRows.length > 0 ? (
        <div className="pt-2">
          <p className="mb-2 text-xs font-medium text-muted">
            {t("serverModal.promptsSection", { count: promptRows.length })}
          </p>
          <div className="space-y-2">
            {promptRows.map((row) => {
              const loading = pendingItems.has(`prompt:${row.originalName}`);
              return (
                <div
                  key={row.originalName}
                  className="flex items-center justify-between gap-3 rounded-lg border border-line px-3 py-2.5"
                >
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <code className="truncate font-mono text-[13px] text-ink">
                        {row.originalName}
                      </code>
                      {row.enabled ? (
                        <code className="truncate font-mono text-[11px] text-faint">
                          → {row.exposedName}
                        </code>
                      ) : (
                        <Tag>{t("serverModal.toolDisabled")}</Tag>
                      )}
                    </div>
                    {row.description ? (
                      <p className="mt-0.5 max-w-md truncate text-xs text-faint">
                        {row.description}
                      </p>
                    ) : null}
                  </div>
                  <Switch
                    checked={row.enabled}
                    disabled={hubDown}
                    loading={loading}
                    label={t("serverModal.toggleItem", { name: row.originalName })}
                    onChange={(next) =>
                      toggleItem.mutate({
                        kind: "prompt",
                        originalName: row.originalName,
                        enabled: next,
                      })
                    }
                  />
                </div>
              );
            })}
          </div>
        </div>
      ) : null}

      {resourceRows.length > 0 ? (
        <div className="pt-2">
          <p className="mb-2 text-xs font-medium text-muted">
            {t("serverModal.resourcesSection", { count: resourceRows.length })}
          </p>
          <div className="space-y-2">
            {resourceRows.map((row) => {
              const loading = pendingItems.has(`resource:${row.uri}`);
              return (
                <div
                  key={row.uri}
                  className="flex items-center justify-between gap-3 rounded-lg border border-line px-3 py-2.5"
                >
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <code className="truncate font-mono text-[13px] text-ink" title={row.uri}>
                        {row.uri}
                      </code>
                      {!row.enabled ? <Tag>{t("serverModal.toolDisabled")}</Tag> : null}
                    </div>
                    {row.name ? (
                      <p className="mt-0.5 max-w-md truncate text-xs text-faint">{row.name}</p>
                    ) : null}
                  </div>
                  <Switch
                    checked={row.enabled}
                    disabled={hubDown}
                    loading={loading}
                    label={t("serverModal.toggleItem", { name: row.uri })}
                    onChange={(next) =>
                      toggleItem.mutate({ kind: "resource", originalName: row.uri, enabled: next })
                    }
                  />
                </div>
              );
            })}
          </div>
        </div>
      ) : null}
    </div>
  ) : null;

  /* ---------------- 常规设置（编辑态） ---------------- */
  const generalEditTab = editing ? (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center gap-3 rounded-lg border border-line bg-app px-3 py-2.5">
        <StatusBadge tone={statusBadge?.tone ?? "stop"}>
          {statusBadge
            ? t(statusBadge.key)
            : (downstream?.status ??
              t(editing.enabled ? "server.status.disconnected" : "server.status.disabled"))}
        </StatusBadge>
        {downstream?.crashCount ? (
          <span className="text-[11px] text-danger">
            {t("server.crashes", { count: downstream.crashCount })}
          </span>
        ) : null}
        <Tag>{editing.transport}</Tag>
        <span className="flex-1" />
        <span className="flex items-center gap-2 text-xs text-muted">
          {t("serverModal.enabledLabel")}
          <Switch checked={enabled} onChange={setEnabled} label={t("serverModal.enableServer")} />
        </span>
      </div>

      <div className="flex items-center justify-between">
        <div className="flex gap-1 rounded-lg border border-line p-0.5" role="tablist">
          {(["form", "json"] as const).map((mode) => (
            <button
              key={mode}
              type="button"
              role="tab"
              aria-selected={jsonMode === (mode === "json")}
              onClick={() => {
                if (mode === "json" && !jsonMode) enterJsonMode();
                if (mode === "form" && jsonMode) applyJsonDraft();
              }}
              className={cn(
                "rounded-md px-2.5 py-1 text-xs font-medium transition-colors",
                jsonMode === (mode === "json")
                  ? "bg-accent-soft text-ink"
                  : "text-muted hover:text-ink",
              )}
            >
              {t(mode === "form" ? "serverModal.modeForm" : "serverModal.modeJson")}
            </button>
          ))}
        </div>
        <Button size="sm" variant="ghost" icon={copied ? Check : Copy} onClick={copySnippet}>
          {t(copied ? "serverModal.copied" : "serverModal.copyConfig")}
        </Button>
      </div>

      {jsonMode ? (
        <JsonEditor value={jsonDraft} onChange={setJsonDraft} minHeight={260} />
      ) : (
        <>
          {tab === "stdio" ? stdioForm : httpForm}
          {workspaceSection}
        </>
      )}
      {testResultLine}
    </div>
  ) : null;

  /* ---------------- 组装 ---------------- */
  // 预设页签不给保存/测试：那里还没有 transport 可言，存什么都是错的。
  // 选完预设会自动切到 stdio/http，保存按钮自然就出现。
  // （serverInput() 里 transport 已自行归一，不依赖这里兜底。）
  const footer =
    (editing && editTab === "tools") || (!editing && tab === "preset") ? null : tab === "json" &&
      !editing ? (
      <ServerImportAction importer={importer} />
    ) : (
      <div className="flex w-full items-center gap-2">
        <Button
          icon={PlugZap}
          loading={test.isPending}
          disabled={!canSave}
          onClick={() => test.mutate()}
        >
          {t("serverModal.testConnection")}
        </Button>
        <span className="flex-1" />
        <Button
          variant="primary"
          icon={editing ? undefined : Plus}
          disabled={!canSave}
          loading={save.isPending}
          onClick={saveAction}
        >
          {t(editing ? "serverModal.saveEdit" : "serverModal.createServer")}
        </Button>
      </div>
    );

  return (
    <Modal
      title={
        editing ? t("serverModal.editTitle", { name: editing.name }) : t("serverModal.addTitle")
      }
      size="lg"
      onClose={guard.requestClose}
      footer={footer}
    >
      {/* 容器级脏检测：change/input 事件冒泡，一处接住所有字段输入。 */}
      <div className="contents" onChangeCapture={guard.markDirty} onInputCapture={guard.markDirty}>
        {editing ? (
          <TabBar
            tabs={[
              {
                id: "general" as const,
                label: t("serverModal.tab.general"),
                icon: SlidersHorizontal,
              },
              {
                id: "tools" as const,
                label: t("serverModal.tab.tools", { count: toolRows.length }),
                icon: Wrench,
              },
            ]}
            active={editTab}
            onSelect={setEditTab}
          />
        ) : (
          <TabBar
            tabs={TABS.map(({ id, labelKey, icon }) => ({ id, label: t(labelKey), icon }))}
            active={tab}
            onSelect={(id) => {
              setTab(id);
              setError(null);
              setTestResult(null);
            }}
          />
        )}

        {editing ? (
          editTab === "general" ? (
            generalEditTab
          ) : (
            toolsTab
          )
        ) : (
          <>
            {tab === "preset" ? (
              <>
                <ServerDiscoverPanel onPick={applyPresetFill} />
                <ServerPresetPicker onPick={applyPresetFill} />
              </>
            ) : null}
            {tab === "json" ? <ServerImportPanel importer={importer} /> : null}
            {tab === "stdio" ? stdioForm : null}
            {tab === "http" ? httpForm : null}
            {tab === "stdio" || tab === "http" ? (
              <div className="mt-3">{testResultLine}</div>
            ) : null}
          </>
        )}

        {error ? <p className="mt-3 text-xs text-danger">{error}</p> : null}
      </div>

      {guard.confirmOpen ? (
        <ConfirmDialog
          title={t("common.discardTitle")}
          message={t("common.discardMessage")}
          confirmText={t("common.discardConfirm")}
          cancelText={t("common.keepEditing")}
          onConfirm={guard.confirmDiscard}
          onCancel={guard.cancelDiscard}
        />
      ) : null}
    </Modal>
  );
}

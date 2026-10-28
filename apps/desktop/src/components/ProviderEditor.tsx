import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft,
  BadgeCheck,
  Boxes,
  Download,
  ExternalLink,
  Eye,
  EyeOff,
  FileJson,
  SlidersHorizontal,
  Sparkles,
  Trash2,
  TriangleAlert,
  Wand2,
  Zap,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { parse as parseToml, stringify as stringifyToml } from "smol-toml";
import { CLIENT_META, hasOfficial, isAdditiveApp, isRoutingApp } from "../lib/clients";
import i18n from "../lib/i18n";
import {
  type ClaudeApiFormat,
  type ClaudeAuthField,
  type ClientApp,
  type CodexWireApi,
  type DesktopMode,
  type DesktopRoute,
  type EndpointLatency,
  ipc,
  type ProviderProfile,
  type SwitchOutcome,
  type UpstreamApi,
} from "../lib/ipc";
import {
  CODEX_AUTO_COMPACT_LIMIT_1M,
  DESKTOP_ROLE_ROUTE_IDS,
  type DesktopRoleId,
  PRESET_CATEGORY_ORDER,
  presetCategory,
  type RelayPreset,
  relayPresetsFor,
} from "../lib/providerPresets";
import { queryKeys } from "../lib/queryKeys";
import { useUnsavedGuard } from "../lib/useUnsavedGuard";
import { cn, errorText } from "../lib/utils";
import { ClientGlyph } from "./BrandIcon";
import { ConfirmDialog } from "./ConfirmDialog";
import { JsonEditor } from "./JsonEditor";
import { hasBuiltinIcon, inferProviderIcon, ProviderIcon } from "./ProviderIcon";
import { Checkbox } from "./shadcn/checkbox";
import { Input } from "./shadcn/input";
import { Label } from "./shadcn/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./shadcn/select";
import {
  Button,
  Card,
  CollapseToggle,
  FormField,
  FormSection,
  IconButton,
  Modal,
  ModelCombobox,
  RecommendedBadge,
  SearchInput,
  SettingsAccordion,
  SettingsAccordionItem,
  SheetHero,
  Switch,
  Tag,
  Textarea,
} from "./ui";

/** Claude Desktop 四档 UI 行序（对齐 cc-switch ROLE_ORDER：Sonnet/Opus/Fable/Haiku）。 */
const DESKTOP_ROLE_ORDER: DesktopRoleId[] = ["sonnet", "opus", "fable", "haiku"];

interface DesktopRouteRow {
  model: string;
  label: string;
  supports1m: boolean;
}

const emptyDesktopRow = (): DesktopRouteRow => ({ model: "", label: "", supports1m: false });

/** 存储路由 → 四档行（按 routeId 反查角色；未知 routeId 丢弃——UI 只编辑标准四档）。 */
function desktopRowsFromRoutes(routes: DesktopRoute[]): Record<DesktopRoleId, DesktopRouteRow> {
  const rows: Record<DesktopRoleId, DesktopRouteRow> = {
    sonnet: emptyDesktopRow(),
    opus: emptyDesktopRow(),
    fable: emptyDesktopRow(),
    haiku: emptyDesktopRow(),
  };
  for (const route of routes) {
    const role = DESKTOP_ROLE_ORDER.find((r) => DESKTOP_ROLE_ROUTE_IDS[r] === route.routeId);
    if (role) {
      rows[role] = {
        model: route.upstreamModel,
        label: route.labelOverride ?? "",
        supports1m: route.supports1m,
      };
    }
  }
  return rows;
}

/** TOML 无 null 语义：序列化前深移除 null/undefined 叶子（兼容 JSON 时代的历史片段）。 */
function withoutNullLeaves(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.filter((item) => item != null).map(withoutNullLeaves);
  }
  if (typeof value === "object" && value !== null) {
    return Object.fromEntries(
      Object.entries(value)
        .filter(([, v]) => v != null)
        .map(([k, v]) => [k, withoutNullLeaves(v)]),
    );
  }
  return value;
}

/**
 * 附加配置片段 <-> 编辑器文本（空片段显示为空串，保存回 null）。
 * 编辑器文本格式与目标文件一致：Claude=settings.json 的 JSON，
 * Codex=config.toml 的 TOML（对齐 cc-switch：Codex 编辑器直接呈现 TOML）。
 */
export function formatFragment(
  app: ClientApp,
  fragment: Record<string, unknown> | null | undefined,
): string {
  if (!fragment || Object.keys(fragment).length === 0) return "";
  if (app !== "codex") return JSON.stringify(fragment, null, 2);
  const cleaned = withoutNullLeaves(fragment) as Record<string, unknown>;
  if (Object.keys(cleaned).length === 0) return "";
  return `${stringifyToml(cleaned).trim()}\n`;
}

/** 解析编辑器文本；空 → null；非法/非对象 → 抛错（由调用方提示）。 */
export function parseFragment(app: ClientApp, text: string): Record<string, unknown> | null {
  const trimmed = text.trim();
  if (!trimmed) return null;
  const parsed: unknown = app === "codex" ? parseToml(trimmed) : JSON.parse(trimmed);
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    // 模块级函数拿不到 hook：直接用 i18n 实例（错误文案会渲染进编辑器提示）。
    throw new Error(
      i18n.t(
        app === "codex" ? "providers.modal.tomlMustBeTable" : "providers.modal.jsonMustBeObject",
      ),
    );
  }
  return parsed as Record<string, unknown>;
}

type JsonObject = Record<string, unknown>;

function isPlainObject(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function deepEqualJson(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * 深遍历禁键：`obj[key] =` 对 `__proto__` 走 setter 会改写副本原型而非落键，
 * 用户片段里的这些键会静默丢失（对齐 cc-switch 的 FORBIDDEN_MERGE_KEYS 硬化）。
 */
const FORBIDDEN_MERGE_KEYS = new Set(["__proto__", "constructor", "prototype"]);

/** overlay 深合并进 base 的副本（overlay 覆盖同名标量）。 */
export function deepMergeJson(base: JsonObject, overlay: JsonObject): JsonObject {
  const result: JsonObject = { ...base };
  for (const [key, value] of Object.entries(overlay)) {
    if (FORBIDDEN_MERGE_KEYS.has(key)) continue;
    const existing = result[key];
    result[key] =
      isPlainObject(existing) && isPlainObject(value) ? deepMergeJson(existing, value) : value;
  }
  return result;
}

/** 从 base 副本中按值匹配深移除 fragment 的叶子（空父级修剪）。 */
export function deepRemoveMatching(base: JsonObject, fragment: JsonObject): JsonObject {
  const result: JsonObject = { ...base };
  for (const [key, value] of Object.entries(fragment)) {
    if (FORBIDDEN_MERGE_KEYS.has(key)) continue;
    const existing = result[key];
    if (isPlainObject(existing) && isPlainObject(value)) {
      const child = deepRemoveMatching(existing, value);
      if (Object.keys(child).length === 0) {
        delete result[key];
      } else {
        result[key] = child;
      }
    } else if (deepEqualJson(existing, value)) {
      delete result[key];
    }
  }
  return result;
}

/**
 * 编辑器内容相对 live 基线的"新增/修改"差集——只有这些键成为受管片段。
 * 相对基线的删除不生效（本机制只做叠加覆盖，不删用户自有配置）。
 */
export function diffFragment(edited: JsonObject, baseline: JsonObject): JsonObject | null {
  const result: JsonObject = {};
  for (const [key, value] of Object.entries(edited)) {
    if (FORBIDDEN_MERGE_KEYS.has(key)) continue;
    const base = baseline[key];
    if (isPlainObject(value) && isPlainObject(base)) {
      const child = diffFragment(value, base);
      if (child) result[key] = child;
    } else if (!deepEqualJson(value, base)) {
      result[key] = value;
    }
  }
  return Object.keys(result).length > 0 ? result : null;
}

/** query 参数 <-> "key=value" 多行文本。 */
function formatKeyValues(map: Record<string, string>): string {
  return Object.entries(map)
    .map(([key, value]) => `${key}=${value}`)
    .join("\n");
}

function parseKeyValues(text: string): Record<string, string> {
  const result: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const trimmed = line.trim();
    if (!trimmed) continue;
    const eq = trimmed.indexOf("=");
    if (eq <= 0) continue;
    result[trimmed.slice(0, eq).trim()] = trimmed.slice(eq + 1).trim();
  }
  return result;
}

/**
 * 快捷开关定义（对齐 cc-switch CommonConfigEditor 的 6 个 toggle）：
 * 每个开关声明片段中的路径、开启值与判定函数，直接读写编辑器里的 JSON 文本。
 */
interface QuickToggle {
  key: string;
  labelKey: string;
  isOn: (fragment: Record<string, unknown>) => boolean;
  turnOn: (fragment: Record<string, unknown>) => void;
  turnOff: (fragment: Record<string, unknown>) => void;
}

function envOf(fragment: Record<string, unknown>): Record<string, unknown> {
  const env = fragment.env;
  return typeof env === "object" && env !== null && !Array.isArray(env)
    ? (env as Record<string, unknown>)
    : {};
}

function envToggle(labelKey: string, key: string, envKey: string, onValue: string): QuickToggle {
  return {
    key,
    labelKey,
    isOn: (fragment) => {
      const value = envOf(fragment)[envKey];
      return value === onValue || value === Number(onValue);
    },
    turnOn: (fragment) => {
      fragment.env = { ...envOf(fragment), [envKey]: onValue };
    },
    turnOff: (fragment) => {
      const env = { ...envOf(fragment) };
      delete env[envKey];
      if (Object.keys(env).length === 0) {
        delete fragment.env;
      } else {
        fragment.env = env;
      }
    },
  };
}

const QUICK_TOGGLES: QuickToggle[] = [
  {
    key: "hideAttribution",
    labelKey: "providers.modal.toggle.hideAttribution",
    isOn: (fragment) => {
      const attribution = fragment.attribution as Record<string, unknown> | undefined;
      return attribution?.commit === "" && attribution?.pr === "";
    },
    turnOn: (fragment) => {
      fragment.attribution = { commit: "", pr: "" };
    },
    turnOff: (fragment) => {
      delete fragment.attribution;
    },
  },
  envToggle(
    "providers.modal.toggle.teammates",
    "teammates",
    "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
    "1",
  ),
  envToggle("providers.modal.toggle.toolSearch", "toolSearch", "ENABLE_TOOL_SEARCH", "true"),
  envToggle("providers.modal.toggle.effortMax", "effortMax", "CLAUDE_CODE_EFFORT_LEVEL", "max"),
  envToggle(
    "providers.modal.toggle.disableAutoUpgrade",
    "disableAutoUpgrade",
    "DISABLE_AUTOUPDATER",
    "1",
  ),
  // DeepSeek 等严格校验工具 schema 的网关会对 Artifact 工具整请求 400，
  // 关掉它是唯一绕法（变量名在 Claude Code 2.1.278 二进制里核实过）。
  envToggle(
    "providers.modal.toggle.disableArtifact",
    "disableArtifact",
    "CLAUDE_CODE_DISABLE_ARTIFACT",
    "1",
  ),
];

/**
 * 添加/编辑供应商——**页内面板**，不是弹窗。
 *
 * 弹窗对这件事是错的容器：预设有 40+ 个，装进对话框就得在一个小窗里滚动找，
 * 而选完预设后还要填表、测速、改 JSON 片段——一整套工作流，不是一次确认。
 * 面板铺开后左右分工：左列选预设（竖排可扫、带搜索、选中项一直在视野里），
 * 右侧填表，顶部一条返回栏交代"从哪来、在编辑谁"。
 *
 * - 预设列（仅新建）：官方订阅置顶（选中即可一键启用，无需任何参数）→ 推荐 → 分类折叠；
 * - 基础信息：名称/备注两列 + 官网地址；
 * - API Key：明/密切换 + 「获取 API Key」外链（预设或官网地址）；
 * - Base URL；高级选项（折叠）：认证字段（AUTH_TOKEN/API_KEY）+ 模型映射
 *  （兜底 + Haiku/Sonnet/Opus 角色，一键设置），Codex 侧仅兜底模型；
 * - Key 明文只经 providerSetApiKey 进系统 Keychain，不进任何本地状态存储。
 */
export function ProviderEditor({
  app,
  profile,
  nextSortOrder = 0,
  isCurrent = false,
  onClose,
}: {
  app: ClientApp;
  /** 传入表示编辑；缺省表示新建。 */
  profile?: ProviderProfile;
  /** 新建时的排序号（追加到列表末尾）。 */
  nextSortOrder?: number;
  /** 正在编辑的 Profile 是否为当前生效项：是则保存后需重新 switch 才能写回 live 配置。 */
  isCurrent?: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 改过内容再点「返回」/ ESC 先弹确认：这页字段多，误触返回整张表白填。
  const guard = useUnsavedGuard(onClose);
  const [name, setName] = useState(profile?.name ?? "");
  const [notes, setNotes] = useState(profile?.notes ?? "");
  const [websiteUrl, setWebsiteUrl] = useState(profile?.websiteUrl ?? "");
  const [baseUrl, setBaseUrl] = useState(profile?.baseUrl ?? "");
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [apiKeyField, setApiKeyField] = useState<ClaudeAuthField>(
    profile?.apiKeyField ?? "auth-token",
  );
  const [wireApi, setWireApi] = useState<CodexWireApi>(profile?.codexWireApi ?? "responses");
  const [apiFormat, setApiFormat] = useState<ClaudeApiFormat>(
    profile?.claudeApiFormat ?? "anthropic",
  );
  const [desktopMode, setDesktopMode] = useState<DesktopMode>(profile?.desktopMode ?? "direct");
  const [desktopRows, setDesktopRows] = useState<Record<DesktopRoleId, DesktopRouteRow>>(() =>
    desktopRowsFromRoutes(profile?.desktopRoutes ?? []),
  );
  const [desktopDirectListOpen, setDesktopDirectListOpen] = useState(false);
  const [queryParamsText, setQueryParamsText] = useState(
    formatKeyValues(profile?.codexQueryParams ?? {}),
  );
  const [contextWindow1M, setContextWindow1M] = useState(profile?.codexContextWindow1m ?? false);
  const [autoCompactLimit, setAutoCompactLimit] = useState(
    profile?.codexAutoCompactLimit
      ? String(profile.codexAutoCompactLimit)
      : profile?.codexContextWindow1m
        ? String(CODEX_AUTO_COMPACT_LIMIT_1M)
        : "",
  );
  // 显式图标 key（对齐 cc-switch meta.icon）：选预设时携带、随保存落库；
  // null = 按名称自动推断。无独立选择 UI。
  const [iconKey, setIconKey] = useState<string | null>(profile?.icon ?? null);
  const [model, setModel] = useState(profile?.model ?? "");
  const [modelHaiku, setModelHaiku] = useState(profile?.modelHaiku ?? "");
  const [modelSonnet, setModelSonnet] = useState(profile?.modelSonnet ?? "");
  const [modelOpus, setModelOpus] = useState(profile?.modelOpus ?? "");
  const [modelFable, setModelFable] = useState(profile?.modelFable ?? "");
  const [modelSubagent, setModelSubagent] = useState(profile?.modelSubagent ?? "");
  // 「拉取模型」结果：null=未拉取，[]=拉到空。选一项写入回退模型字段。
  const [fetchedModels, setFetchedModels] = useState<string[] | null>(null);
  // 端点候选（测速/一键切换，对齐 cc-switch endpointCandidates）。
  const [endpoints, setEndpoints] = useState<string[]>(profile?.endpointCandidates ?? []);
  const [endpointInput, setEndpointInput] = useState("");
  // additive 客户端的模型列表（OpenCode / OpenClaw 只认显式声明过的模型，
  // 而且要的是一组）。老数据没有 models 字段时用单值 model 兜底。
  const [models, setModels] = useState<string[]>(
    profile?.models?.length ? profile.models : profile?.model ? [profile.model] : [],
  );
  const [modelInput, setModelInput] = useState("");
  const [upstreamApi, setUpstreamApi] = useState<UpstreamApi>(profile?.upstreamApi ?? "openai");
  const [headersText, setHeadersText] = useState(formatKeyValues(profile?.headers ?? {}));
  const [routeBodyOverrideText, setRouteBodyOverrideText] = useState(
    profile?.routeBodyOverride ? JSON.stringify(profile.routeBodyOverride, null, 2) : "",
  );
  const [contextLength, setContextLength] = useState(
    profile?.contextLength ? String(profile.contextLength) : "",
  );
  const [latency, setLatency] = useState<Record<string, EndpointLatency>>({});
  const [testingEndpoints, setTestingEndpoints] = useState(false);
  // 配置片段（Claude=settings.json / Codex=config.toml）+ 通用配置开关（对齐 cc-switch）。
  // 编辑器展示 live 配置全貌（基线，去除受管键），保存时只把相对基线的
  // 新增/修改存为受管片段——观感对齐 cc-switch 的整份配置编辑，语义仍是最小 Patch。
  const [extraJson, setExtraJson] = useState(formatFragment(app, profile?.extraSettings));
  const [liveBaseline, setLiveBaseline] = useState<JsonObject | null>(null);
  // 基线异步加载完成前用户已选预设/输入过 → 不覆写编辑器内容。
  const editorDirty = useRef(false);
  const [extraJsonError, setExtraJsonError] = useState<string | null>(null);
  const [commonEnabled, setCommonEnabled] = useState(profile?.commonConfigEnabled ?? false);
  const [editingCommon, setEditingCommon] = useState(false);
  // 折叠段的初始开合：编辑既有供应商时，配过东西的段直接展开（否则用户要
  // 逐个点开才能确认自己配了什么）；新建时一律收起，只留必填项在接入卡里。
  const hasAdvancedValue = Boolean(
    (profile?.apiKeyField ?? "auth-token") !== "auth-token" ||
      (profile?.codexWireApi ?? "responses") !== "responses" ||
      (profile?.claudeApiFormat ?? "anthropic") !== "anthropic" ||
      Object.keys(profile?.codexQueryParams ?? {}).length > 0 ||
      Object.keys(profile?.headers ?? {}).length > 0 ||
      Boolean(profile?.routeBodyOverride) ||
      profile?.codexContextWindow1m,
  );
  // 折叠段用一份"展开中的段 id"，而不是每段一个 useState——段是同一类东西，
  // 状态也该是同一份（Accordion 的受控接口正好也是这个形状）。
  const [openSections, setOpenSections] = useState<string[]>(() => {
    const open: string[] = [];
    if ((profile?.endpointCandidates?.length ?? 0) > 0) open.push("endpoints");
    // additive 客户端的模型列表是必填、Desktop 的路由段带着模式开关，
    // 收起来等于把必答题藏了——这两类默认展开。
    if (
      isAdditiveApp(app) ||
      app === "claude-desktop" ||
      profile?.model ||
      profile?.modelHaiku ||
      profile?.modelSonnet ||
      profile?.modelOpus ||
      profile?.modelFable ||
      profile?.modelSubagent
    ) {
      open.push("models");
    }
    if (hasAdvancedValue) open.push("advanced");
    return open;
  });
  const openSection = (id: string) =>
    setOpenSections((prev) => (prev.includes(id) ? prev : [...prev, id]));
  const [error, setError] = useState<string | null>(null);
  // 新建模式下记录已创建的 Profile：若"创建成功但存 Key 失败"，
  // 重试走更新路径，避免重复创建。
  const [created, setCreated] = useState<ProviderProfile | null>(null);

  // 打开弹窗时读取 live 配置作为编辑器基线；Tauri 不可用（测试/浏览器）时
  // 保持"仅片段"的兜底行为（liveBaseline 为 null）。
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const live = await ipc.providerLiveSettings(app);
        const snippet = profile?.commonConfigEnabled
          ? ((await ipc.providerCommonSnippet(app)) ?? {})
          : {};
        if (cancelled) return;
        const oldFragment = profile?.extraSettings ?? {};
        // 基线 = live 去掉本供应商当前生效的片段（含通用配置），
        // 编辑器 = 基线 ⊕ 自身片段（通用配置不并入，单独管理）。
        const effectiveOld = deepMergeJson({ ...oldFragment }, snippet as JsonObject);
        const baseline = deepRemoveMatching(live, effectiveOld);
        setLiveBaseline(baseline);
        if (!editorDirty.current) {
          const merged = deepMergeJson(baseline, oldFragment as JsonObject);
          setExtraJson(formatFragment(app, merged));
          setExtraJsonError(null);
        }
      } catch {
        // 保持初始（仅片段）内容。
      }
    })();
    return () => {
      cancelled = true;
    };
    // 弹窗每次打开都是全新挂载，app/profile 在其生命周期内引用稳定，
    // 该 effect 实际只执行一次。
  }, [app, profile]);

  // ESC 走脏检测守卫（与导览 / 首启向导同惯例：随时能退，但别把改动无声丢掉）。
  // 有弹层开着时 ESC 归那一层——放弃确认框、通用配置弹窗等自己会处理。
  const { requestClose } = guard;
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (document.querySelector('[role="dialog"][aria-modal="true"]')) return;
      requestClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [requestClose]);

  const presets = profile ? [] : relayPresetsFor(app);
  const isAdding = !profile;
  /** 编辑官方订阅：只开放配置 JSON 片段与通用配置（对齐 cc-switch 官方可编辑）。 */
  const isOfficialEdit = profile?.category === "official";
  /** 支持本地路由的 app 的中转站：请求头与请求体覆盖在路由接管时对它生效。 */
  const isRoutingRelay = isRoutingApp(app) && !isOfficialEdit;
  const [presetId, setPresetId] = useState<string>("custom");
  const preset = presets.find((p) => p.id === presetId) ?? null;
  // additive 客户端（OpenCode / OpenClaw）自己不提供模型，没有官方订阅可选——
  // 后端 ensure_official 会直接拒绝，预设列里就不该出现这一项。
  const appHasOfficial = hasOfficial(app);
  const isOfficialSelected = isAdding && appHasOfficial && presetId === "official";
  // 预设搜索：40+ 个预设里找一家的最快路径就是打名字，搜索态下拍平分组直接列结果。
  const [presetSearch, setPresetSearch] = useState("");
  const presetQuery = presetSearch.trim().toLowerCase();
  const matchedPresets = presets.filter((p) =>
    [p.name, p.baseUrl].some((field) => field?.toLowerCase().includes(presetQuery)),
  );

  // 拉取供应商可用模型：新增时用表单 Key，编辑时后端按 id 从 Keychain 物化。
  const fetchModels = useMutation({
    mutationFn: () =>
      ipc.providerFetchModels({
        baseUrl: baseUrl.trim(),
        apiKey: apiKey.trim() || undefined,
        id: profile?.id,
      }),
    onSuccess: (models) => setFetchedModels(models),
  });
  // 有可用 Key 来源（表单已输入，或编辑既有项走 Keychain）且有地址才能拉取。
  const canFetchModels = Boolean(baseUrl.trim()) && (Boolean(apiKey.trim()) || Boolean(profile));

  const applyPreset = (next: RelayPreset | null) => {
    setPresetId(next?.id ?? "custom");
    setName(next?.name ?? "");
    setBaseUrl(next?.baseUrl ?? "");
    setWebsiteUrl(next?.websiteUrl ?? "");
    setModel(next?.model ?? "");
    setModelHaiku(next?.modelHaiku ?? "");
    setModelSonnet(next?.modelSonnet ?? "");
    setModelOpus(next?.modelOpus ?? "");
    setModelFable(next?.modelFable ?? "");
    // 子代理槽没有预设值：留空 = 走正常别名解析（四档），填了才全局覆盖。
    setModelSubagent("");
    setApiKeyField(next?.apiKeyField ?? "auth-token");
    setWireApi(next?.wireApi ?? "responses");
    setApiFormat("anthropic");
    setDesktopMode(next?.desktopMode ?? "direct");
    setDesktopRows(desktopRowsFromRoutes(next?.desktopRoutes ?? []));
    setIconKey(next?.icon ?? null);
    editorDirty.current = true;
    {
      const merged = deepMergeJson(liveBaseline ?? {}, (next?.extraSettings ?? {}) as JsonObject);
      setExtraJson(formatFragment(app, merged));
    }
    setExtraJsonError(null);
    setEndpoints(next?.endpointCandidates ?? []);
    // additive 预设声明的是**整组**模型（客户端只显示声明过的），缺省才回落
    // 到单值 model。
    setModels(next?.models ?? (next?.model ? [next.model] : []));
    setModelInput("");
    // 共享预设一律不带协议（见 providerPresets.ts 的守卫说明），恒回落 openai。
    setUpstreamApi(next?.upstreamApi ?? "openai");
    setHeadersText("");
    setRouteBodyOverrideText("");
    setContextLength(next?.contextLength ? String(next.contextLength) : "");
    setLatency({});
    setFetchedModels(null);
    fetchModels.reset();
    setQueryParamsText(formatKeyValues(next?.queryParams ?? {}));
    setContextWindow1M(Boolean(next?.contextWindow1M));
    setAutoCompactLimit(next?.contextWindow1M ? String(CODEX_AUTO_COMPACT_LIMIT_1M) : "");
    // 预设带了非默认的认证形态时展开「高级」，让用户看见这一处不寻常。
    // 模型映射不在此列——它有自己的段，摘要行已经把模型名摆出来了。
    if (
      next &&
      (next.apiKeyField === "api-key" ||
        next.wireApi === "chat" ||
        next.queryParams ||
        next.contextWindow1M)
    ) {
      openSection("advanced");
    }
  };

  const renderPresetTile = (p: RelayPreset) => {
    const selected = presetId === p.id;
    return (
      <PresetTile
        key={p.id}
        selected={selected}
        brandName
        label={p.name}
        // 选中格是实心主色底，彩色品牌 SVG 会与之糊成一片——plate 给它一块白衬底。
        icon={<ProviderIcon name={p.name} icon={p.icon} size={16} plate={selected} />}
        badge={
          p.recommended ? (
            <Sparkles
              size={12}
              strokeWidth={2.5}
              aria-label={t("providers.modal.presetRecommended")}
              className={cn("shrink-0", selected ? "text-amber-200" : "text-amber-500")}
            />
          ) : undefined
        }
        onClick={() => applyPreset(p)}
      />
    );
  };

  const addModel = () => {
    const id = modelInput.trim();
    if (!id || models.includes(id)) return;
    setModels((list) => [...list, id]);
    setModelInput("");
  };

  /**
   * additive 客户端的模型列表控件。
   *
   * 之前这里是借 Claude 的角色映射字段（Sonnet/Opus/Haiku 三个格子）凑一组模型——
   * 标签在这个上下文里毫无意义，用户根本猜不到该往哪填。改成一个平铺的列表：
   * 它就是"要声明给客户端的模型"，没有角色概念。
   */
  const additiveModelsSection = isAdditiveApp(app) ? (
    <div className="divide-y divide-line">
      {CLIENT_META[app].supportsUpstreamApi ? (
        <FormField
          label={t("providers.modal.upstreamApi")}
          hint={t("providers.modal.upstreamApiHint")}
        >
          <Select
            value={upstreamApi}
            onValueChange={(value) => setUpstreamApi(value as UpstreamApi)}
          >
            <SelectTrigger id="provider-upstream-api" className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="openai">{t("providers.modal.upstreamApiOpenai")}</SelectItem>
              <SelectItem value="anthropic">{t("providers.modal.upstreamApiAnthropic")}</SelectItem>
            </SelectContent>
          </Select>
        </FormField>
      ) : null}
      <FormField
        label={t("providers.modal.models")}
        required
        hint={t("providers.modal.modelsHint")}
      >
        <div className="space-y-1.5">
          {models.map((id) => (
            <div key={id} className="flex items-center gap-2">
              <span className="flex-1 truncate rounded-lg border border-line bg-app px-3 py-2 font-mono text-[13px] text-ink">
                {id}
              </span>
              <IconButton
                icon={Trash2}
                tone="danger"
                title={t("providers.modal.modelRemove", { model: id })}
                aria-label={t("providers.modal.modelRemove", { model: id })}
                onClick={() => setModels((list) => list.filter((m) => m !== id))}
              />
            </div>
          ))}
          <div className="flex gap-2">
            <Input
              className="flex-1 font-mono"
              placeholder={t("providers.modal.modelAddPlaceholder")}
              value={modelInput}
              onChange={(e) => setModelInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  addModel();
                }
              }}
            />
            <Button size="sm" disabled={!modelInput.trim()} onClick={addModel}>
              {t("providers.modal.modelAdd")}
            </Button>
          </div>
          {models.length === 0 ? (
            <p className="text-[11px] text-warn">{t("providers.modal.modelRequiredAdditive")}</p>
          ) : null}
        </div>
      </FormField>
      {CLIENT_META[app].supportsContextLength ? (
        <FormField
          label={t("providers.modal.contextLength")}
          hint={t("providers.modal.contextLengthHint")}
        >
          <Input
            className="w-full max-w-[200px] font-mono"
            inputMode="numeric"
            placeholder={t("providers.modal.contextLengthPlaceholder")}
            value={contextLength}
            onChange={(e) => setContextLength(e.target.value.replace(/\D/g, ""))}
          />
        </FormField>
      ) : null}
    </div>
  ) : null;

  // 请求体覆盖（JSON Merge Patch）：空文本 = 不覆盖；非对象/非法 JSON 在这一层拦住，
  // 不要等后端拒收才发现。仅路由 app 的中转站这个字段才生效，其余 app 保持原值不动。
  const routeBodyOverrideParsed = useMemo(() => {
    const text = routeBodyOverrideText.trim();
    if (!text)
      return { value: null as Record<string, unknown> | null, error: null as string | null };
    try {
      const parsed = JSON.parse(text);
      if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
        return { value: null, error: t("providers.modal.routeBodyOverrideInvalid") };
      }
      return { value: parsed as Record<string, unknown>, error: null };
    } catch (e) {
      return { value: null, error: errorText(e) };
    }
  }, [routeBodyOverrideText, t]);
  const routeBodyOverrideError = isRoutingRelay ? routeBodyOverrideParsed.error : null;

  /**
   * 自定义请求头：additive 客户端始终生效；支持路由的 app（claude/codex/gemini/grok）
   * 的中转站也显示——仅在本地路由接管时才作为上游请求头发送，提示文案要说清楚这一点。
   */
  const headersSection =
    isAdditiveApp(app) || isRoutingRelay ? (
      <FormField
        id="provider-headers"
        label={t("providers.modal.headers")}
        hint={
          isRoutingRelay
            ? t("providers.modal.headersHintRouting")
            : t("providers.modal.headersHint")
        }
      >
        <Textarea
          id="provider-headers"
          className="h-20 font-mono"
          placeholder={t("providers.modal.headersPlaceholder")}
          value={headersText}
          onChange={(e) => setHeadersText(e.target.value)}
        />
      </FormField>
    ) : null;

  /** 请求体覆盖（JSON Merge Patch）：仅支持路由的 app 的中转站，本地路由接管时对上游请求体生效。 */
  const routeBodyOverrideSection = isRoutingRelay ? (
    <FormField
      id="provider-route-body-override"
      label={t("providers.modal.routeBodyOverride")}
      hint={t("providers.modal.routeBodyOverrideHint")}
      error={routeBodyOverrideError ?? undefined}
    >
      <Textarea
        id="provider-route-body-override"
        className="h-24 font-mono"
        placeholder={'{ "enable_thinking": false }'}
        value={routeBodyOverrideText}
        onChange={(e) => setRouteBodyOverrideText(e.target.value)}
      />
    </FormField>
  ) : null;

  const save = useMutation({
    mutationFn: async () => {
      const editedFragment = (() => {
        const parsed = parseFragment(app, extraJson);
        if (!liveBaseline || parsed === null) return parsed;
        return diffFragment(parsed as JsonObject, liveBaseline) as Record<string, unknown> | null;
      })();
      if (isOfficialEdit) {
        await ipc.providerOfficialUpdate(app, editedFragment, commonEnabled);
        // 官方订阅本就是当前生效项时，覆盖项只落 store 不会自动写回
        // live 配置（写回只在 switch 时发生），这里补一次 switch 让编辑立即生效。
        if (isCurrent) {
          await ipc.providerSwitch(app, CLIENT_META[app].officialId);
        }
        return;
      }
      const trimmed = (value: string) => value.trim() || null;
      // Claude Desktop 路由落库（对齐 cc-switch 表单语义）：
      // - direct：不做行编辑，预设/既有路由原样保存（自定义为空表 = Desktop 自读模型）；
      // - proxy：四档齐全落库——留空档回填第一个已填档（行序 Sonnet 优先），
      //   菜单显示名缺省用模型名，1M 声明随 primary（显式勾选优先）。
      const desktopRoutesForSave = (): DesktopRoute[] => {
        if (app !== "claude-desktop") return profile?.desktopRoutes ?? [];
        if (desktopMode === "direct") {
          // direct 透传：启用的档（model 非空）写 inferenceModels + 1M 声明；
          // 全空 = 不写（Desktop 自读上游 /v1/models）。
          return DESKTOP_ROLE_ORDER.filter((role) => desktopRows[role].model.trim()).map(
            (role) => ({
              routeId: DESKTOP_ROLE_ROUTE_IDS[role],
              upstreamModel: DESKTOP_ROLE_ROUTE_IDS[role],
              supports1m: desktopRows[role].supports1m,
            }),
          );
        }
        const filledRoles = DESKTOP_ROLE_ORDER.filter((role) => desktopRows[role].model.trim());
        const primaryRole = filledRoles[0];
        if (!primaryRole) return [];
        const primary = desktopRows[primaryRole];
        const primaryLabel = primary.label.trim() || primary.model.trim();
        return DESKTOP_ROLE_ORDER.map((role) => {
          const row = desktopRows[role];
          const hasOwn = Boolean(row.model.trim());
          const upstreamModel = hasOwn ? row.model.trim() : primary.model.trim();
          const labelOverride =
            (hasOwn ? row.label.trim() : row.label.trim() || primaryLabel) || upstreamModel;
          return {
            routeId: DESKTOP_ROLE_ROUTE_IDS[role],
            upstreamModel,
            labelOverride,
            supports1m: hasOwn ? row.supports1m : row.supports1m || primary.supports1m,
          };
        });
      };
      const fields = {
        name,
        baseUrl,
        model: trimmed(model),
        modelHaiku: trimmed(modelHaiku),
        modelSonnet: trimmed(modelSonnet),
        modelOpus: trimmed(modelOpus),
        modelFable: trimmed(modelFable),
        modelSubagent: trimmed(modelSubagent),
        apiKeyField,
        codexWireApi: wireApi,
        // Desktop 直连模式不经路由，Chat 协议无从转换，按 Anthropic 保存。
        claudeApiFormat:
          app === "claude-desktop" && desktopMode === "direct" ? "anthropic" : apiFormat,
        desktopMode,
        desktopRoutes: desktopRoutesForSave(),
        codexQueryParams: parseKeyValues(queryParamsText),
        extraSettings: editedFragment,
        commonConfigEnabled: commonEnabled,
        endpointCandidates: endpoints,
        models,
        upstreamApi: CLIENT_META[app].supportsUpstreamApi ? upstreamApi : null,
        headers: isAdditiveApp(app) || isRoutingRelay ? parseKeyValues(headersText) : {},
        // 非路由中转站保留原值不动；解析失败时 valid 已挡住保存，这里兜底 null。
        routeBodyOverride: isRoutingRelay
          ? routeBodyOverrideParsed.value
          : (profile?.routeBodyOverride ?? null),
        // 只有真支持这一维度的客户端才收下；解析不出正整数就当没填，不猜。
        contextLength: CLIENT_META[app].supportsContextLength
          ? Number.parseInt(contextLength, 10) || null
          : null,
        icon: iconKey,
        notes: trimmed(notes),
        websiteUrl: trimmed(websiteUrl),
        codexContextWindow1m: app === "codex" && contextWindow1M,
        codexAutoCompactLimit:
          app === "codex" && contextWindow1M
            ? Number.parseInt(autoCompactLimit, 10) || CODEX_AUTO_COMPACT_LIMIT_1M
            : null,
      };
      const existing = profile ?? created;
      let target: ProviderProfile;
      if (existing) {
        target = { ...existing, ...fields };
        await ipc.providerUpdate(target);
      } else {
        target = await ipc.providerAdd({
          schemaVersion: 2,
          id: crypto.randomUUID().replaceAll("-", ""),
          app,
          category: "relay",
          ...fields,
          apiKeyRef: null,
          codexSlug: null,
          sortOrder: nextSortOrder,
        });
        setCreated(target);
      }
      if (apiKey.trim()) {
        await ipc.providerSetApiKey(target.id, apiKey.trim());
      }
      // 正在编辑的中转站本就是当前生效项时，同理需要补一次 switch
      // 才能把改动写回 live 配置（否则改动只落 store，不生效）。
      if (existing && isCurrent) {
        await ipc.providerSwitch(app, target.id);
      }
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.providers });
      void queryClient.invalidateQueries({ queryKey: queryKeys.providerCurrent });
      onClose();
    },
    onError: (e) => {
      setError(errorText(e));
      // 部分失败（如已创建但存 Key 失败）时也要刷新列表反映真实状态。
      void queryClient.invalidateQueries({ queryKey: queryKeys.providers });
    },
  });

  // 选中「官方订阅」时，主按钮 = 确保官方项在列表里（用户可能删过）+ 立即启用。
  // ensure 幂等，官方项还在时只是一次空操作。
  const enableOfficial = useMutation({
    mutationFn: async (): Promise<SwitchOutcome> => {
      await ipc.providerOfficialEnsure(app);
      return ipc.providerSwitch(app, CLIENT_META[app].officialId);
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.providers });
      void queryClient.invalidateQueries({ queryKey: queryKeys.providerCurrent });
      onClose();
    },
    onError: (e) => setError(errorText(e)),
  });

  // Desktop proxy 模式至少要有一个已填模型档（对齐 cc-switch routesRequired）。
  const desktopRoutesValid =
    app !== "claude-desktop" ||
    desktopMode === "direct" ||
    DESKTOP_ROLE_ORDER.some((role) => desktopRows[role].model.trim());
  // additive 客户端只认显式声明过的模型：模型空着就算存下来也启用不了
  //（后端会拒绝），所以在表单这一层就挡住。
  const additiveModelValid = !isAdditiveApp(app) || models.length > 0;
  const valid = isOfficialEdit
    ? !extraJsonError
    : name.trim().length > 0 &&
      baseUrl.trim().length > 0 &&
      !extraJsonError &&
      !routeBodyOverrideError &&
      desktopRoutesValid &&
      additiveModelValid;
  const apiKeyUrl = preset?.apiKeyUrl ?? preset?.websiteUrl ?? (websiteUrl.trim() || null);

  // 「还差什么」：保存按钮置灰却不说原因，用户只能挨个字段猜。把缺口写在
  // 接入卡底部——它就在必填项下面，是最容易被读到的位置。
  const blocker = (() => {
    if (isOfficialEdit) return extraJsonError ? t("providers.editor.blockConfig") : null;
    const missing = [
      !name.trim() && t("providers.modal.name"),
      !baseUrl.trim() && t("providers.modal.baseUrl"),
    ].filter(Boolean) as string[];
    if (missing.length > 0) {
      return t("providers.editor.blockMissing", {
        fields: missing.join(t("providers.editor.fieldJoin")),
      });
    }
    if (!additiveModelValid) return t("providers.modal.modelRequiredAdditive");
    if (!desktopRoutesValid) return t("providers.editor.blockRoutes");
    if (extraJsonError) return t("providers.editor.blockConfig");
    if (routeBodyOverrideError) return t("providers.editor.blockRouteBody");
    return null;
  })();

  // 快捷开关：解析编辑器 JSON → 翻转对应键 → 写回（JSON 非法时开关不动作，对齐 cc-switch）。
  // memo 按 extraJson 收敛：渲染期 IIFE 会让每个按键把全文 parse 一遍。
  const parsedFragment = useMemo(() => {
    try {
      return parseFragment(app, extraJson) ?? {};
    } catch {
      return null;
    }
  }, [app, extraJson]);
  const handleQuickToggle = (toggle: QuickToggle) => {
    if (!parsedFragment) return;
    const fragment = structuredClone(parsedFragment);
    if (toggle.isOn(fragment)) {
      toggle.turnOff(fragment);
    } else {
      toggle.turnOn(fragment);
    }
    editorDirty.current = true;
    setExtraJson(formatFragment(app, fragment));
    setExtraJsonError(null);
  };
  const handleExtraJsonChange = (text: string) => {
    editorDirty.current = true;
    setExtraJson(text);
    try {
      parseFragment(app, text);
      setExtraJsonError(null);
    } catch (e) {
      setExtraJsonError(errorText(e));
    }
  };

  // 端点测速：候选 + 当前 baseUrl 一起并发测（对齐 cc-switch EndpointSpeedTest）。
  const endpointList = Array.from(new Set([baseUrl.trim(), ...endpoints].filter(Boolean)));
  const runEndpointTest = async () => {
    setTestingEndpoints(true);
    try {
      const results = await ipc.providerTestEndpoints(endpointList);
      setLatency(Object.fromEntries(results.map((r) => [r.url, r])));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setTestingEndpoints(false);
    }
  };
  const addEndpoint = () => {
    const url = endpointInput.trim();
    if (!url || endpoints.includes(url)) return;
    setEndpoints((list) => [...list, url]);
    setEndpointInput("");
  };

  // 一键设置：把兜底模型（或第一个非空角色）应用到全部角色。
  const quickSetModels = () => {
    const value =
      model.trim() ||
      modelSonnet.trim() ||
      modelOpus.trim() ||
      modelHaiku.trim() ||
      modelFable.trim();
    if (!value) return;
    setModel(value);
    setModelHaiku(value);
    setModelSonnet(value);
    setModelOpus(value);
    setModelFable(value);
    // 子代理槽刻意不铺：它会覆盖每次调用与子代理定义里指定的模型，
    // 留空才是让 Claude Code 走正常别名解析（即上面四档）的默认行为。
  };

  const modelRoles: {
    key: string;
    label: string;
    value: string;
    set: (v: string) => void;
    hint?: string;
  }[] = [
    { key: "haiku", label: "Haiku", value: modelHaiku, set: setModelHaiku },
    { key: "sonnet", label: "Sonnet", value: modelSonnet, set: setModelSonnet },
    { key: "opus", label: "Opus", value: modelOpus, set: setModelOpus },
    { key: "fable", label: "Fable", value: modelFable, set: setModelFable },
    {
      key: "subagent",
      label: t("providers.modal.modelRoleSubagent"),
      value: modelSubagent,
      set: setModelSubagent,
      hint: t("providers.modal.modelRoleSubagentHint"),
    },
  ];

  /**
   * 折叠段右侧的状态摘要。折叠只有配上摘要才不是"藏起来"——收起态也要读得出
   * 这一段里有没有东西、有多少。
   */
  const noneSummary = t("providers.editor.summaryNone");
  const endpointsSummary =
    endpoints.length > 0
      ? t("providers.editor.summaryEndpoints", { count: endpoints.length })
      : noneSummary;
  const modelsSummary = isAdditiveApp(app)
    ? models.length > 0
      ? t("providers.editor.summaryModels", { count: models.length })
      : t("providers.editor.summaryRequired")
    : model.trim() ||
      modelSonnet.trim() ||
      modelOpus.trim() ||
      modelHaiku.trim() ||
      modelFable.trim() ||
      noneSummary;
  const advancedSummary = hasAdvancedValue
    ? t("providers.editor.summaryCustom")
    : t("providers.editor.summaryDefault");
  // 受管片段项数 = 编辑器内容相对 live 基线的差集顶层键数，与保存时落库的口径一致。
  const configSummary = (() => {
    if (extraJsonError) return t("providers.editor.summaryInvalid");
    const count = parsedFragment
      ? Object.keys(
          (liveBaseline ? diffFragment(parsedFragment, liveBaseline) : parsedFragment) ?? {},
        ).length
      : 0;
    return count > 0
      ? t("providers.editor.summaryConfigDirty", { count })
      : t("providers.editor.summaryConfigClean");
  })();

  // 配置编辑区（中转站与官方订阅共用；官方编辑模式下是唯一可编辑区）。
  // Claude 编辑 settings.json（JSON），Codex 编辑 config.toml（TOML 原生格式）。
  const configJsonSection = (
    <Card className="overflow-hidden">
      {/* 段头不再压一层灰底：白纸上靠图标与字重就分得出层级，加了灰底反而把
          每张卡都切成"灰头—白身"的三明治，几张卡摞起来整页就发灰。 */}
      <div className="flex items-center gap-2.5 px-7 pt-5 pb-3">
        <FileJson size={15} strokeWidth={2} aria-hidden className="shrink-0 text-faint" />
        <span className="font-semibold text-[13px] text-ink">
          {t(app !== "codex" ? "providers.modal.configJson" : "providers.modal.configToml")}
        </span>
        <span className="ml-auto truncate text-xs text-faint">{configSummary}</span>
        {/* 「编辑通用配置」开的是另一份全局片段，不属于本供应商——挂段头右侧，
            别混进下面那排作用于本片段的开关里。 */}
        <button
          type="button"
          onClick={() => setEditingCommon(true)}
          className="shrink-0 text-xs text-accent hover:underline"
        >
          {t("providers.modal.editCommonConfig")}
        </button>
      </div>
      <div className="px-7 pb-6">
        {/* 所有开关同为 checkbox，排成一行左对齐；之前「应用通用配置」单独右对齐
            占一行，与下面那排左对齐的快捷开关分成两行两个方向，读着像两组设置。 */}
        <div className="mb-2.5 flex flex-wrap gap-x-4 gap-y-1.5">
          <Label className="cursor-pointer gap-2 font-normal text-ink text-xs">
            <Checkbox
              checked={commonEnabled}
              onCheckedChange={(checked) => setCommonEnabled(checked === true)}
            />
            {t("providers.modal.applyCommonConfig")}
          </Label>
          {app === "claude"
            ? QUICK_TOGGLES.map((toggle) => (
                <Label
                  key={toggle.key}
                  className={cn(
                    "gap-2 font-normal text-xs",
                    parsedFragment ? "cursor-pointer text-ink" : "cursor-not-allowed text-faint",
                  )}
                >
                  <Checkbox
                    disabled={!parsedFragment}
                    checked={parsedFragment ? toggle.isOn(parsedFragment) : false}
                    onCheckedChange={() => handleQuickToggle(toggle)}
                  />
                  {t(toggle.labelKey)}
                </Label>
              ))
            : null}
        </div>
        <JsonEditor
          value={extraJson}
          onChange={handleExtraJsonChange}
          language={app !== "codex" ? "json" : "toml"}
          placeholder={t(
            app !== "codex"
              ? "providers.modal.configJsonPlaceholder"
              : "providers.modal.configTomlPlaceholder",
          )}
          hasError={Boolean(extraJsonError)}
        />
        {extraJsonError ? (
          <p className="mt-1 text-[11px] text-danger">
            {t(
              app !== "codex"
                ? "providers.modal.configJsonInvalid"
                : "providers.modal.configTomlInvalid",
              { error: extraJsonError },
            )}
          </p>
        ) : (
          <p className="mt-1 text-[11px] text-faint">
            {t(
              app === "codex"
                ? "providers.modal.configTomlHint"
                : app === "openclaw"
                  ? "providers.modal.configJson5Hint"
                  : "providers.modal.configJsonHint",
            )}
          </p>
        )}
      </div>
    </Card>
  );

  /** Claude 系上游协议（Chat 接口须经本地路由转换）。 */
  const apiFormatField = (
    <FormField label={t("providers.modal.apiFormat")} hint={t("providers.modal.apiFormatHint")}>
      <Select value={apiFormat} onValueChange={(value) => setApiFormat(value as ClaudeApiFormat)}>
        <SelectTrigger className="w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="anthropic">{t("providers.modal.apiFormatAnthropicOption")}</SelectItem>
          <SelectItem value="openai-chat">{t("providers.modal.apiFormatChatOption")}</SelectItem>
          <SelectItem value="openai-responses">
            {t("providers.modal.apiFormatResponsesOption")}
          </SelectItem>
        </SelectContent>
      </Select>
      {apiFormat !== "anthropic" ? (
        <p className="mt-1.5 text-xs text-warn">{t("providers.modal.apiFormatChatNote")}</p>
      ) : null}
    </FormField>
  );
  const authFieldSelect = (
    <FormField label={t("providers.modal.authField")} hint={t("providers.modal.authFieldHint")}>
      <Select
        value={apiKeyField}
        onValueChange={(value) => setApiKeyField(value as ClaudeAuthField)}
      >
        <SelectTrigger className="w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="auth-token">
            {t("providers.modal.authFieldAuthTokenOption")}
          </SelectItem>
          <SelectItem value="api-key">ANTHROPIC_API_KEY</SelectItem>
        </SelectContent>
      </Select>
    </FormField>
  );

  /** 认证形态（Claude 的认证字段 / Codex 的 wire_api 与 query 参数）：一次配好就不再动，归「高级」。 */
  const authSection =
    app === "claude" ? (
      <div className="divide-y divide-line">
        {apiFormatField}
        {/* Chat 接口一律 Bearer，认证字段只对 Anthropic 端点有意义。 */}
        {apiFormat === "anthropic" ? authFieldSelect : null}
      </div>
    ) : app === "codex" ? (
      <div className="divide-y divide-line">
        <FormField label={t("providers.modal.wireApi")} hint={t("providers.modal.wireApiHint")}>
          <Select value={wireApi} onValueChange={(value) => setWireApi(value as CodexWireApi)}>
            <SelectTrigger className="w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="responses">
                {t("providers.modal.wireApiResponsesOption")}
              </SelectItem>
              <SelectItem value="chat">{t("providers.modal.wireApiChatOption")}</SelectItem>
              <SelectItem value="anthropic">
                {t("providers.modal.wireApiAnthropicOption")}
              </SelectItem>
            </SelectContent>
          </Select>
          {wireApi !== "responses" ? (
            <p className="mt-1.5 text-xs text-warn">{t("providers.modal.wireApiChatRemoved")}</p>
          ) : null}
        </FormField>
        {/* Anthropic 端点有的认 x-api-key、有的认 Bearer，与 Claude 供应商同一选择。 */}
        {wireApi === "anthropic" ? authFieldSelect : null}
        <FormField
          label={t("providers.modal.queryParams")}
          hint={t("providers.modal.queryParamsHint")}
        >
          <Textarea
            value={queryParamsText}
            onChange={(e) => setQueryParamsText(e.target.value)}
            spellCheck={false}
            placeholder={"api-version=2025-04-01-preview"}
            className="h-16 font-mono text-xs transition-colors"
          />
        </FormField>
        <div className="space-y-2">
          <div className="flex min-h-8 items-center justify-between gap-3">
            <span className="text-[13px] text-ink">{t("providers.modal.contextWindow1M")}</span>
            <Switch
              checked={contextWindow1M}
              onChange={(next) => {
                setContextWindow1M(next);
                if (next && !autoCompactLimit) {
                  setAutoCompactLimit(String(CODEX_AUTO_COMPACT_LIMIT_1M));
                }
              }}
              label={t("providers.modal.contextWindow1M")}
            />
          </div>
          <p className="text-[11px] leading-relaxed text-faint">
            {t("providers.modal.contextWindow1MHint")}
          </p>
        </div>
        <FormField
          label={t("providers.modal.autoCompactLimit")}
          hint={t("providers.modal.autoCompactLimitHint")}
        >
          <Input
            className="w-full max-w-[200px] font-mono"
            inputMode="numeric"
            disabled={!contextWindow1M}
            placeholder={String(CODEX_AUTO_COMPACT_LIMIT_1M)}
            value={autoCompactLimit}
            onChange={(e) => setAutoCompactLimit(e.target.value.replace(/\D/g, ""))}
          />
        </FormField>
      </div>
    ) : null /* Gemini：认证/协议形态固定（.env + gemini-api-key），无高级项 */;

  /** 模型（Claude=兜底 + 四档角色映射 + 子代理覆盖，其余客户端=单个兜底模型）。 */
  const modelMappingSection = (
    <div>
      <div className="mb-1.5 flex items-center justify-between gap-2">
        <span className="text-xs font-medium text-muted">
          {app === "claude" ? t("providers.modal.modelMapping") : t("providers.modal.model")}
        </span>
        <div className="flex items-center gap-1.5">
          <Button
            size="sm"
            icon={Download}
            loading={fetchModels.isPending}
            disabled={!canFetchModels}
            onClick={() => fetchModels.mutate()}
          >
            {fetchModels.isPending
              ? t("providers.modal.fetchModelsLoading")
              : t("providers.modal.fetchModels")}
          </Button>
          {app === "claude" ? (
            <Button
              size="sm"
              icon={Wand2}
              disabled={
                !model.trim() &&
                !modelHaiku.trim() &&
                !modelSonnet.trim() &&
                !modelOpus.trim() &&
                !modelFable.trim()
              }
              onClick={quickSetModels}
            >
              {t("providers.modal.quickSetModels")}
            </Button>
          ) : null}
        </div>
      </div>
      {fetchModels.isError ? (
        <p className="mb-2 text-[11px] text-danger">
          {t("providers.modal.fetchModelsError", { error: errorText(fetchModels.error) })}
        </p>
      ) : null}
      {fetchedModels ? (
        <p className="mb-2 text-[11px] text-faint">
          {fetchedModels.length > 0
            ? t("providers.modal.fetchModelsPick", { count: fetchedModels.length })
            : t("providers.modal.fetchModelsEmpty")}
        </p>
      ) : null}
      {/* 模型名都短，两列（兜底 + 四档角色 + 子代理 = 3 行）一屏看得见全部
          映射；单列会拉成六条长条。 */}
      <div className="@container">
        <div className="grid gap-x-5 gap-y-4 @[420px]:grid-cols-2">
          <FormField label={t("providers.modal.fallbackModel")}>
            <ModelCombobox value={model} onChange={setModel} options={fetchedModels ?? []} />
          </FormField>
          {app === "claude"
            ? modelRoles.map((role) => (
                <FormField key={role.key} label={role.label} hint={role.hint}>
                  <ModelCombobox
                    value={role.value}
                    onChange={role.set}
                    options={fetchedModels ?? []}
                  />
                </FormField>
              ))
            : null}
        </div>
      </div>
      {app === "claude" ? (
        <p className="mt-1.5 text-[11px] text-faint">{t("providers.modal.modelMappingHint")}</p>
      ) : null}
    </div>
  );

  const setDesktopRow = (role: DesktopRoleId, patch: Partial<DesktopRouteRow>) =>
    setDesktopRows((rows) => ({ ...rows, [role]: { ...rows[role], ...patch } }));

  /** Claude Desktop 专属区块：模式开关 + proxy 四档路由表（替代通用高级区）。 */
  const desktopSection =
    app === "claude-desktop" ? (
      <div className="space-y-3">
        <div className="flex items-center justify-between gap-3">
          <span className="font-medium text-[13px] text-ink">
            {t("claudeDesktop.modelMappingToggle")}
          </span>
          <Switch
            checked={desktopMode === "proxy"}
            onChange={(next) => setDesktopMode(next ? "proxy" : "direct")}
            label={t("claudeDesktop.modelMappingToggle")}
          />
        </div>
        <p className="text-[11px] text-faint">
          {t(
            desktopMode === "proxy"
              ? "claudeDesktop.modelMappingOnHint"
              : "claudeDesktop.modelMappingOffHint",
          )}
        </p>
        {desktopMode === "proxy" ? (
          <>
            {apiFormatField}
            {apiFormat === "anthropic" ? authFieldSelect : null}
            <div>
              <div className="mb-1.5 flex items-center justify-between gap-2">
                <span className="text-xs font-medium text-muted">
                  {t("claudeDesktop.routeMapTitle")}
                </span>
                {/* 拉取上游模型填路由（对齐 cc-switch desktop 表单的获取模型按钮）。 */}
                <Button
                  size="sm"
                  icon={Download}
                  loading={fetchModels.isPending}
                  disabled={!canFetchModels}
                  onClick={() => fetchModels.mutate()}
                >
                  {fetchModels.isPending
                    ? t("providers.modal.fetchModelsLoading")
                    : t("providers.modal.fetchModels")}
                </Button>
              </div>
              {fetchModels.isError ? (
                <p className="mb-2 text-[11px] text-danger">
                  {t("providers.modal.fetchModelsError", { error: errorText(fetchModels.error) })}
                </p>
              ) : null}
              <div className="space-y-2">
                {DESKTOP_ROLE_ORDER.map((role) => (
                  <div key={role} className="grid grid-cols-[72px_1fr_1fr_auto] items-center gap-2">
                    <span className="flex h-9 items-center rounded-lg border border-line bg-app px-3 text-xs font-medium text-muted">
                      {t(`claudeDesktop.routeRole.${role}`)}
                    </span>
                    <Input
                      placeholder={t("claudeDesktop.labelOverrideLabel")}
                      value={desktopRows[role].label}
                      onChange={(e) => setDesktopRow(role, { label: e.target.value })}
                    />
                    <ModelCombobox
                      aria-label={t("claudeDesktop.upstreamModelLabel")}
                      placeholder={t("claudeDesktop.upstreamModelLabel")}
                      value={desktopRows[role].model}
                      onChange={(next) => setDesktopRow(role, { model: next })}
                      options={fetchedModels ?? []}
                    />
                    <Label
                      className="cursor-pointer gap-1.5 font-normal text-muted text-xs"
                      title={t("claudeDesktop.supports1mLabel")}
                    >
                      <Checkbox
                        checked={desktopRows[role].supports1m}
                        onCheckedChange={(checked) =>
                          setDesktopRow(role, { supports1m: checked === true })
                        }
                      />
                      {t("claudeDesktop.supports1mShort")}
                    </Label>
                  </div>
                ))}
              </div>
              <p className="mt-1.5 text-[11px] text-faint">{t("claudeDesktop.routeMapHint")}</p>
            </div>
          </>
        ) : (
          /* direct：手动指定模型列表（高级，可选）——对齐 cc-switch directModelList。 */
          <div>
            <CollapseToggle
              open={desktopDirectListOpen}
              onToggle={() => setDesktopDirectListOpen((v) => !v)}
            >
              {t("claudeDesktop.directModelListTitle")}
            </CollapseToggle>
            {!desktopDirectListOpen ? (
              <p className="mt-1 ml-1 text-[11px] text-faint">
                {t("claudeDesktop.directModelListCollapsedHint")}
              </p>
            ) : (
              <div className="mt-2 space-y-2">
                {DESKTOP_ROLE_ORDER.map((role) => {
                  const enabled = Boolean(desktopRows[role].model.trim());
                  return (
                    <div key={role} className="flex items-center gap-3">
                      <Label className="w-32 cursor-pointer gap-2 font-normal text-ink text-xs">
                        <Checkbox
                          checked={enabled}
                          onCheckedChange={(checked) =>
                            // direct 透传：启用即钉住角色 id 本身。
                            setDesktopRow(role, {
                              model: checked === true ? DESKTOP_ROLE_ROUTE_IDS[role] : "",
                            })
                          }
                        />
                        {t(`claudeDesktop.routeRole.${role}`)}
                      </Label>
                      <span className="font-mono text-[11px] text-faint">
                        {DESKTOP_ROLE_ROUTE_IDS[role]}
                      </span>
                      <Label
                        className={cn(
                          "gap-1.5 font-normal text-xs",
                          enabled ? "cursor-pointer text-muted" : "cursor-not-allowed text-faint",
                        )}
                        title={t("claudeDesktop.supports1mLabel")}
                      >
                        <Checkbox
                          disabled={!enabled}
                          checked={desktopRows[role].supports1m}
                          onCheckedChange={(checked) =>
                            setDesktopRow(role, { supports1m: checked === true })
                          }
                        />
                        {t("claudeDesktop.supports1mShort")}
                      </Label>
                    </div>
                  );
                })}
                <p className="text-[11px] text-faint">{t("claudeDesktop.directModelListHint")}</p>
              </div>
            )}
          </div>
        )}
      </div>
    ) : null;

  // 顶栏交代"在哪个客户端下、在编辑谁"。没选预设时副标题只留客户端名——
  // 标题已经是「添加供应商」，副标题再写一遍就是同一句话说两遍。
  const heading = profile
    ? profile.name
    : isOfficialSelected
      ? t(`providers.modal.officialPreset.${app}`)
      : (preset?.name ?? t("providers.modal.addTitle"));
  const subheading =
    profile || preset || isOfficialSelected
      ? `${CLIENT_META[app].label} · ${t(profile ? "providers.modal.editTitle" : "providers.modal.addTitle")}`
      : CLIENT_META[app].label;

  /**
   * 抬头的品牌图标。有内置 logo 才用中性板托（彩色 SVG 需要块干净的底）；
   * 认不出品牌时走主色板 + 首字母/通用图标——否则抬头整块（灰板 + 灰图标 +
   * 灰占位文案）全是灰的，正是这页最先被看出"没设计"的地方。
   */
  const resolvedIcon = iconKey ?? inferProviderIcon(name);
  const hasBrandGlyph = Boolean(resolvedIcon && hasBuiltinIcon(resolvedIcon));
  const cardGlyph = hasBrandGlyph ? (
    <ProviderIcon name={name} icon={iconKey} size={28} />
  ) : name.trim() ? (
    <ProviderIcon name={name} icon={iconKey} size={30} flat />
  ) : (
    <SlidersHorizontal size={24} aria-hidden />
  );

  /** 官方订阅：没有 Key、没有地址可填，只需交代"这是什么、怎么生效"。 */
  const officialCard = (
    <Card className="overflow-hidden">
      <SheetHero
        glyph={<ClientGlyph app={app} size={28} />}
        title={profile?.name ?? t(`providers.modal.officialPreset.${app}`)}
        badge={<Tag>{t("providers.editor.officialTag")}</Tag>}
        subtitle={<p className="text-[13px] text-muted">{t(`providers.officialDesc.${app}`)}</p>}
      />
      <div className="flex items-center gap-1.5 border-line border-t px-7 py-3">
        <BadgeCheck size={13} strokeWidth={2} aria-hidden className="shrink-0 text-faint" />
        <p className="text-[11px] text-faint">{t("providers.modal.officialNoKey")}</p>
      </div>
    </Card>
  );

  return (
    // 容器级脏检测：change/input 事件冒泡，一处接住所有字段输入。
    <div
      className="editor-panel-in"
      onChangeCapture={guard.markDirty}
      onInputCapture={guard.markDirty}
    >
      {/* 返回栏：面板占满页面，这一行负责交代"从哪来、在编辑谁"，并常驻主操作。
          负边距取 -mx-8 对齐 Shell 主区的 px-8——取 -mx-6 时左右各差 8px，
          滚动的内容会从这条缝里透到栏底下。 */}
      <div className="sticky top-0 z-10 -mx-8 mb-5 border-line border-b bg-app/90 px-8 py-3 backdrop-blur">
        <div className="flex flex-wrap items-center gap-3">
          <Button size="sm" variant="ghost" icon={ArrowLeft} onClick={guard.requestClose}>
            {t("providers.editor.back")}
          </Button>
          <div className="min-w-0 flex-1">
            <p className="truncate font-semibold text-[15px] text-ink">{heading}</p>
            <p className="truncate text-[11px] text-faint">{subheading}</p>
          </div>
          {error ? <p className="text-xs text-danger">{error}</p> : null}
          {isOfficialSelected ? (
            <Button
              size="sm"
              variant="primary"
              disabled={enableOfficial.isPending}
              onClick={() => enableOfficial.mutate()}
            >
              {enableOfficial.isPending
                ? t("providers.switching")
                : t("providers.modal.enableOfficial")}
            </Button>
          ) : (
            <Button
              size="sm"
              variant="primary"
              disabled={!valid || save.isPending}
              // 置灰时把原因带给鼠标与读屏：按钮自己不说，用户只能挨个字段猜。
              title={blocker ?? undefined}
              aria-describedby={blocker ? "provider-blocker" : undefined}
              onClick={() => save.mutate()}
            >
              {t("providers.modal.save")}
            </Button>
          )}
        </div>
      </div>

      {/* 新建：预设列 + 表单列并排；编辑：没有预设列，表单列居中，
          否则宽屏下整页内容全挤在左半边，右边空一大片。 */}
      {/* 单列居中：预设挑选在上、接入表单在下，新建与编辑走同一套骨架（编辑时
          只是没有上面那张预设纸）。此前预设是左侧一条 224px 竖列，27 家品牌挤在
          窄列里只能靠折叠收纳，而折叠掉的东西用户不会去点开。 */}
      <div className="mx-auto flex w-full max-w-[760px] flex-col gap-5">
        {isAdding ? (
          <Card className="overflow-hidden">
            <div className="flex flex-wrap items-center gap-x-3 gap-y-2 px-7 pt-5 pb-3">
              <span className="font-semibold text-[13px] text-ink">
                {t("providers.modal.preset")}
              </span>
              <SearchInput
                size="sm"
                placeholder={t("providers.editor.searchPresets")}
                value={presetSearch}
                onChange={(e) => setPresetSearch(e.target.value)}
                className="ml-auto w-full sm:w-52"
              />
            </div>

            <div className="px-7 pb-5">
              {!presetQuery ? (
                // 官方订阅与空白自定义是"不选任何厂商"的两个入口，与下面按品牌
                // 罗列的预设不是一类，单独一行放在最前面。
                <div className={cn(PRESET_GRID, "mb-4")}>
                  {appHasOfficial ? (
                    <PresetTile
                      selected={isOfficialSelected}
                      label={t(`providers.modal.officialPreset.${app}`)}
                      // 选中格是实心主色底，图标得跟着变白，故那时改用 inherit。
                      icon={
                        <ClientGlyph
                          app={app}
                          size={16}
                          tone={isOfficialSelected ? "inherit" : "brand"}
                        />
                      }
                      onClick={() => setPresetId("official")}
                    />
                  ) : null}
                  <PresetTile
                    selected={presetId === "custom"}
                    label={t("providers.modal.presetCustom")}
                    // 自定义没有品牌可依，用主色——与它选中时抬头那块主色图标板呼应。
                    icon={
                      <SlidersHorizontal
                        size={16}
                        aria-hidden
                        className={presetId === "custom" ? undefined : "text-accent"}
                      />
                    }
                    onClick={() => applyPreset(null)}
                  />
                  {/* 推荐项与这两个入口同排：它也是"不用翻找就能选"的一格。自己占
                      一整行的话，右边空出三格，看着像掉队的一项而不是被推荐的一项。 */}
                  {presets.filter((p) => p.recommended).map(renderPresetTile)}
                </div>
              ) : null}

              {presetQuery ? (
                matchedPresets.length > 0 ? (
                  <div className={PRESET_GRID}>{matchedPresets.map(renderPresetTile)}</div>
                ) : (
                  <p className="py-6 text-center text-muted text-xs">{t("providers.noMatch")}</p>
                )
              ) : (
                <div className="space-y-4">
                  {/*
                    分组全部铺开，不折叠（对齐 cc-switch：它把全部预设平铺成一个网格）。
                    收起态省下来的长度，代价是用户根本不会去点——27 家预设有 25 家
                    一直藏着。组标题因此退化成静态 eyebrow：仍交代分类与数量，但不是按钮。
                  */}
                  {PRESET_CATEGORY_ORDER.map((cat) => {
                    const inCat = presets.filter(
                      (p) => presetCategory(p) === cat && !p.recommended,
                    );
                    if (inCat.length === 0) return null;
                    return (
                      <div key={cat}>
                        <p className="flex items-center gap-2 pb-1.5 font-semibold text-[11px] text-faint uppercase tracking-[0.05em]">
                          <span className="truncate">
                            {t(`providers.modal.presetCategory.${cat}`)}
                          </span>
                          <span className="shrink-0 font-normal tabular-nums">{inCat.length}</span>
                        </p>
                        <div className={PRESET_GRID}>{inCat.map(renderPresetTile)}</div>
                      </div>
                    );
                  })}
                </div>
              )}

              <p className="mt-4 text-[11px] text-faint leading-relaxed">
                {isOfficialSelected
                  ? // Desktop 官方是应用内置 1P 登录，非浏览器 OAuth，文案单列。
                    t(
                      app === "claude-desktop"
                        ? "claudeDesktop.officialNotice"
                        : "providers.modal.officialHint",
                    )
                  : t("providers.modal.presetHint")}
              </p>
            </div>
          </Card>
        ) : null}

        <div className="min-w-0 space-y-5">
          {isOfficialSelected ? (
            officialCard
          ) : isOfficialEdit ? (
            // 官方订阅编辑：无 Key/地址可配，只开放配置片段与通用配置。
            <>
              {officialCard}
              {/* Desktop 官方 = 内置 1P 登录，无片段可配（profile 全托管）。 */}
              {app !== "claude-desktop" ? configJsonSection : null}
            </>
          ) : (
            <>
              {app === "codex" && isAdding ? (
                <p className="rounded-lg bg-warn-soft px-3 py-2.5 text-warn text-xs">
                  {t("providers.modal.codexRelayHint")}
                </p>
              ) : null}

              {/*
                接入纸：这一页的主戏。抬头 + 三段接线（身份 / 端点 / 凭证），缺东西时
                纸底出一条缺口提示。其余全部收进下方折叠段——把"粘一个 Key 就能用"的
                主路径和"改 settings.json"的长尾拉开视觉权重差。
              */}
              <Card className="overflow-hidden">
                {/* 抬头：整页唯一的大字号，一眼定身份。 */}
                <SheetHero
                  glyph={cardGlyph}
                  tone={hasBrandGlyph ? "brand" : "accent"}
                  title={name.trim() || t("providers.editor.untitled")}
                  badge={
                    preset?.recommended ? (
                      <RecommendedBadge>{t("providers.modal.presetRecommended")}</RecommendedBadge>
                    ) : null
                  }
                  subtitle={
                    <p className="truncate font-mono text-xs text-faint">
                      {baseUrl.trim() || t("providers.editor.noBaseUrl")}
                    </p>
                  }
                />

                {/*
                  主路径分三段走轨：这是谁 → 接口在哪 → 拿什么调用。后一步在前一步没定
                  之前没有意义，轨把这层顺序画出来，同时把"还差哪一步"变成扫一眼的事。
                  每段第一行是该段的主字段，可选字段降到第二行并排——视线从上往下越走
                  越窄，最后落在唯一真正要粘贴的东西（Key）上。
                */}
                <div className="@container border-line border-t px-7 pt-6 pb-6">
                  <FormSection
                    label={t("providers.editor.section.identity")}
                    state={name.trim() ? "done" : "todo"}
                  >
                    <div className="space-y-4">
                      <FormField id="provider-name" label={t("providers.modal.name")} required>
                        {/* 编辑态 name 是首字段，聚焦即可改；新建态预设列在左侧，
                          自动聚焦会把焦点从预设选择上抢走，故只在编辑态自动聚焦。 */}
                        <Input
                          id="provider-name"
                          autoFocus={!isAdding}
                          value={name}
                          onChange={(e) => setName(e.target.value)}
                        />
                      </FormField>
                      {/* 两个可选字段并排收在必填项下面，不与主字段抢竖排节奏。 */}
                      <div className="grid gap-x-5 gap-y-4 @[420px]:grid-cols-2">
                        <FormField id="provider-notes" label={t("providers.modal.notes")}>
                          <Input
                            id="provider-notes"
                            placeholder={t("providers.modal.notesPlaceholder")}
                            value={notes}
                            onChange={(e) => setNotes(e.target.value)}
                          />
                        </FormField>
                        <FormField id="provider-website" label={t("providers.modal.websiteUrl")}>
                          <Input
                            id="provider-website"
                            className="font-mono"
                            placeholder="https://example.com"
                            value={websiteUrl}
                            onChange={(e) => setWebsiteUrl(e.target.value)}
                          />
                        </FormField>
                      </div>
                    </div>
                  </FormSection>

                  <FormSection
                    label={t("providers.editor.section.endpoint")}
                    state={baseUrl.trim() ? "done" : "todo"}
                  >
                    <FormField
                      id="provider-base-url"
                      label={t("providers.modal.baseUrl")}
                      required
                      hint={t("providers.modal.baseUrlHint")}
                      // 备用地址与测速就是围绕这个字段的操作，入口挂在它的标签行上。
                      action={
                        <button
                          type="button"
                          onClick={() => openSection("endpoints")}
                          className="inline-flex items-center gap-1 text-[11px] text-muted transition-colors hover:text-ink"
                        >
                          <Zap size={12} strokeWidth={2} aria-hidden />
                          {t("providers.modal.endpoints")}
                        </button>
                      }
                    >
                      <Input
                        id="provider-base-url"
                        className="font-mono"
                        placeholder="https://relay.example.com/api"
                        value={baseUrl}
                        onChange={(e) => setBaseUrl(e.target.value)}
                      />
                    </FormField>
                  </FormSection>

                  <FormSection
                    label={t("providers.editor.section.credential")}
                    // Key 不是必填（可以先存配置later再补），所以没填是 optional 不是 todo——
                    // 空心点会被读成"还差这一步"，而它并不拦着保存。
                    state={apiKey.trim() || profile?.apiKeyRef ? "done" : "optional"}
                    last
                  >
                    <FormField
                      id="provider-api-key"
                      label={t("providers.modal.apiKey")}
                      hint={t("providers.modal.apiKeyHint")}
                      action={
                        apiKeyUrl ? (
                          <button
                            type="button"
                            onClick={() => void ipc.openExternal(apiKeyUrl)}
                            className="inline-flex items-center gap-1 text-[11px] text-accent hover:underline"
                          >
                            {t("providers.modal.getApiKey")}
                            <ExternalLink size={11} strokeWidth={2} aria-hidden />
                          </button>
                        ) : null
                      }
                    >
                      <div className="relative">
                        <Input
                          id="provider-api-key"
                          type={showKey ? "text" : "password"}
                          autoComplete="off"
                          className="pr-10"
                          placeholder={profile?.apiKeyRef ? t("providers.modal.apiKeyKeep") : ""}
                          value={apiKey}
                          onChange={(e) => setApiKey(e.target.value)}
                        />
                        {apiKey ? (
                          <button
                            type="button"
                            onClick={() => setShowKey((v) => !v)}
                            aria-label={
                              showKey ? t("providers.modal.hideKey") : t("providers.modal.showKey")
                            }
                            className="absolute inset-y-0 right-0 flex items-center pr-3 text-faint transition-colors hover:text-ink"
                          >
                            {showKey ? (
                              <EyeOff size={15} aria-hidden />
                            ) : (
                              <Eye size={15} aria-hidden />
                            )}
                          </button>
                        ) : null}
                      </div>
                    </FormField>
                  </FormSection>
                </div>

                {/* 缺口条只在真的缺东西时出现：轨上三个点已经说明进度，就绪后再留一行
                    常驻灰字既没有新信息，又给纸面收了个灰边。 */}
                {blocker ? (
                  <div
                    id="provider-blocker"
                    className="flex items-center gap-2 border-line border-t bg-warn-soft px-7 py-3"
                  >
                    <TriangleAlert
                      size={13}
                      strokeWidth={2}
                      aria-hidden
                      className="shrink-0 text-warn"
                    />
                    <p className="text-[11px] text-warn">{blocker}</p>
                  </div>
                ) : null}
              </Card>

              {/* 配置片段常驻铺开（对齐 cc-switch）：这份文件是切换时真正写下去的东西，
                  每次都要能直接看到，不该藏在折叠里。Desktop 的 profile 全托管，没有片段概念。 */}
              {app !== "claude-desktop" ? configJsonSection : null}

              {/* 长尾配置：段标题常驻、右侧摘要说明里面配了什么，展开才铺开表单。
                  组头把它们整体标成"可选"，用户扫到这里就知道可以停手了。 */}
              {/* 组头做 eyebrow：小号、加粗、字距拉开——与纸内的段名同级但更轻，
                  一眼看出"下面这些和上面那张纸不是一回事"。 */}
              <p className="px-1 pt-2 pb-1 font-semibold text-[11px] text-faint tracking-[0.06em] uppercase">
                {t("providers.editor.optionalGroup")}
              </p>
              <SettingsAccordion value={openSections} onValueChange={setOpenSections}>
                {/* 端点候选与测速（对齐 cc-switch EndpointSpeedTest） */}
                <SettingsAccordionItem
                  value="endpoints"
                  icon={Zap}
                  title={t("providers.modal.endpoints")}
                  summary={endpointsSummary}
                >
                  <div className="space-y-2">
                    <div className="flex items-center justify-between gap-3">
                      <p className="text-[11px] text-faint">{t("providers.modal.endpointsHint")}</p>
                      <Button
                        size="sm"
                        disabled={testingEndpoints || endpointList.length === 0}
                        onClick={() => void runEndpointTest()}
                      >
                        {testingEndpoints
                          ? t("providers.modal.endpointTesting")
                          : t("providers.modal.endpointTest")}
                      </Button>
                    </div>
                    {endpointList.map((url) => {
                      const result = latency[url];
                      const active = url === baseUrl.trim();
                      return (
                        <button
                          key={url}
                          type="button"
                          onClick={() => setBaseUrl(url)}
                          title={t("providers.modal.endpointUse")}
                          className={cn(
                            "flex w-full items-center justify-between gap-2 rounded-lg border px-3 py-2 text-left font-mono text-xs transition-colors",
                            active
                              ? "border-accent bg-accent-soft/40 text-ink"
                              : "border-line bg-app text-muted hover:border-line-strong hover:text-ink",
                          )}
                        >
                          <span className="truncate">{url}</span>
                          <span className="shrink-0 font-sans text-[11px]">
                            {result
                              ? result.latencyMs != null
                                ? t("providers.modal.endpointLatency", {
                                    ms: result.latencyMs,
                                    status: result.status,
                                  })
                                : t("providers.modal.endpointFail")
                              : null}
                          </span>
                        </button>
                      );
                    })}
                    <div className="flex gap-2">
                      <Input
                        className="flex-1 font-mono"
                        placeholder={t("providers.modal.endpointAddPlaceholder")}
                        value={endpointInput}
                        onChange={(e) => setEndpointInput(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") {
                            e.preventDefault();
                            addEndpoint();
                          }
                        }}
                      />
                      <Button size="sm" disabled={!endpointInput.trim()} onClick={addEndpoint}>
                        {t("providers.modal.endpointAdd")}
                      </Button>
                    </div>
                  </div>
                </SettingsAccordionItem>

                {/* 模型：Desktop 走四档路由，additive 客户端要一份显式模型列表，其余是映射表。 */}
                <SettingsAccordionItem
                  value="models"
                  icon={Boxes}
                  title={t(
                    app === "claude-desktop"
                      ? "claudeDesktop.routeMapTitle"
                      : "providers.editor.section.models",
                  )}
                  summary={modelsSummary}
                >
                  {app === "claude-desktop" ? (
                    desktopSection
                  ) : (
                    <div className="space-y-5">
                      {additiveModelsSection}
                      {modelMappingSection}
                    </div>
                  )}
                </SettingsAccordionItem>

                {/* 高级：认证形态、自定义请求头、请求体覆盖。Desktop 的认证字段在路由段内，无此段。 */}
                {app !== "claude-desktop" &&
                (authSection || headersSection || routeBodyOverrideSection) ? (
                  <SettingsAccordionItem
                    value="advanced"
                    icon={SlidersHorizontal}
                    title={t("providers.modal.advanced")}
                    summary={advancedSummary}
                  >
                    <div className="space-y-5">
                      {authSection}
                      {headersSection}
                      {routeBodyOverrideSection}
                    </div>
                  </SettingsAccordionItem>
                ) : null}
              </SettingsAccordion>
            </>
          )}
        </div>
      </div>

      {editingCommon ? (
        <CommonSnippetModal app={app} onClose={() => setEditingCommon(false)} />
      ) : null}

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
    </div>
  );
}

/**
 * 全局「通用配置」片段编辑（对齐 cc-switch 的 Common Config）：
 * 跨供应商共享的 settings.json JSON 子集，勾选「应用通用配置」的
 * 中转站在切换时叠加写入（同名键以通用配置为准）。
 */
function CommonSnippetModal({ app, onClose }: { app: ClientApp; onClose: () => void }) {
  const { t } = useTranslation();
  const guard = useUnsavedGuard(onClose);
  const [text, setText] = useState<string | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);

  // 打开时加载一次；加载完成前 text 为 null（禁用编辑区）。
  useEffect(() => {
    let cancelled = false;
    ipc
      .providerCommonSnippet(app)
      .then((snippet) => {
        if (!cancelled) setText(formatFragment(app, snippet));
      })
      .catch((e) => {
        if (!cancelled) {
          setText("");
          setSaveError(errorText(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [app]);

  const save = useMutation({
    mutationFn: async () => {
      const snippet = parseFragment(app, text ?? "");
      await ipc.providerCommonSnippetSet(app, snippet);
    },
    onSuccess: onClose,
    onError: (e) => setSaveError(errorText(e)),
  });

  const handleChange = (next: string) => {
    guard.markDirty();
    setText(next);
    try {
      parseFragment(app, next);
      setParseError(null);
    } catch (e) {
      setParseError(errorText(e));
    }
  };

  return (
    <Modal
      size="md"
      title={t("providers.modal.commonConfigTitle")}
      onClose={guard.requestClose}
      footer={
        <>
          {saveError ? <p className="mr-auto text-xs text-danger">{saveError}</p> : null}
          <Button size="sm" onClick={guard.requestClose}>
            {t("providers.modal.cancel")}
          </Button>
          <Button
            size="sm"
            variant="primary"
            disabled={text === null || Boolean(parseError) || save.isPending}
            onClick={() => save.mutate()}
          >
            {t("providers.modal.save")}
          </Button>
        </>
      }
    >
      <div className="space-y-2">
        <p className="text-xs text-muted">
          {t(
            app !== "codex"
              ? "providers.modal.commonConfigDesc"
              : "providers.modal.commonConfigDescCodex",
          )}
        </p>
        <JsonEditor
          value={text ?? ""}
          onChange={handleChange}
          disabled={text === null}
          language={app !== "codex" ? "json" : "toml"}
          placeholder={
            app !== "codex"
              ? '{\n  "includeCoAuthoredBy": false\n}'
              : 'model_reasoning_effort = "high"'
          }
          minHeight={192}
          hasError={Boolean(parseError)}
        />
        {parseError ? (
          <p className="text-[11px] text-danger">
            {t(
              app !== "codex"
                ? "providers.modal.configJsonInvalid"
                : "providers.modal.configTomlInvalid",
              { error: parseError },
            )}
          </p>
        ) : null}
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

/** 预设网格里的单个按钮：选中态默认 accent 底色，官方项可传品牌色。 */
/**
 * 预设列的一行。选中态不用整块反色（一列里反色块太吵），改用左侧 2px 主色标尺 +
 * 淡底——同一套语汇也用在当前生效的供应商卡片上，"这一项是选中的"全应用一致。
 */
/** 预设网格的列定义：一行放得下 3–4 家，窄窗口自动降列（对齐 cc-switch 的 auto-fill 网格）。 */
const PRESET_GRID = "grid grid-cols-[repeat(auto-fill,minmax(150px,1fr))] gap-2";

function PresetTile({
  selected,
  label,
  icon,
  badge,
  brandName,
  onClick,
}: {
  selected: boolean;
  label: string;
  icon?: React.ReactNode;
  /** 标签徽标（如「特别推荐」），置于名称右侧。 */
  badge?: React.ReactNode;
  /**
   * 名称是品牌专有名词（不翻译，且「火山 Agentplan」这类本来就带中文）。
   * 打上标记让 i18n 残留守卫跳过这一格；官方 / 自定义那两格是真 i18n 文案，不打。
   */
  brandName?: boolean;
  onClick: () => void;
}) {
  return (
    // 未选中是白纸上的浅色凹槽（同输入框语汇，无边框），选中是实心主色 + 白字——
    // 27 个格子里有且只有一个是实心的，扫一眼就知道选了谁。
    <button
      type="button"
      onClick={onClick}
      aria-pressed={selected}
      data-preset-names={brandName ? "" : undefined}
      className={cn(
        "flex w-full items-center gap-2 rounded-lg px-2.5 py-2 text-left font-medium text-[13px] transition-colors",
        selected
          ? "bg-accent-solid text-on-accent-solid"
          : "bg-slot text-muted hover:bg-hover hover:text-ink",
      )}
    >
      {/* 图标不跟随格子的次要文字色：单色 logo 靠 currentColor 上色，继承 muted
          会让它比旁边的彩色 logo 淡一档，看着像没加载完。选中格是白字，图标
          跟着走即可（彩色 logo 那时由 plate 的白衬底托住）。 */}
      <span className={cn("flex shrink-0", selected ? undefined : "text-ink")}>
        {icon ?? <span className="inline-block h-4 w-4" aria-hidden />}
      </span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {badge}
    </button>
  );
}

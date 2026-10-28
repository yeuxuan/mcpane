/**
 * Tauri IPC 类型化封装。
 * 浏览器/测试环境（无 Tauri runtime）下调用会抛错，由查询层的
 * error 状态兜底展示，不阻塞页面渲染。
 */
import { invoke } from "@tauri-apps/api/core";

/** Claude Code / Codex 单客户端状态（安装 + 接入 + How To Use 所需信息）。 */
export interface DedicatedClientStatus {
  installed: boolean;
  version: string | null;
  /** 配置文件里已存在 mcpane Hub 条目。 */
  connected: boolean;
  configPath: string;
  tokenEnv: string;
}

export interface ClientsStatus {
  claude: DedicatedClientStatus;
  codex: DedicatedClientStatus;
}

export interface McpClientStatus {
  id: string;
  display: string;
  installed: boolean;
  connected: boolean;
  tokenEnv: string | null;
  configPath: string;
  /** 用户手动添加的客户端：没有"接入/断开"，只有凭证与访问控制。 */
  custom?: boolean;
}

/** 用户手动添加的 MCP 客户端（后端 `CustomMcpClient`）。 */
export interface CustomMcpClient {
  /** 稳定标识，同时是连接的 clientType。 */
  id: string;
  display: string;
  createdAt: number;
}

/**
 * 内置客户端 id（与后端 `mcp_clients::CLIENTS` + 两个专用 adapter 对齐）。
 *
 * 写成值而不只是类型：图标表漏配这条编译器管不到——`isBrandName` 认不出就
 * 静默不画图，卡片上只剩一个空色块。有了这份清单，测试能逐个断言。
 */
export const CLIENT_TYPE_IDS = [
  "claude",
  "claude-desktop",
  "codex",
  "cursor",
  "vscode",
  "cline",
  "devin",
  "gemini-cli",
  "antigravity",
  "trae",
  "qoder",
  "qwen-code",
  "codebuddy",
  "zcode",
] as const;

export type ClientTypeId = (typeof CLIENT_TYPE_IDS)[number];

export interface HubConnection {
  clientConnectionId: string;
  /**
   * 内置客户端是 ClientTypeId；用户自定义客户端上报的是它自己的 id（不是字面量
   * "custom"），显示名要经 clientDisplayName 翻译。`string & {}` 保住内置 id 的补全。
   */
  clientType: ClientTypeId | (string & {});
  tokenPrefix: string;
  enabled: boolean;
  /** 绑定的默认 Workspace；null 表示跟随默认工作区。 */
  defaultWorkspaceId: string | null;
  /** 该连接最近一次请求协商到的 MCP 协议版本；null 表示尚未见到请求。 */
  protocolVersion?: string | null;
  /** 工具暴露模式（ADR-020）：flat 平铺 / gateway 网关 / auto 按客户端自适应。 */
  toolExposureMode: ToolExposureMode;
}

/** 工具暴露模式（ADR-020）。 */
export type ToolExposureMode = "flat" | "gateway" | "auto";

/** 凭据模式（两种都一键完成，ADR-016）：env 默认（envsync 代管环境变量，
 * 明文存 MCPane 数据目录）/ inline 可选（Token 直写客户端配置）。 */
export type CredentialMode = "inline" | "env";

/** envsync 持久化报告（后端 EnvSyncReport）。 */
interface EnvSyncReport {
  /** 凭据落点（env.sh 路径 / HKCU\Environment）。 */
  credentialStore: string;
  rcFiles: string[];
  guiSession: "synced_now" | "synced_at_login" | "not_applicable";
  persistenceAgent: "installed" | "updated" | "unchanged" | "failed" | "not_applicable";
  warnings: string[];
}

/** 一键接入预览（对比弹窗数据源）。 */
export interface ConnectPreview {
  targetPath: string;
  unifiedDiff: string;
  beforeText: string;
  afterText: string;
  credentialMode: CredentialMode;
  /** EnvVar 默认模式的环境变量名（inline 模式为 null）；写入时自动设置。 */
  envVar: string | null;
}

/** 一键接入写入结果。 */
export interface ConnectOutcome {
  targetPath: string;
  unifiedDiff: string;
  snapshotId: string;
  credentialMode: CredentialMode;
  /** 本次写入的 Token 前缀（凭证页可对照识别）。 */
  tokenPrefix: string | null;
  /** 收尾指引：两种模式都只剩「重启客户端」，env 模式附持久化明细。 */
  nextSteps: string[];
}

/** 轮换 Token 联动同步的结果。 */
export interface RotateSyncOutcome {
  tokenPrefix: string;
  /** 仅完全未接入时返回明文供一次性展示。 */
  token: string | null;
  /** Inline 形态：配置文件已就地改写。 */
  synced: { targetPath: string; snapshotId: string } | null;
  /** EnvVar 形态：环境变量已由 envsync 重写。 */
  envSynced: EnvSyncReport | null;
}

export interface HubStatus {
  name: string;
  serverVersion: string;
  /** 主协议版本。Hub 是双栈的，展示时需连同 legacyProtocolVersion 一起说（ADR-019）。 */
  protocolVersion: string;
  /** 兼容层版本：两个锚定客户端目前实际走的那一档。 */
  legacyProtocolVersion: string;
  endpoint: string;
  uptimeSecs: number;
  connections: HubConnection[];
}

/** Hub 托管进程状态（serde tag="state"，见 src-tauri hub_process.rs）。 */
export type HubProcessState =
  | { state: "probing" }
  | { state: "disowned" }
  | { state: "starting" }
  | { state: "managed"; pid: number }
  | { state: "backoff"; attempt: number }
  | {
      state: "failed";
      reason: "portOccupied" | "repeatedCrash";
      /** 端口占用者：仅 portOccupied 会带，且只有 unix 查得到（Windows 无对应探测）。 */
      occupant?: { name: string; pid: number };
    }
  | { state: "disabled" }
  | { state: "shutDown" };

/**
 * 全局出网代理配置（ADR-017）。形状对齐 Rust 侧 `#[serde(tag = "mode")]`。
 *
 * `system` 与 `direct` 是两回事：前者跟随系统与环境变量，后者强制直连。
 * 少了 `direct`，用户就没有「明确关掉代理」的表达方式。
 */
export type ProxyConfig = { mode: "system" } | { mode: "direct" } | { mode: "manual"; url: string };

/** 扫描到的本机代理（已实测可用）。 */
export interface DetectedProxy {
  url: string;
  port: number;
  /** 该端口常见归属，仅作提示。 */
  hint: string;
  latencyMs: number;
}

export interface ProxyTestResult {
  ok: boolean;
  latencyMs: number | null;
  error: string | null;
}

/** Provider 归属的客户端 app。 */
export type ClientApp =
  | "claude"
  | "codex"
  | "gemini"
  | "grok"
  | "opencode"
  | "openclaw"
  | "hermes"
  | "claude-desktop";

// additive 与否等客户端元数据统一放 lib/clients.ts（单一事实源），
// 这里只留协议类型与 IPC 调用。
/** 只有两类：官方订阅（OAuth 登录）与第三方中转站（base_url + API Key）。 */
type ProviderCategory = "official" | "relay";

/** additive 客户端的上游协议。 */
export type UpstreamApi = "openai" | "anthropic";

/** Claude 认证环境变量形态：多数中转站用 auth-token，少数要求 api-key（x-api-key 头）。 */
export type ClaudeAuthField = "auth-token" | "api-key";

/**
 * Codex 中转站 wire_api。Codex 2026-02 起只接受 responses；chat 仅为读懂旧档案
 * 保留，编辑器里标「已不支持」、切换时被拒绝。
 */
export type CodexWireApi = "responses" | "chat" | "anthropic";
/** Claude / Claude Desktop 上游协议；openai-chat 须经本地路由转换。 */
export type ClaudeApiFormat = "anthropic" | "openai-chat" | "openai-responses";

/**
 * Claude Desktop 写入模式：direct（供应商直接卖 claude-* 模型，profile 直指）
 * 或 proxy（非 Claude 厂商，经本地网关做模型名映射）。
 */
export type DesktopMode = "direct" | "proxy";

/** Claude Desktop 模型路由（四档 Sonnet/Opus/Haiku/Fable）。 */
export interface DesktopRoute {
  /** Desktop 模型菜单的 route ID（claude-safe，UI 生成恒合法）。 */
  routeId: string;
  /** 上游真实模型名（direct 模式必须与 routeId 相同）。 */
  upstreamModel: string;
  /** 菜单显示名（品牌名）；省略则显示 routeId。 */
  labelOverride?: string;
  /** 向 Desktop 声明 1M 上下文能力。 */
  supports1m: boolean;
}

export interface ProviderProfile {
  schemaVersion: number;
  id: string;
  app: ClientApp;
  category: ProviderCategory;
  name: string;
  baseUrl: string | null;
  /** 兜底模型（Claude 写 ANTHROPIC_MODEL；Codex 写顶层 model）。 */
  model: string | null;
  /** Claude 模型角色映射（ANTHROPIC_DEFAULT_*_MODEL）；Codex 忽略。 */
  modelHaiku: string | null;
  modelSonnet: string | null;
  modelOpus: string | null;
  modelFable: string | null;
  /** 全部子代理/agent team/workflow 的模型（CLAUDE_CODE_SUBAGENT_MODEL）；不做回落。 */
  modelSubagent: string | null;
  /** Claude 认证环境变量形态；Codex 忽略。 */
  apiKeyField: ClaudeAuthField;
  /** Codex config.toml 的 wire_api；Claude 忽略。 */
  codexWireApi: CodexWireApi;
  claudeApiFormat?: ClaudeApiFormat;
  /** Claude Desktop 写入模式；其他 app 忽略。 */
  desktopMode: DesktopMode;
  /** Claude Desktop 模型路由表；其他 app 忽略。 */
  desktopRoutes: DesktopRoute[];
  /**
   * 附加配置片段（对齐 cc-switch「配置 JSON」）：Claude 为 settings.json
   * 子集，Codex 为 config.toml 子树的 JSON 表达。切换时深合并写入，
   * 切走时按值匹配深移除。null = 空。
   */
  extraSettings: Record<string, unknown> | null;
  /** 是否叠加所属 app 的全局「通用配置」片段。 */
  commonConfigEnabled: boolean;
  /** 请求地址候选列表（测速/一键切换，对齐 cc-switch endpointCandidates）。 */
  endpointCandidates: string[];
  /** 声明给客户端的模型列表；**只有 additive 客户端用它**（OpenCode / OpenClaw / Hermes）。 */
  models: string[];
  /**
   * 上游协议；**只有 additive 客户端用它**。同一件事两家写法不同——
   * OpenCode 换 AI SDK 包、OpenClaw 写 api 字段，后端负责映射。null = OpenAI 兼容。
   */
  upstreamApi: UpstreamApi | null;
  /**
   * 自定义请求头：additive 客户端（OpenCode / OpenClaw）始终生效；对支持本地路由的
   * app（claude/codex/gemini/grok）的中转站，仅在本地路由接管时作为上游请求头发送
   * （鉴权/host/content-type 等受保护头会被忽略）。
   */
  headers: Record<string, string>;
  /**
   * 本地路由接管时对协议转换后的上游请求体做 JSON Merge Patch；null = 不覆盖，
   * 值为 null 的键表示删除。仅支持路由的 app 的中转站使用，`stream` 不可覆盖。
   */
  routeBodyOverride: Record<string, unknown> | null;
  /** 模型上下文长度；**只有 Hermes 用**，null 则不写该键（用客户端默认）。 */
  contextLength: number | null;
  /** Codex URL query 参数（Azure 的 api-version 等）；Claude 忽略。 */
  codexQueryParams: Record<string, string>;
  /** Codex 1M 上下文窗口；开启时写入 model_context_window = 1000000。 */
  codexContextWindow1m: boolean;
  /** 开启 1M 时的自动压缩阈值；null 用默认 900000。 */
  codexAutoCompactLimit: number | null;
  /** 显式图标 key（内置品牌图标表）；null 走名称推断/首字母兜底。 */
  icon: string | null;
  /** 备注（卡片第二行优先展示；不写入客户端配置）。 */
  notes: string | null;
  /** 官网/控制台地址（卡片外链与获取 Key 入口）。 */
  websiteUrl: string | null;
  apiKeyRef: { account: string } | null;
  /** Codex 中转站的 [model_providers.<slug>] 表名（后端生成，前端只读）。 */
  codexSlug: string | null;
  sortOrder: number;
}

/** 每个 app 当前生效的 Provider id（null = 从未切换）。 */
export interface CurrentProviders {
  claude: string | null;
  codex: string | null;
  gemini: string | null;
  grok: string | null;
  opencode: string | null;
  openclaw: string | null;
  hermes: string | null;
  "claude-desktop": string | null;
}

/** 各客户端是否检测到安装（CLI 查 PATH，Claude Desktop 查应用本体）。 */
export type ClientInstallStatus = Record<ClientApp, boolean>;

/**
 * `--version` 探测失败的原因（对应 Rust 的 `ProbeFailure`，内部标签 `kind`）。
 * - `spawn`：进程根本没起来，`detail` 是系统给的 io 错误原文
 * - `timeout`：到点仍未退出，已被击杀
 * - `exit`：跑起来了但非零退出，`stderr` 为末几行原文（可能为空）
 */
export type ClientEnvProblem =
  | { kind: "spawn"; detail: string }
  | { kind: "timeout"; seconds: number }
  | { kind: "exit"; code: number | null; stderr: string };

/** 本地环境实况：装没装、装在哪、什么版本、跑不跑得起来。 */
export interface ClientEnv {
  app: ClientApp;
  installed: boolean;
  /** `<cli> --version` 解析出的版本；GUI 应用与解析失败为 null。 */
  version: string | null;
  /** 可执行文件 / 应用本体的绝对路径。 */
  path: string | null;
  /** false = 装了但跑不起来（npm 漏装平台二进制等半损坏状态）。 */
  runnable: boolean;
  /** runnable 为 false 时的失败原因；三种失败的补救动作不同，故分开上报。 */
  problem: ClientEnvProblem | null;
  /** 同一个 CLI 的其他安装位置（不含 path 那份）；非空 = 装了好几份。 */
  conflicts: string[];
}

/** Claude Desktop 状态面（对齐 cc-switch ClaudeDesktopStatus 字段语义）。 */
export interface ClaudeDesktopStatus {
  /** 当前平台是否支持 3P 配置写入（macOS/Windows）。 */
  supported: boolean;
  /** profile 存在或 meta 有我们的条目。 */
  configured: boolean;
  appliedId: string | null;
  /** profile 实际指向的地址（与 expected 不一致 = 被外部改动）。 */
  actualBaseUrl: string | null;
  /** profile 中存在非 claude-safe 模型名（新版 Desktop 会拒收整组）。 */
  staleRawModels: boolean;
  /** 当前生效供应商的模式（官方/未切换为 null）。 */
  mode: DesktopMode | null;
  expectedBaseUrl: string | null;
  /** proxy 模式但路由表无有效映射。 */
  missingRouteMappings: boolean;
  gatewayTokenConfigured: boolean;
  proxyRunning: boolean;
  profilePath: string;
}

/** Claude Desktop 本地网关运行态。 */
export interface DesktopProxyStatus {
  running: boolean;
  /** 实际监听端口（未运行为 null）。 */
  port: number | null;
  /** 是否有生效的上游快照。 */
  hasUpstream: boolean;
  /** store 中配置的监听端口。 */
  listenPort: number;
  /** 最近一次同步失败原因（如端口被占用）；成功后为 null。 */
  lastError: string | null;
}

/** 熔断/超时调参：决定何时判定当前供应商故障并转移。范围校验在后端。 */
export interface RoutingTuning {
  /** 首字节超时（秒），10–600。 */
  firstByteTimeoutSecs: number;
  /** 无新数据中止（秒），30–1800。 */
  idleTimeoutSecs: number;
  /** 连续失败达到几次判定故障，1–20。 */
  failureThreshold: number;
  /** 熔断后的暂停时长（秒），10–3600。 */
  cooldownSecs: number;
}

/** 与后端一致的缺省值；旧数据/加载完成前的占位都用它兜底。 */
export const DEFAULT_ROUTING_TUNING: RoutingTuning = {
  firstByteTimeoutSecs: 60,
  idleTimeoutSecs: 120,
  failureThreshold: 4,
  cooldownSecs: 60,
};

/** 本地路由配置（Claude Code / Codex，ADR-022）。 */
export interface RoutingConfig {
  enabled: boolean;
  /** 当前供应商失败时依次尝试的备用中转站 id。 */
  failoverQueue: string[];
  tuning: RoutingTuning;
  /** 候选排序：failover = 按顺序；quota-aware = 额度已用尽的后置。 */
  strategy: RoutingStrategy;
}

export type RoutingStrategy = "failover" | "quota-aware";

/** 路由候选的健康状态。 */
export interface RouteCandidateStatus {
  providerId: string;
  name: string;
  /** 是否为当前供应商（候选首位）。 */
  primary: boolean;
  state: "closed" | "open" | "half-open";
  consecutiveFailures: number;
  /** 熔断剩余秒数（非熔断为 null）。 */
  openRemainingSecs: number | null;
  lastError: string | null;
  lastFailureAt: number | null;
  /** 本次运行以来成功响应 / 失败的累计次数。 */
  served: number;
  failed: number;
  /** 本次运行以来经它完成的请求与用量。 */
  totals: ProviderTotals;
  /** 额度优先策略下因额度用尽被后置时的预计恢复剩余秒数（否则为 null）。 */
  quotaExhaustedSecs: number | null;
}

export interface ProviderTotals {
  requests: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
}

/** 路由请求日志的一条。 */
export interface RouteRequestRecord {
  id: number;
  /** Unix 毫秒。 */
  at: number;
  app: ClientApp;
  model: string | null;
  stream: boolean;
  providerId: string | null;
  providerName: string | null;
  /** 最终尝试之前的经过（故障转移 / 整流重试）。 */
  attempts: { providerId: string; providerName: string; outcome: string }[];
  status: number | null;
  firstByteMs: number | null;
  durationMs: number | null;
  usage: { inputTokens: number; outputTokens: number; cacheReadTokens: number } | null;
  error: string | null;
}

export interface AppRouteStatus {
  app: ClientApp;
  /** 最近一次成功服务请求的供应商 id。 */
  lastServedBy: string | null;
  /** 主站失败后由备用临时接替。 */
  sticky: { providerId: string; remainingSecs: number } | null;
  candidates: RouteCandidateStatus[];
}

/** 配额窗口（five-hour / weekly / monthly）。 */
export interface QuotaTier {
  window: "five-hour" | "weekly" | "monthly";
  /** 已用百分比 0-100。 */
  utilization: number;
  /** 重置时间（Unix 毫秒）。 */
  resetsAt: number | null;
}

/** 账户余额（DeepSeek/StepFun/SiliconFlow/OpenRouter/Novita 等按余额计费的中转站）。 */
export interface Balance {
  amount: number;
  currency: string;
}

/** Provider 用量查询结果（官方订阅 / Coding Plan 用 tiers，余额制中转站用 balances）。 */
export interface ProviderQuota {
  success: boolean;
  tiers: QuotaTier[];
  balances: Balance[];
  planLabel: string | null;
  error: string | null;
}

/** 支持用量查询的 base_url 识别（与 Rust detect_coding_plan 同规则）。 */
export function supportsQuota(baseUrl: string | null): boolean {
  if (!baseUrl) return false;
  const url = baseUrl.toLowerCase();
  return (
    url.includes("api.kimi.com/coding") ||
    url.includes("bigmodel.cn") ||
    url.includes("api.z.ai") ||
    url.includes("api.minimaxi.com") ||
    url.includes("api.minimax.io")
  );
}

/** 单个端点的测速结果（对齐 cc-switch EndpointLatency）。 */
export interface EndpointLatency {
  url: string;
  status: number | null;
  latencyMs: number | null;
  error: string | null;
}

/** 切换结果：written=false 表示 no-op；warnings 为非阻断性提示。 */
export interface SwitchOutcome {
  written: boolean;
  warnings: string[];
}

/** `mcpane://import/provider` 深链解析出的待确认导入（不含 Key 明文）。 */
export interface ImportLinkPreview {
  app: ClientApp;
  name: string;
  baseUrl: string;
  /** 目标主机名，界面突出显示。 */
  host: string;
  model: string | null;
  /** 链接带了 Key 时的掩码；null = 链接没带。 */
  keyHint: string | null;
}

/** 深链导入的落库结果：imported 成功；skipped 已有同地址供应商；failed 校验失败。 */
export interface ImportLinkOutcome {
  imported: string[];
  skipped: string[];
  failed: string[];
}

/** 从影子条目 id（`unified-<app>-<uid>`）提取统一供应商 uid；非影子返回 null。 */
export function unifiedIdOfShadow(shadowId: string): string | null {
  const match = /^unified-(?:claude|codex)-(.+)$/.exec(shadowId);
  return match?.[1] ?? null;
}

/** 统一供应商（跨 app 共享的网关配置，如 NewAPI；保存即同步影子条目）。 */
export interface UnifiedProvider {
  schemaVersion: number;
  id: string;
  name: string;
  baseUrl: string;
  apps: { claude: boolean; codex: boolean };
  /** Claude 兜底模型与角色映射。 */
  model: string | null;
  modelHaiku: string | null;
  modelSonnet: string | null;
  modelOpus: string | null;
  modelFable: string | null;
  /** Codex 顶层 model。 */
  codexModel: string | null;
  apiKeyRef: { account: string } | null;
  notes: string | null;
  websiteUrl: string | null;
  sortOrder: number;
}

/**
 * 一张按时间增长的表的保留窗口（设置页数据分区）。
 * `retainDays` 由后端回传——保留期的事实源在 Rust（commands/data.rs 的常量，
 * 启动自动清理用的是同一个值），前端不再各写一份魔数。
 */
export interface RetentionView {
  rows: number;
  /** 最早一条的时间戳（秒）；表为空时 null。 */
  oldestTs: number | null;
  /** 早于保留期的条数（= 此刻点清理会删掉的数量）。 */
  prunable: number;
  retainDays: number;
}

export interface DataOverview {
  /** mcpane.db 连同 WAL/SHM 边车的磁盘占用（字节）。 */
  dbBytes: number;
  logs: RetentionView;
  usage: RetentionView;
}

// ---------- 应用自身（commands/app.rs、commands/updater.rs） ----------

export interface AppInfo {
  dataDir: string;
  dbPath: string;
  hubEndpoint: string;
  appVersion: string;
}

/** 更新检查结果；同时是 `mcpane://update-available` 事件的载荷。 */
export interface UpdateStatus {
  available: boolean;
  currentVersion: string;
  version: string | null;
}

// ---------- MCP Server 管理（对应 src-tauri commands/servers.rs） ----------

export type ServerTransport = "stdio" | "http" | "sse";

/** 下游 MCP Server 定义（servers_list 返回项，字段对应 Rust McpServerDef）。 */
export interface ServerDef {
  id: string;
  slug: string;
  name: string;
  transport: ServerTransport;
  command: string | null;
  args: string[];
  env: Record<string, string>;
  cwd: string | null;
  url: string | null;
  bearer: string | null;
  bearerEnv: string | null;
  headers: Record<string, string>;
  runtimePolicy: string;
  enabled: boolean;
}

/** server_upsert / server_test 的入参（对应 Rust ServerInput）。 */
export interface ServerUpsertInput {
  /** null/缺省 = 新建；非空 = 编辑（保留原 slug/enabled）。 */
  id?: string | null;
  name: string;
  transport: ServerTransport;
  command: string | null;
  args: string[];
  env: Record<string, string>;
  cwd: string | null;
  url: string | null;
  bearer?: string | null;
  bearerEnv?: string | null;
  headers?: Record<string, string>;
  /** null = 沿用现状（编辑保留原值、新建默认启用）。 */
  enabled?: boolean | null;
}

/** servers_import_json / import_from_clients 的导入结果。 */
export interface ServerImportSummary {
  imported: string[];
  skipped: string[];
  /** 非致命异常：某份客户端配置读取/解析失败等（Hub 重载在后端后台执行，不进此通道）。 */
  warnings: string[];
  /** 已入库但仍有 `${VAR}` 占位符没填、按停用导入的服务器（只有 JSON 导入会产生）。 */
  incomplete: string[];
}

/** servers_import_preview：导入面板逐条确认用，不落库。 */
export interface ServerImportPreview {
  entries: ServerImportPreviewEntry[];
  /** 既无 command 也无 url、认不出传输方式的条目名。 */
  invalid: string[];
}

export interface ServerImportPreviewEntry {
  /** 解析结果（id 为空）；条目名即 server.name。 */
  server: ServerDef;
  /** 已占用同一工具前缀的服务器名；null = 可导入。 */
  conflict: string | null;
  /** 待补填的 `${VAR}` 占位符变量名。 */
  variables: string[];
}

/** servers_export：凭据已替换为 `${VAR}` 占位符的导出内容（ADR-027）。 */
export interface ServerExport {
  json: string;
  redactions: ServerRedaction[];
}

export interface ServerRedaction {
  server: string;
  field: "bearer" | "env" | "headers" | "url" | "args";
  variable: string;
}

/** cc-switch 迁移：检测结果。found 为 null 表示这台机器上没装（或装在别处）。 */
export interface CcSwitchDetection {
  /** 查找过的目录，没找到时展示给用户判断。 */
  searched: string;
  found: CcSwitchFound | null;
}

export interface CcSwitchFound {
  path: string;
  providerCount: number;
  serverCount: number;
  apps: { app: ClientApp; count: number }[];
}

/** ccswitch_import 的结果。三段分开：供应商、MCP 服务、读取阶段就搬不动的。 */
export interface CcSwitchImportSummary {
  providers: { imported: string[]; skipped: string[]; failed: string[] };
  servers: ServerImportSummary;
  warnings: string[];
  /** 并入故障转移队列的候选数（开启本地路由后生效）。 */
  failoverMigrated: number;
}

/** server_test 的连通性结果。 */
export interface ServerTestResult {
  ok: boolean;
  toolCount?: number;
  tools?: string[];
  elapsedMs?: number;
  error?: string;
}

// ---------- Workspace 与 Catalog（对应 src-tauri commands/workspace.rs） ----------

/** 正主在此，lib/workspace.tsx re-export 以维持既有引用路径。 */
export interface Workspace {
  id: string;
  name: string;
  projectId?: string | null;
  /** 工具动态发现模式（tools/list 只暴露 meta 工具，按需检索）。 */
  toolDiscovery?: boolean;
  /** 用户自定义使用准则（注入 instructions，空串 = 未配置，ADR-020）。 */
  usageGuidance?: string;
}

/** 下游申报的图标（MCP Icon 对象，原样透传）。 */
export interface ToolIcon {
  src: string;
  mimeType?: string | null;
  sizes?: string[] | null;
  theme?: "light" | "dark" | null;
}

/**
 * MCP ToolAnnotations 的四个行为提示。
 *
 * 下游不申报 annotations 时 Codex `exec`（非交互）会直接取消工具调用
 * （openai/codex#16685），用户可为这类工具自己声明一份。
 */
export interface DeclaredAnnotations {
  readOnlyHint: boolean;
  destructiveHint: boolean;
  idempotentHint: boolean;
  openWorldHint: boolean;
}

/** 字段对应 crates/mcpane-hub-catalog 的 CatalogTool（camelCase 序列化）。 */
export interface CatalogTool {
  serverId: string;
  serverSlug: string;
  originalName: string;
  exposedName: string;
  description: string | null;
  icons?: ToolIcon[] | null;
  schemaHash: string;
  /** 下游 Server 自己申报的注解；null = 没申报（Hub 不代填）。 */
  annotations?: Record<string, unknown> | null;
  /** 用户声明的注解（全局，非按工作区）；仅在 annotations 为 null 时才会被下发。 */
  declaredAnnotations?: DeclaredAnnotations | null;
}

/** `raw` 是下游 Prompt 的原样透传，这里只声明界面用得到的字段。 */
export interface CatalogPrompt {
  serverId: string;
  serverSlug: string;
  originalName: string;
  exposedName: string;
  raw: { description?: string | null };
}

export interface CatalogResource {
  serverId: string;
  serverSlug: string;
  uri: string;
  raw: { name?: string | null };
}

/** 某 Workspace 的 Catalog 快照（Hub CatalogRevision 的可序列化部分）。 */
export interface Catalog {
  id: string;
  workspaceId: string;
  tools: CatalogTool[];
  prompts: CatalogPrompt[];
  resources: CatalogResource[];
}

/** Tool / Prompt / Resource 的按工作区覆盖项（禁用与别名）。 */
export interface ToolOverride {
  serverId: string;
  originalName: string;
  enabled: boolean;
  alias: string | null;
  /** 已保存的触发提示；null = 未配置。工具行的编辑框靠它回显。 */
  descriptionSuffix: string | null;
}

/**
 * 声明/撤回某工具注解的入参。`annotations` 传 null = 撤回声明。
 *
 * 没有 workspaceId：注解陈述的是工具行为，对所有工作区一起生效。
 */
export interface ToolAnnotationsInput {
  serverId: string;
  originalName: string;
  annotations: DeclaredAnnotations | null;
}

/**
 * 工具开关入参。`serverId` 可空是照实建模：编辑弹窗在组件顶层构造这个对象，
 * 此时尚无选中的 Server，只有真正点开关时才必定有值。
 */
export interface ToolToggleInput {
  /** null = 默认工作区。 */
  workspaceId: string | null;
  serverId: string | undefined;
  originalName: string;
  enabled: boolean;
  alias?: string | null;
  /** 追加到工具描述后的触发式提示（告诉模型「何时该用」，ADR-020 闸③）。 */
  descriptionSuffix?: string | null;
}

export interface ItemToggleInput extends ToolToggleInput {
  kind: "prompt" | "resource";
}

/**
 * 使用准则的投递目标（对应 src-tauri commands/guidance.rs）。
 * 不支持的客户端照样列出并带原因——静默隐藏会让用户以为 MCPane 漏了它。
 */
export interface GuidanceTarget {
  clientType: string;
  label: string;
  /** 规则文件绝对路径；不支持时为 null。 */
  path: string | null;
  supported: boolean;
  delivered: boolean;
  /** 区块内容与当前工作区的准则不一致（多工作区时常见）。 */
  stale: boolean;
  unsupportedReason: string | null;
  /** 手动投递的操作步骤（可自动投递的目标为空数组）。 */
  steps: string[];
  /** 落点是否经过实测；false 时 UI 必须提示「路径按文档推定，未验证客户端是否读它」。 */
  pathVerified: boolean;
  /** 读取该文件时的错误（如托管区块残缺），有值时应拦住投递。 */
  error: string | null;
}

// ---------- 请求日志与统计（对应 src-tauri commands/logs.rs） ----------

export interface LogEntry {
  id: string;
  ts: number;
  requestId: string;
  clientConnectionId: string | null;
  sessionId: string | null;
  workspaceId: string | null;
  serverId: string | null;
  exposedName: string | null;
  method: string;
  durationMs: number;
  success: boolean;
  errorCategory: string | null;
  requestSize: number | null;
  responseSize: number | null;
  retryCount: number;
  requestSummary?: string | null;
  responseSummary?: string | null;
  errorMessage?: string | null;
}

export interface LogGroupCount {
  key: string;
  count: number;
  errors: number;
}

export interface LogStats {
  total: number;
  success: number;
  avgDurationMs: number;
  p95DurationMs: number;
  byServer: LogGroupCount[];
  byMethod: LogGroupCount[];
  /** 按工具暴露名的调用量：哪些工具真被用起来了。 */
  byTool: LogGroupCount[];
  /** 按客户端连接的调用量：横向比各客户端的调用密度。 */
  byConnection: LogGroupCount[];
}

// ---------- 用量（与 mcpane-storage usage.rs 的 Serialize 对齐） ----------

export interface UsageSummary {
  events: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number;
  unpricedEvents: number;
}

/** models.dev 定价同步状态（对应 Rust PricingSyncState）。 */
export interface PricingSyncState {
  enabled: boolean;
  lastSyncedAt: number | null;
  lastError: string | null;
  lastWritten: number | null;
}

export interface UsageGroupRow {
  key: string;
  displayName: string | null;
  events: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number;
  /** 命中官方定价或有自定义价格；false 时成本记 0，界面标「无定价」。 */
  priced: boolean;
  /** 仅按模型聚合时有意义：正在使用用户自定义价格。 */
  overridden: boolean;
}

/** 一个模型的四项价格（USD / 百万 token）。 */
export interface ModelPrices {
  input: number;
  output: number;
  cacheRead: number;
  cacheCreation: number;
}

/** 「设置价格」弹窗要的两份价格。 */
export interface ModelPriceInfo {
  official: ModelPrices | null;
  custom: ModelPrices | null;
}

export interface UsageTrendPoint {
  bucketTs: number;
  events: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
  costUsd: number;
}

export interface UsageSyncResult {
  imported: number;
  filesScanned: number;
  errors: string[];
}

// ---------- 备份与云同步（对应 src-tauri commands/sync.rs） ----------

/** Hermes v12+ providers 字典里的外部供应商（Web UI 托管，只读呈现）。 */
export interface HermesExternalProvider {
  name: string;
  baseUrl: string | null;
}

/** 数据库初始化失败详情（对应 Rust InitErrorInfo；null = 初始化正常）。 */
export interface InitErrorInfo {
  kind: "version-too-new" | "corrupt" | "locked" | "other";
  message: string;
  dbPath: string;
  backups: BackupEntry[];
}

export interface BackupEntry {
  fileName: string;
  sizeBytes: number;
  createdAt: number;
}

export interface SyncStatus {
  lastSyncAt: number | null;
  lastSnapshotId: string | null;
  lastDeviceName: string | null;
  lastError: string | null;
  lastErrorSource: string | null;
}

export interface SyncSettingsView {
  /** 线上传回来的原始值；调用方收敛后才是三选一。 */
  provider: string;
  autoSync: boolean;
  webdav: { baseUrl: string; username: string; remoteRoot: string; hasPassword: boolean };
  s3: {
    region: string;
    bucket: string;
    accessKeyId: string;
    endpoint: string;
    remoteRoot: string;
    hasSecretKey: boolean;
  };
  status: SyncStatus;
}

/** 写入侧与读取侧形状不同：密码/密钥留 null 表示沿用已存凭据。 */
export interface SyncSettingsInput {
  provider: string;
  autoSync: boolean;
  webdav: { baseUrl: string; username: string; remoteRoot: string; password: string | null };
  s3: {
    region: string;
    bucket: string;
    accessKeyId: string;
    endpoint: string;
    remoteRoot: string;
    secretKey: string | null;
  };
}

export interface RemoteInfo {
  exists: boolean;
  deviceName: string | null;
  createdAt: number | null;
  snapshotId: string | null;
  incompatible: string | null;
}

export interface SyncOutcome {
  snapshotId: string;
  totalBytes: number;
  deviceName: string;
  createdAt: number;
}

export interface ConflictInfo {
  deviceName: string | null;
  createdAt: number | null;
  snapshotId: string | null;
}

export interface UploadOutcome {
  outcome: SyncOutcome | null;
  conflict: ConflictInfo | null;
}

// ---------- 配置操作历史（对应 src-tauri commands/history.rs） ----------

export interface OperationRecord {
  id: string;
  createdUnixSecs: number;
  targetPath: string;
  status: "applied" | "rolled-back" | "failed-recovered";
  hashBefore: string;
  hashAfter: string | null;
  /** no-op 操作没有快照（此类记录不会入历史，防御性可空）。 */
  snapshot: { id: string; sourcePath: string; storedAt: string } | null;
  changedPaths: string[];
  description: string;
}

/** 「按地址查找」的候选（ADR-025）：字段与 `mcpane-registry` 的序列化一一对应。 */
export interface DiscoverField {
  name: string;
  /** 固定值 / 默认值；空串表示要用户自己填。 */
  value: string;
  secret: boolean;
  required: boolean;
  description: string | null;
}

export type DiscoverInstall =
  | {
      kind: "stdio";
      runtime: "node" | "uv";
      command: string;
      args: string[];
      env: DiscoverField[];
    }
  | { kind: "remote"; transport: "http" | "sse"; url: string; headers: DiscoverField[] }
  | { kind: "unsupported"; reason: string };

export interface DiscoverCandidate {
  id: string;
  /** registry = 官方 Registry 登记；direct = 按粘贴内容字面生成、未经登记。 */
  source: "registry" | "direct";
  name: string;
  title: string | null;
  description: string | null;
  namespace: string | null;
  repositoryUrl: string | null;
  websiteUrl: string | null;
  /** 与粘贴的地址一致。 */
  exact: boolean;
  install: DiscoverInstall;
  todo: string[];
}

export type DiscoverNotice = "repositoryNotListed" | "directCandidate" | "partialFailure";

export interface DiscoverResult {
  candidates: DiscoverCandidate[];
  notices: DiscoverNotice[];
}

export const ipc = {
  serversList: () => invoke<ServerDef[]>("servers_list"),
  serverUpsert: (input: ServerUpsertInput) => invoke<void>("server_upsert", { input }),
  /** 测试一份配置能否连通（不落库，经 Hub 以真实运行环境解析执行）。 */
  serverTest: (input: ServerUpsertInput) => invoke<ServerTestResult>("server_test", { input }),
  serverSetEnabled: (id: string, enabled: boolean) =>
    invoke<void>("server_set_enabled", { id, enabled }),
  serverDelete: (id: string) => invoke<void>("server_delete", { id }),
  /** 按地址查找 MCP 服务器：GitHub 仓库 / npm / PyPI / 远程 URL → 官方 Registry 候选（只读，ADR-025）。 */
  serverDiscover: (query: string) => invoke<DiscoverResult>("server_discover", { query }),
  /** 远程 MCP 的 OAuth 登录：打开系统浏览器授权，成功即 Hub 已拿到令牌并重连（ADR-024）。 */
  serverOauthLogin: (id: string) => invoke<unknown>("server_oauth_login", { id }),
  serverOauthCancel: (id: string) => invoke<void>("server_oauth_cancel", { id }),
  serverOauthLogout: (id: string) => invoke<void>("server_oauth_logout", { id }),
  serversImportPreview: (json: string) =>
    invoke<ServerImportPreview>("servers_import_preview", { json }),
  /** 导入预览里勾选的条目；values 按条目名给出占位符取值，没填全的按停用入库。 */
  serversImportJson: (
    json: string,
    selected: string[],
    values: Record<string, Record<string, string>>,
  ) => invoke<ServerImportSummary>("servers_import_json", { json, selected, values }),
  serversExport: (ids: string[]) => invoke<ServerExport>("servers_export", { ids }),
  /** 写入「下载」文件夹并在文件管理器中定位，返回文件路径。 */
  serversExportSave: (ids: string[]) => invoke<string>("servers_export_save", { ids }),
  importFromClients: () => invoke<ServerImportSummary>("import_from_clients"),
  importLinkPeek: () => invoke<ImportLinkPreview | null>("import_link_peek"),
  importLinkConfirm: (apiKey: string | null) =>
    invoke<ImportLinkOutcome>("import_link_confirm", { apiKey }),
  importLinkDiscard: () => invoke<void>("import_link_discard"),
  ccswitchDetect: () => invoke<CcSwitchDetection>("ccswitch_detect"),
  ccswitchImport: (providers: boolean, servers: boolean) =>
    invoke<CcSwitchImportSummary>("ccswitch_import", { providers, servers }),
  detectClients: () => invoke<ClientsStatus>("detect_clients"),
  dataOverview: () => invoke<DataOverview>("data_overview"),
  mcpClientsStatus: () => invoke<McpClientStatus[]>("mcp_clients_status"),
  customMcpClientsList: () => invoke<CustomMcpClient[]>("custom_mcp_clients_list"),
  customMcpClientUpsert: (id: string, display: string) =>
    invoke<void>("custom_mcp_client_upsert", { id, display }),
  customMcpClientRemove: (id: string) => invoke<void>("custom_mcp_client_remove", { id }),
  clientConfigSnippet: (client: string) => invoke<string>("client_config_snippet", { client }),
  hubStatus: () => invoke<HubStatus>("hub_status"),
  hubDiagnostics: () => invoke<Record<string, unknown>>("hub_diagnostics"),
  hubProcessState: () => invoke<HubProcessState>("hub_process_state"),
  hubEnabledGet: () => invoke<boolean>("hub_enabled_get"),
  hubSetEnabled: (enabled: boolean) => invoke<void>("hub_set_enabled", { enabled }),
  autostartGet: () => invoke<boolean>("autostart_get"),
  /** 返回写入后实测的系统状态——可能与入参不同（被安全策略挡下）。 */
  autostartSet: (enabled: boolean) => invoke<boolean>("autostart_set", { enabled }),
  proxyGet: () => invoke<ProxyConfig>("proxy_get"),
  proxySet: (config: ProxyConfig) => invoke<void>("proxy_set", { config }),
  proxyScan: () => invoke<DetectedProxy[]>("proxy_scan"),
  proxyTest: (url: string) => invoke<ProxyTestResult>("proxy_test", { url }),
  connectionCreate: (name: string, clientType: string) =>
    invoke<{ ok: boolean; connection?: unknown; token?: string }>("connection_create", {
      name,
      clientType,
    }),
  /** 轮换 Token 并联动同步已接入的客户端配置（ADR-015）。 */
  connectionRotateSync: (clientConnectionId: string, clientType: string) =>
    invoke<RotateSyncOutcome>("connection_rotate_sync", { clientConnectionId, clientType }),
  connectionSetEnabled: (clientConnectionId: string, enabled: boolean) =>
    invoke<{ ok: boolean }>("connection_set_enabled", { clientConnectionId, enabled }),
  /** 永久删除凭证（Token 立即失效且不可恢复，区别于撤销的可逆开关）。 */
  connectionDelete: (clientConnectionId: string) =>
    invoke<{ ok: boolean }>("connection_delete", { clientConnectionId }),
  connectionSetWorkspace: (clientConnectionId: string, workspaceId: string | null) =>
    invoke<{ ok: boolean }>("connection_set_workspace", { clientConnectionId, workspaceId }),
  /** 工具暴露模式（ADR-020）：flat 平铺 / gateway 网关 / auto 按客户端自适应。 */
  connectionSetToolExposure: (clientConnectionId: string, mode: ToolExposureMode) =>
    invoke<void>("connection_set_tool_exposure", { clientConnectionId, mode }),
  /** 访问控制：某连接被拒绝访问的 Server ID 列表（deny-list，无行=全部允许）。 */
  connectionDeniedServers: (clientConnectionId: string) =>
    invoke<string[]>("connection_denied_servers", { clientConnectionId }),
  /** 访问控制：开关某连接对某 Server 的访问权（对活跃会话立即生效）。 */
  connectionServerAccessSet: (clientConnectionId: string, serverId: string, allowed: boolean) =>
    invoke<void>("connection_server_access_set", { clientConnectionId, serverId, allowed }),
  providersList: () => invoke<ProviderProfile[]>("providers_list"),
  providerCurrent: () => invoke<CurrentProviders>("provider_current"),
  /** 各客户端安装态（供应商页标灰页签用，轻量：不跑 --version）。 */
  providerClientsInstalled: () => invoke<ClientInstallStatus>("provider_clients_installed"),
  /** additive 客户端：从 live 配置移除（保留列表记录，可再启用）。 */
  providerRemoveFromLive: (id: string) => invoke<void>("provider_remove_from_live", { id }),
  /** additive 客户端在 live 配置里已启用的供应商 id。 */
  providerLiveIds: (app: ClientApp) => invoke<string[]>("provider_live_ids", { app }),
  providerHermesExternal: () => invoke<HermesExternalProvider[]>("provider_hermes_external"),
  /** 各客户端配置目录的用户覆盖（app → 绝对路径；未覆盖的 app 不出现）。 */
  providerConfigDirsGet: () => invoke<Record<string, string>>("provider_config_dirs_get"),
  /** 设置/清除配置目录覆盖（空串 = 回到默认位置）。 */
  providerConfigDirSet: (app: ClientApp, dir: string) =>
    invoke<void>("provider_config_dir_set", { app, dir }),
  /** 本地环境实况（设置页面板用，会跑 --version）。 */
  clientEnvStatus: () => invoke<ClientEnv[]>("client_env_status"),
  /** 官方项被删后重新加回来（幂等：已存在则返回存量）。 */
  providerOfficialEnsure: (app: ClientApp) =>
    invoke<ProviderProfile>("provider_official_ensure", { app }),
  providerAdd: (profile: ProviderProfile) => invoke<ProviderProfile>("provider_add", { profile }),
  providerUpdate: (profile: ProviderProfile) => invoke<void>("provider_update", { profile }),
  providerDelete: (id: string) => invoke<void>("provider_delete", { id }),
  providerSwitch: (app: ClientApp, id: string) =>
    invoke<SwitchOutcome>("provider_switch", { app, id }),
  providerSetApiKey: (id: string, apiKey: string) =>
    invoke<void>("provider_set_api_key", { id, apiKey }),
  providerTestConnectivity: (id: string) => invoke<number>("provider_test_connectivity", { id }),
  providerQuota: (id: string) => invoke<ProviderQuota>("provider_quota", { id }),
  /**
   * 拉取供应商可用模型列表。
   * 新增时传表单里的 apiKey；编辑时 Key 在 Keychain，改传 id 由后端物化。
   */
  providerFetchModels: (args: { baseUrl: string; apiKey?: string; id?: string }) =>
    invoke<string[]>("provider_fetch_models", args),
  /** live 配置（剔除受管键），作为配置片段编辑器的展示基线。 */
  providerLiveSettings: (app: ClientApp) =>
    invoke<Record<string, unknown>>("provider_live_settings", { app }),
  providerOfficialUpdate: (
    app: ClientApp,
    extraSettings: Record<string, unknown> | null,
    commonConfigEnabled: boolean,
  ) => invoke<void>("provider_official_update", { app, extraSettings, commonConfigEnabled }),
  providerCommonSnippet: (app: ClientApp) =>
    invoke<Record<string, unknown> | null>("provider_common_snippet", { app }),
  providerCommonSnippetSet: (app: ClientApp, snippet: Record<string, unknown> | null) =>
    invoke<void>("provider_common_snippet_set", { app, snippet }),
  providerTestEndpoints: (urls: string[]) =>
    invoke<EndpointLatency[]>("provider_test_endpoints", { urls }),
  providerUnifiedList: () => invoke<UnifiedProvider[]>("provider_unified_list"),
  providerUnifiedUpsert: (unified: UnifiedProvider) =>
    invoke<UnifiedProvider>("provider_unified_upsert", { unified }),
  providerUnifiedDelete: (id: string) => invoke<void>("provider_unified_delete", { id }),
  providerUnifiedSetApiKey: (id: string, apiKey: string) =>
    invoke<void>("provider_unified_set_api_key", { id, apiKey }),
  /** Claude Desktop 状态面（配置一致性检查 + 网关运行态）。 */
  claudeDesktopStatus: () => invoke<ClaudeDesktopStatus>("claude_desktop_status"),
  /** 从 Claude Code 供应商批量导入兼容的 Desktop 供应商；返回导入条数。 */
  claudeDesktopImportFromClaude: () => invoke<number>("claude_desktop_import_from_claude"),
  desktopProxyStatus: () => invoke<DesktopProxyStatus>("desktop_proxy_status"),
  desktopProxyStart: () => invoke<DesktopProxyStatus>("desktop_proxy_start"),
  desktopProxyStop: () => invoke<void>("desktop_proxy_stop"),
  desktopProxySetPort: (listenPort: number) =>
    invoke<void>("desktop_proxy_set_port", { listenPort }),
  providerRoutingGet: (app: ClientApp) => invoke<RoutingConfig>("provider_routing_get", { app }),
  /** 保存路由配置并立即重写当前项 live、同步路由服务。 */
  providerRoutingSet: (app: ClientApp, config: RoutingConfig) =>
    invoke<RoutingConfig>("provider_routing_set", { app, config }),
  routerStatus: () => invoke<AppRouteStatus[]>("router_status"),
  routerRecentRequests: (app: ClientApp, limit = 50) =>
    invoke<RouteRequestRecord[]>("router_recent_requests", { app, limit }),
  routerResetBreaker: (app: ClientApp, providerId: string) =>
    invoke<void>("router_reset_breaker", { app, providerId }),
  /** 清空请求日志记录（各供应商累计用量不受影响）；省略 app = 清全部。 */
  routerClearRequests: (app?: ClientApp) => invoke<void>("router_clear_requests", { app }),
  /** 系统浏览器打开外部链接（后端只放行 http/https）。 */
  openExternal: (url: string) => invoke<void>("open_external", { url }),
  appInfo: () => invoke<AppInfo>("app_info"),
  updateCheck: () => invoke<UpdateStatus>("update_check"),
  updateInstall: () => invoke<void>("update_install"),
  /** 重启应用让已下载的更新生效（不返回——进程随即退出）。 */
  updateRestart: () => invoke<void>("update_restart"),
  initStatus: () => invoke<InitErrorInfo | null>("init_status"),
  initReplaceDbFromBackup: (fileName: string) =>
    invoke<void>("init_replace_db_from_backup", { fileName }),
  autoUpdateCheckGet: () => invoke<boolean>("auto_update_check_get"),
  autoUpdateCheckSet: (enabled: boolean) => invoke<void>("auto_update_check_set", { enabled }),
  /** 通用 MCP 客户端（Cursor/VSCode/Cline…）的接入三件套；Claude/Codex 走专用命令。 */
  mcpClientConnectPreview: (client: string, mode: CredentialMode) =>
    invoke<ConnectPreview>("mcp_client_connect_preview", { client, mode }),
  mcpClientConnectApply: (client: string, mode: CredentialMode) =>
    invoke<ConnectOutcome>("mcp_client_connect_apply", { client, mode }),
  /** 解绑成功后的非致命警告（如环境变量清理失败，Token 仍留在 shell 配置里）。 */
  mcpClientDisconnect: (client: string) => invoke<string[]>("mcp_client_disconnect", { client }),
  /** 首帧已提交：后端据此把启动时隐藏的主窗口亮出来。 */
  windowReady: () => invoke<void>("window_ready"),
  workspacesList: () => invoke<Workspace[]>("workspaces_list"),
  workspaceCreate: (name: string) => invoke<Workspace>("workspace_create", { name }),
  /** workspaceId 传 null = 默认工作区（聚合全部启用的 Server）。 */
  catalogList: (workspaceId: string | null) => invoke<Catalog>("catalog_list", { workspaceId }),
  workspaceServers: (workspaceId: string) => invoke<string[]>("workspace_servers", { workspaceId }),
  workspaceServerSet: (workspaceId: string, serverId: string, enabled: boolean) =>
    invoke<void>("workspace_server_set", { workspaceId, serverId, enabled }),
  workspaceToolOverrides: (workspaceId: string | null) =>
    invoke<ToolOverride[]>("workspace_tool_overrides", { workspaceId }),
  workspaceItemOverrides: (kind: "prompt" | "resource", workspaceId: string | null) =>
    invoke<ToolOverride[]>("workspace_item_overrides", { kind, workspaceId }),
  workspaceToolDiscovery: (workspaceId: string | null) =>
    invoke<boolean>("workspace_tool_discovery", { workspaceId }),
  workspaceToolDiscoverySet: (workspaceId: string | null, enabled: boolean) =>
    invoke<void>("workspace_tool_discovery_set", { workspaceId, enabled }),
  /** 工作区使用准则：注入 instructions 让所有客户端共享（ADR-020 主动性轴）。 */
  workspaceUsageGuidance: (workspaceId: string | null) =>
    invoke<string>("workspace_usage_guidance", { workspaceId }),
  workspaceUsageGuidanceSet: (workspaceId: string | null, guidance: string) =>
    invoke<void>("workspace_usage_guidance_set", { workspaceId, guidance }),
  workspaceToolToggle: (input: ToolToggleInput) => invoke<void>("workspace_tool_toggle", { input }),
  guidanceTargets: (workspaceId: string | null) =>
    invoke<GuidanceTarget[]>("guidance_targets", { workspaceId }),
  guidanceDeliver: (clientType: string, workspaceId: string | null) =>
    invoke<void>("guidance_deliver", { clientType, workspaceId }),
  guidanceRevoke: (clientType: string) => invoke<void>("guidance_revoke", { clientType }),
  guidancePreview: (workspaceId: string | null) =>
    invoke<string>("guidance_preview", { workspaceId }),
  toolAnnotationsSet: (input: ToolAnnotationsInput) =>
    invoke<void>("tool_annotations_set", { input }),
  workspaceItemToggle: (input: ItemToggleInput) => invoke<void>("workspace_item_toggle", { input }),
  logsRecent: (limit: number) => invoke<LogEntry[]>("logs_recent", { limit }),
  logsGet: (id: string) => invoke<LogEntry>("logs_get", { id }),
  logsStats: (hours: number) => invoke<LogStats>("logs_stats", { hours }),
  /** 返回删除条数。 */
  logsPrune: (days: number) => invoke<number>("logs_prune", { days }),
  usagePrune: (days: number) => invoke<number>("usage_prune", { days }),
  /** 增量解析本地会话文件，把用量事件落库。 */
  usageSync: () => invoke<UsageSyncResult>("usage_sync"),
  pricingSyncStatus: () => invoke<PricingSyncState>("pricing_sync_status"),
  pricingSyncSetEnabled: (enabled: boolean) =>
    invoke<PricingSyncState>("pricing_sync_set_enabled", { enabled }),
  pricingSyncRun: () => invoke<PricingSyncState>("pricing_sync_run"),
  usageSummary: (hours: number) => invoke<UsageSummary>("usage_summary", { hours }),
  usageTrend: (hours: number, tzOffsetMinutes: number) =>
    invoke<UsageTrendPoint[]>("usage_trend", { hours, tzOffsetMinutes }),
  usageByModel: (hours: number) => invoke<UsageGroupRow[]>("usage_by_model", { hours }),
  /** 某模型的官方价与自定义价。 */
  usagePriceInfo: (modelId: string) => invoke<ModelPriceInfo>("usage_price_info", { modelId }),
  /** 设置某模型的自定义价格；按模型生效、不区分供应商（ADR-026）。 */
  usagePriceOverrideSet: (modelId: string, prices: ModelPrices) =>
    invoke<void>("usage_price_override_set", { modelId, prices }),
  usagePriceOverrideClear: (modelId: string) =>
    invoke<void>("usage_price_override_clear", { modelId }),
  usageByProject: (hours: number) => invoke<UsageGroupRow[]>("usage_by_project", { hours }),
  usageByProvider: (hours: number) => invoke<UsageGroupRow[]>("usage_by_provider", { hours }),
  backupList: () => invoke<BackupEntry[]>("backup_list"),
  /** 返回新备份的文件名。 */
  backupCreate: () => invoke<string>("backup_create"),
  backupRestore: (fileName: string) => invoke<void>("backup_restore", { fileName }),
  backupDelete: (fileName: string) => invoke<void>("backup_delete", { fileName }),
  syncSettingsGet: () => invoke<SyncSettingsView>("sync_settings_get"),
  syncSettingsSet: (input: SyncSettingsInput) =>
    invoke<SyncSettingsView>("sync_settings_set", { input }),
  syncTest: () => invoke<void>("sync_test"),
  syncRemoteInfo: () => invoke<RemoteInfo>("sync_remote_info"),
  syncDownload: () => invoke<SyncOutcome>("sync_download"),
  /** force = 无视云端快照与本机基线不一致，强制覆盖。 */
  syncUpload: (force: boolean) => invoke<UploadOutcome>("sync_upload", { force }),
  configHistoryList: () => invoke<OperationRecord[]>("config_history_list"),
  /** 只传 id：快照与目标路径由后端从历史库回查（IPC 信任边界）。 */
  configRollback: (recordId: string) => invoke<void>("config_rollback", { recordId }),
  /** 取走即清空：窗口关着时积攒的外部改动路径。 */
  configExternalChanges: () => invoke<string[]>("config_external_changes"),
};

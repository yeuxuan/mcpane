/**
 * 客户端元数据的**单一事实源**。
 *
 * 之前这些信息散在七处 `Record<ClientApp, ...>`（供应商页签、设置页可见性、
 * 本地环境面板、编辑器标题、官方项 id、安装命令、默认可见性），加一个客户端
 * 要挨个补——TypeScript 能逮住其中大半，但 `BrandName` 与 `ClientApp` 是两个
 * 类型，图标那条编译器管不到，加 Grok 时就是靠报错一个个试出来的。
 *
 * 现在只改这一张表。数组顺序即展示序（页签、设置列表、本地环境都按它渲染）。
 */
import type { BrandName } from "../components/BrandIcon";
import type { ClientApp } from "./ipc";

export interface ClientMeta {
  app: ClientApp;
  /** 界面显示名（品牌专有名词，不翻译）。 */
  label: string;
  /** 品牌色；没有识别度鲜明的品牌色时用 currentColor 跟随文本。 */
  color: string;
  /**
   * 形态角标：仅在**多个客户端共用同一个品牌 logo** 时填（目前只有 Claude 系
   * 两项，Code 与 Desktop 都是 Anthropic 的 Claude 标）。纯图标页签下没有文案
   * 兜底，不加角标这两个格子就是一模一样的图形，只能靠 hover 猜。
   * 对齐 cc-switch `AppSwitcher` 的 `APP_BADGE_ICON`；其余客户端 logo 本身就
   * 唯一，全都标上 cli 反而是噪音。
   */
  surface?: "cli" | "gui";
  /** 官方项的固定 id（与后端 `OFFICIAL_*_ID` 同值）。 */
  officialId: string;
  /**
   * additive：多个供应商并存于同一份配置、由客户端自己选用，没有"当前生效项"，
   * 也没有官方订阅（这类客户端自己不提供模型）。UI 因此用「启用 / 已启用」，
   * 并要求显式声明模型列表。
   */
  additive: boolean;
  /**
   * 该客户端的配置里是否有"上游协议"这一维度。三家 additive 都有，只是字段名
   * 与取值各不相同——OpenCode 是 AI SDK 包名（`npm`）、OpenClaw 是 `api`、
   * Hermes 是 `api_mode`；后端 adapter 各自映射。
   */
  supportsUpstreamApi?: boolean;
  /**
   * 配置里的模型条目是否带"上下文长度"。Hermes 是 `models.<id>.context_length`、
   * OpenClaw 是 `models[].contextWindow`；别家给了这个输入也落不到任何字段上。
   */
  supportsContextLength?: boolean;
  /** 未安装时的引导：可复制的官方安装命令（GUI 应用没有）+ 官网。 */
  install: { command?: string; url: string };
}

/** 顺序即展示序：Claude 系相邻，其余按接入时间。 */
export const CLIENTS: ClientMeta[] = [
  {
    app: "claude",
    label: "Claude Code",
    color: "#D97757",
    surface: "cli",
    officialId: "official-claude",
    additive: false,
    install: {
      command: "npm i -g @anthropic-ai/claude-code@latest",
      url: "https://claude.com/product/claude-code",
    },
  },
  {
    app: "claude-desktop",
    label: "Claude Desktop",
    color: "#D97757",
    surface: "gui",
    officialId: "official-claude-desktop",
    additive: false,
    install: { url: "https://claude.ai/download" },
  },
  {
    app: "codex",
    label: "Codex",
    color: "currentColor",
    officialId: "official-codex",
    additive: false,
    install: {
      command: "npm i -g @openai/codex@latest",
      url: "https://developers.openai.com/codex/cli",
    },
  },
  {
    app: "gemini",
    label: "Gemini CLI",
    color: "#3186FF",
    officialId: "official-gemini",
    additive: false,
    install: {
      command: "npm i -g @google/gemini-cli@latest",
      url: "https://github.com/google-gemini/gemini-cli",
    },
  },
  {
    app: "grok",
    label: "Grok CLI",
    color: "currentColor",
    officialId: "official-grok",
    additive: false,
    install: {
      command: "npm i -g @xai-official/grok@latest",
      url: "https://x.ai/grok",
    },
  },
  {
    app: "opencode",
    label: "OpenCode",
    color: "currentColor",
    // 占位：additive 客户端没有官方形态，后端 ensure_official 会直接拒绝。
    officialId: "official-opencode",
    additive: true,
    supportsUpstreamApi: true,
    install: {
      command: "npm i -g opencode-ai@latest",
      url: "https://opencode.ai",
    },
  },
  {
    app: "openclaw",
    label: "OpenClaw",
    color: "currentColor",
    officialId: "official-openclaw",
    additive: true,
    supportsUpstreamApi: true,
    supportsContextLength: true,
    install: {
      command: "npm i -g openclaw@latest",
      url: "https://openclaw.ai",
    },
  },
  {
    app: "hermes",
    label: "Hermes",
    color: "currentColor",
    officialId: "official-hermes",
    additive: true,
    supportsUpstreamApi: true,
    supportsContextLength: true,
    install: {
      // Nous Research 的 hermes-agent：走官方安装脚本（PyPI 包，不是 npm）。
      command:
        "curl -fsSL https://raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.sh | bash",
      url: "https://github.com/NousResearch/hermes-agent",
    },
  },
];

export const CLIENT_META: Record<ClientApp, ClientMeta> = Object.fromEntries(
  CLIENTS.map((meta) => [meta.app, meta]),
) as Record<ClientApp, ClientMeta>;

/** 供应商页可切换的客户端（顺序同 CLIENTS）。 */
export const PROVIDER_APPS: ClientApp[] = CLIENTS.map((meta) => meta.app);

/** 每个 ClientApp 都有同名品牌图标（BrandIcon 的 BRAND_ART 覆盖了全集）。 */
export const brandOf = (app: ClientApp): BrandName => app;

export const isAdditiveApp = (app: ClientApp) => CLIENT_META[app].additive;

/** 该客户端是否存在"官方订阅"这一形态（additive 客户端没有）。 */
export const hasOfficial = (app: ClientApp) => !CLIENT_META[app].additive;

/** 支持本地路由接管的客户端（与后端 `ClientApp::supports_routing` 一致，ADR-022）。 */
export const ROUTING_APPS = ["claude", "codex", "gemini", "grok"] as const;
export type RoutingApp = (typeof ROUTING_APPS)[number];

export function isRoutingApp(app: string): app is RoutingApp {
  return (ROUTING_APPS as readonly string[]).includes(app);
}

/**
 * 形态角标取值：只有共用同一品牌 logo 的客户端才有（见 `ClientMeta.surface`）。
 * 入参也接受客户端页的 MCP 客户端 id——Cursor / VS Code 这些不在表里，
 * 返回 undefined 即"不标角标"，正是想要的行为。
 */
export const surfaceOf = (id: string): ClientMeta["surface"] =>
  CLIENTS.find((meta) => meta.app === id)?.surface;

/**
 * 按 Hub 连接上报的 clientType 取显示名。Claude Code / Codex 走专用探测、没有
 * `McpClientStatus` 行；其余（含自定义客户端，其 clientType 就是自定义 id）从
 * `mcp_clients_status` 的 display 取；都查不到退回原始 id，不留空白。
 */
export function clientDisplayName(
  clientType: string,
  mcpClients: ReadonlyArray<{ id: string; display: string }> | undefined,
): string {
  if (clientType === "claude") return "Claude Code";
  if (clientType === "codex") return "Codex";
  const known = mcpClients?.find((c) => c.id === clientType);
  if (known) return known.display;
  return (CLIENT_META as Partial<Record<string, ClientMeta>>)[clientType]?.label ?? clientType;
}

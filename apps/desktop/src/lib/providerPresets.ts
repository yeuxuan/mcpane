import type { ClientApp, DesktopMode, DesktopRoute, UpstreamApi } from "./ipc";

/**
 * 中转站预设（对齐 cc-switch 的 provider presets：base_url / 默认模型 /
 * 附加 env 逐项取自其源码，去除推广/邀请码等跟踪参数）。
 *
 * 收录原则：只收**模型厂商官方端点**与**中立知名聚合平台**。
 * cc-switch 里 isPartner / partnerPromotionKey 标记的商业推广中转站
 *（CCSub / PackyCode / Code0 等付费 relay 站）一律不收录。
 *
 * 其余未收录项（依赖 MCPane 没有的机制）：
 * - Gemini Native / OpenCode Go（Claude 侧）：需要 Anthropic ↔ OpenAI/Gemini
 *   格式转换，cc-switch 靠内置本地代理完成，MCPane 无代理；
 * - GitHub Copilot / Codex(ChatGPT 反代)：OAuth 反代供应商；
 * - AWS Bedrock ×2：走 AWS_* 凭据 env 模型，不符合 base_url + Key 二元模型；
 * - KAT-Coder：base_url 含 ${ENDPOINT_ID} 模板变量。
 *
 * Codex 侧与 cc-switch 的关键差异：cc-switch 对只有 Chat Completions 的厂商靠
 * 本地代理做 chat→responses 转换；MCPane 无代理，且 Codex 已移除 chat 协议，
 * 所以 Codex 预设只收原生支持 Responses 的端点（见 CODEX_PRESETS 注释）。
 */
export interface RelayPreset {
  id: string;
  name: string;
  baseUrl: string;
  /** 预设默认模型（Claude 写 ANTHROPIC_MODEL；Codex 写顶层 model）。 */
  model?: string;
  /** Claude 模型角色映射（写入 ANTHROPIC_DEFAULT_*_MODEL）。 */
  modelHaiku?: string;
  modelSonnet?: string;
  modelOpus?: string;
  modelFable?: string;
  /** Claude 认证形态（少数厂商要求 ANTHROPIC_API_KEY / x-api-key 头）。 */
  apiKeyField?: "auth-token" | "api-key";
  /** Claude 附加配置片段（settings.json 子集：env.API_TIMEOUT_MS 等，切走时清理）。 */
  extraSettings?: Record<string, unknown>;
  /**
   * 声明给 additive 客户端的**全部**模型（OpenCode / OpenClaw / Hermes）。
   * 这类客户端只显示显式声明过的模型，少声明 = 用户在客户端里选不到。
   * 缺省时回落到单值 `model`。
   */
  models?: string[];
  /**
   * additive 客户端的上游协议；缺省 OpenAI 兼容。
   *
   * 目前**没有预设携带它**，且有守卫测试锁住这一点：一份预设服务
   * OpenCode / OpenClaw / Hermes 三家，只有三家的正确取值一致时才能写进来
   * （详见 `OPENCODE_PRESETS` 上方注释里的 Kimi For Coding 反例）。字段留着
   * 是给"将来真出现三家一致的 anthropic 端点"用的，加时必须先改守卫。
   */
  upstreamApi?: UpstreamApi;
  /** additive 客户端的模型上下文窗口（Hermes / OpenClaw 用）。 */
  contextLength?: number;
  /** Codex 协议；chat = 只有 Chat Completions 端点，需开启本地路由转换（ADR-022）。 */
  wireApi?: "responses" | "chat";
  /** Codex URL query 参数（Azure 的 api-version 等）。 */
  queryParams?: Record<string, string>;
  /** Codex 1M 上下文窗口；缺省关闭。 */
  contextWindow1M?: boolean;
  /** 请求地址候选（测速/一键切换；缺省只有 baseUrl 本身）。 */
  endpointCandidates?: string[];
  /** 厂商官网。 */
  websiteUrl: string;
  /** 获取 API Key 的入口页；缺省用 websiteUrl。 */
  apiKeyUrl?: string;
  /** ProviderIcon 的图标 key；缺省按名称推断/首字母兜底。 */
  icon?: string;
  /** 预设按钮上标注「特别推荐」徽标。 */
  recommended?: boolean;
  /** Claude Desktop 写入模式（仅 claude-desktop 预设携带）。 */
  desktopMode?: DesktopMode;
  /** Claude Desktop 四档模型路由（仅 claude-desktop 预设携带）。 */
  desktopRoutes?: DesktopRoute[];
}

/** Codex 1M 窗口配套的自动压缩阈值默认值（窗口的 90%）。 */
export const CODEX_AUTO_COMPACT_LIMIT_1M = 900_000;

const CLAUDE_PRESETS: RelayPreset[] = [
  {
    // NewAPI 统一网关，Anthropic 兼容端点在 /v1/messages（Claude Code 自动追加）。
    id: "computetoken",
    name: "Compute Token",
    baseUrl: "https://computetoken.ai",
    model: "claude-sonnet-5",
    modelHaiku: "claude-haiku-4-5-20251001",
    modelSonnet: "claude-sonnet-5",
    modelOpus: "claude-opus-5",
    recommended: true,
    websiteUrl: "https://computetoken.ai",
    icon: "computetoken",
  },
  {
    id: "volcengine-agentplan",
    name: "火山 Agentplan",
    baseUrl: "https://ark.cn-beijing.volces.com/api/coding",
    model: "ark-code-latest",
    modelHaiku: "ark-code-latest",
    modelSonnet: "ark-code-latest",
    modelOpus: "ark-code-latest",
    websiteUrl: "https://www.volcengine.com/activity/codingplan",
    icon: "doubao",
  },
  {
    id: "byteplus",
    name: "BytePlus",
    baseUrl: "https://ark.ap-southeast.bytepluses.com/api/coding",
    model: "ark-code-latest",
    modelHaiku: "ark-code-latest",
    modelSonnet: "ark-code-latest",
    modelOpus: "ark-code-latest",
    websiteUrl: "https://www.byteplus.com/en/product/modelark",
  },
  {
    id: "doubao-seed",
    name: "DouBaoSeed",
    baseUrl: "https://ark.cn-beijing.volces.com/api/compatible",
    model: "doubao-seed-2-1-pro-260628",
    modelHaiku: "doubao-seed-2-1-pro-260628",
    modelSonnet: "doubao-seed-2-1-pro-260628",
    modelOpus: "doubao-seed-2-1-pro-260628",
    extraSettings: { env: { API_TIMEOUT_MS: "3000000" } },
    websiteUrl: "https://console.volcengine.com/ark",
    icon: "doubao",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    baseUrl: "https://api.deepseek.com/anthropic",
    model: "deepseek-v4-pro",
    modelHaiku: "deepseek-v4-flash",
    modelSonnet: "deepseek-v4-pro",
    modelOpus: "deepseek-v4-pro",
    websiteUrl: "https://platform.deepseek.com",
    apiKeyUrl: "https://platform.deepseek.com/api_keys",
    icon: "deepseek",
  },
  {
    id: "zhipu-glm",
    name: "Zhipu GLM",
    baseUrl: "https://open.bigmodel.cn/api/anthropic",
    model: "glm-5.1",
    modelHaiku: "glm-5.1",
    modelSonnet: "glm-5.1",
    modelOpus: "glm-5.1",
    websiteUrl: "https://open.bigmodel.cn",
    apiKeyUrl: "https://www.bigmodel.cn/claude-code",
    icon: "zhipu",
  },
  {
    id: "zhipu-glm-en",
    name: "Zhipu GLM en",
    baseUrl: "https://api.z.ai/api/anthropic",
    model: "glm-5.1",
    modelHaiku: "glm-5.1",
    modelSonnet: "glm-5.1",
    modelOpus: "glm-5.1",
    websiteUrl: "https://z.ai",
    apiKeyUrl: "https://z.ai/subscribe",
    icon: "zhipu",
  },
  {
    id: "qianfan",
    name: "Baidu Qianfan",
    baseUrl: "https://qianfan.baidubce.com/anthropic/coding",
    model: "qianfan-code-latest",
    modelHaiku: "qianfan-code-latest",
    modelSonnet: "qianfan-code-latest",
    modelOpus: "qianfan-code-latest",
    websiteUrl: "https://cloud.baidu.com/product/qianfan_modelbuilder",
    apiKeyUrl: "https://console.bce.baidu.com/qianfan/ais/console/applicationConsole/application",
    icon: "baidu",
  },
  {
    id: "bailian",
    name: "阿里云百炼",
    baseUrl: "https://dashscope.aliyuncs.com/apps/anthropic",
    websiteUrl: "https://bailian.console.aliyun.com",
    icon: "bailian",
  },
  {
    id: "bailian-coding",
    name: "Bailian For Coding",
    baseUrl: "https://coding.dashscope.aliyuncs.com/apps/anthropic",
    websiteUrl: "https://bailian.console.aliyun.com",
    icon: "bailian",
  },
  {
    id: "kimi",
    name: "Kimi",
    baseUrl: "https://api.moonshot.cn/anthropic",
    model: "kimi-k2.7-code",
    modelHaiku: "kimi-k2.7-code",
    modelSonnet: "kimi-k2.7-code",
    modelOpus: "kimi-k2.7-code",
    websiteUrl: "https://platform.kimi.com",
    apiKeyUrl: "https://platform.kimi.com/console/api-keys",
    icon: "kimi",
  },
  {
    id: "kimi-coding",
    name: "Kimi For Coding",
    baseUrl: "https://api.kimi.com/coding/",
    // 端点必须显式路由别名 kimi-for-coding，四个角色槽全部钉住：
    // CLAUDE_CODE_MAX_CONTEXT_TOKENS 只对非 claude- 前缀模型 id 生效，
    // 若不覆盖 haiku/sonnet/opus，子代理流量会以官方默认名打中转 404
    // （cc-switch 大量用户验证后的配置，我们此前整条漏设 model）。
    model: "kimi-for-coding",
    modelHaiku: "kimi-for-coding",
    modelSonnet: "kimi-for-coding",
    modelOpus: "kimi-for-coding",
    extraSettings: {
      env: {
        CLAUDE_CODE_MAX_CONTEXT_TOKENS: "262144",
        CLAUDE_CODE_AUTO_COMPACT_WINDOW: "262144",
      },
    },
    websiteUrl: "https://www.kimi.com/code/",
    icon: "kimi",
  },
  {
    id: "stepfun",
    name: "StepFun",
    baseUrl: "https://api.stepfun.com/step_plan",
    model: "step-3.5-flash-2603",
    modelHaiku: "step-3.5-flash-2603",
    modelSonnet: "step-3.5-flash-2603",
    modelOpus: "step-3.5-flash-2603",
    websiteUrl: "https://platform.stepfun.com/step-plan",
    apiKeyUrl: "https://platform.stepfun.com/interface-key",
    icon: "stepfun",
  },
  {
    id: "stepfun-en",
    name: "StepFun en",
    baseUrl: "https://api.stepfun.ai/step_plan",
    model: "step-3.5-flash-2603",
    modelHaiku: "step-3.5-flash-2603",
    modelSonnet: "step-3.5-flash-2603",
    modelOpus: "step-3.5-flash-2603",
    websiteUrl: "https://platform.stepfun.ai/step-plan",
    apiKeyUrl: "https://platform.stepfun.ai/interface-key",
    icon: "stepfun",
  },
  {
    id: "modelscope",
    name: "ModelScope",
    baseUrl: "https://api-inference.modelscope.cn",
    model: "ZhipuAI/GLM-5.1",
    modelHaiku: "ZhipuAI/GLM-5.1",
    modelSonnet: "ZhipuAI/GLM-5.1",
    modelOpus: "ZhipuAI/GLM-5.1",
    websiteUrl: "https://modelscope.cn",
    apiKeyUrl: "https://modelscope.cn/my/myaccesstoken",
    icon: "modelscope",
  },
  {
    id: "longcat",
    name: "Longcat",
    baseUrl: "https://api.longcat.chat/anthropic",
    model: "LongCat-2.0",
    modelHaiku: "LongCat-2.0",
    modelSonnet: "LongCat-2.0",
    modelOpus: "LongCat-2.0",
    extraSettings: {
      env: {
        ANTHROPIC_SMALL_FAST_MODEL: "LongCat-2.0",
        CLAUDE_CODE_MAX_OUTPUT_TOKENS: "131072",
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
      },
    },
    websiteUrl: "https://longcat.chat/platform",
    apiKeyUrl: "https://longcat.chat/platform/api_keys",
    icon: "longcat",
  },
  {
    id: "minimax",
    name: "MiniMax",
    // 官方 Claude Code 文档（platform.minimaxi.com/docs/token-plan/claude-code，2026-09 核验）：
    // 模型 MiniMax-M3[1m]，并把自动压缩阈值对齐 1M 上下文。
    baseUrl: "https://api.minimaxi.com/anthropic",
    model: "MiniMax-M3[1m]",
    modelHaiku: "MiniMax-M3[1m]",
    modelSonnet: "MiniMax-M3[1m]",
    modelOpus: "MiniMax-M3[1m]",
    extraSettings: {
      env: {
        API_TIMEOUT_MS: "3000000",
        CLAUDE_CODE_AUTO_COMPACT_WINDOW: "1000000",
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
      },
    },
    websiteUrl: "https://platform.minimaxi.com",
    apiKeyUrl: "https://platform.minimaxi.com/subscribe/coding-plan",
    icon: "minimax",
  },
  {
    id: "minimax-en",
    name: "MiniMax en",
    // 官方 Claude Code 文档（platform.minimax.io/docs/token-plan/claude-code，2026-09 核验）：
    // 模型 MiniMax-M3[1m]，并把自动压缩阈值对齐 1M 上下文。
    baseUrl: "https://api.minimax.io/anthropic",
    model: "MiniMax-M3[1m]",
    modelHaiku: "MiniMax-M3[1m]",
    modelSonnet: "MiniMax-M3[1m]",
    modelOpus: "MiniMax-M3[1m]",
    extraSettings: {
      env: {
        API_TIMEOUT_MS: "3000000",
        CLAUDE_CODE_AUTO_COMPACT_WINDOW: "1000000",
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
      },
    },
    websiteUrl: "https://platform.minimax.io",
    apiKeyUrl: "https://platform.minimax.io/subscribe/coding-plan",
    icon: "minimax",
  },
  {
    // 腾讯云 TokenHub Token Plan 个人版（cloud.tencent.com/document/product/1823/130060，
    // 2026-08-21 版，2026-09 核验）：Anthropic 协议端点 /plan/anthropic，Auto 智能路由
    // 调用 ID 为 tc-code-latest；与按量的 TokenHub /v1（tencent-hunyuan 预设）是两条
    // 产品线，订阅 Key 只能走 /plan。文档只给 /plan/v3 Chat Completions，无 Responses，
    // 故不设 Codex 预设。
    id: "tencent-token-plan",
    name: "腾讯云 Token Plan",
    baseUrl: "https://api.lkeap.cloud.tencent.com/plan/anthropic",
    model: "tc-code-latest",
    modelHaiku: "tc-code-latest",
    modelSonnet: "tc-code-latest",
    modelOpus: "tc-code-latest",
    websiteUrl: "https://cloud.tencent.com/product/tokenhub",
    apiKeyUrl: "https://console.cloud.tencent.com/tokenhub/tokenplan",
    icon: "tencent",
  },
  {
    id: "bailing",
    name: "BaiLing",
    baseUrl: "https://api.tbox.cn/api/anthropic",
    model: "Ling-2.5-1T",
    modelHaiku: "Ling-2.5-1T",
    modelSonnet: "Ling-2.5-1T",
    modelOpus: "Ling-2.5-1T",
    websiteUrl: "https://alipaytbox.yuque.com/sxs0ba/ling/get_started",
    apiKeyUrl: "https://ling.tbox.cn/open",
  },
  {
    id: "aihubmix",
    name: "AiHubMix",
    baseUrl: "https://aihubmix.com",
    apiKeyField: "api-key",
    endpointCandidates: ["https://aihubmix.com", "https://api.aihubmix.com"],
    websiteUrl: "https://aihubmix.com",
    icon: "aihubmix",
  },
  {
    id: "siliconflow",
    name: "SiliconFlow",
    baseUrl: "https://api.siliconflow.cn",
    model: "Pro/MiniMaxAI/MiniMax-M2.7",
    modelHaiku: "Pro/MiniMaxAI/MiniMax-M2.7",
    modelSonnet: "Pro/MiniMaxAI/MiniMax-M2.7",
    modelOpus: "Pro/MiniMaxAI/MiniMax-M2.7",
    websiteUrl: "https://siliconflow.cn",
    apiKeyUrl: "https://cloud.siliconflow.cn",
    icon: "siliconflow",
  },
  {
    id: "siliconflow-en",
    name: "SiliconFlow en",
    baseUrl: "https://api.siliconflow.com",
    model: "MiniMaxAI/MiniMax-M2.7",
    modelHaiku: "MiniMaxAI/MiniMax-M2.7",
    modelSonnet: "MiniMaxAI/MiniMax-M2.7",
    modelOpus: "MiniMaxAI/MiniMax-M2.7",
    websiteUrl: "https://siliconflow.com",
    apiKeyUrl: "https://cloud.siliconflow.cn",
    icon: "siliconflow",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    baseUrl: "https://openrouter.ai/api",
    model: "anthropic/claude-sonnet-5",
    modelHaiku: "anthropic/claude-haiku-4.5",
    modelSonnet: "anthropic/claude-sonnet-5",
    modelOpus: "anthropic/claude-opus-5",
    websiteUrl: "https://openrouter.ai",
    apiKeyUrl: "https://openrouter.ai/keys",
    icon: "openrouter",
  },
  {
    id: "novita",
    name: "Novita AI",
    baseUrl: "https://api.novita.ai/anthropic",
    model: "zai-org/glm-5.1",
    modelHaiku: "zai-org/glm-5.1",
    modelSonnet: "zai-org/glm-5.1",
    modelOpus: "zai-org/glm-5.1",
    websiteUrl: "https://novita.ai",
    icon: "novita",
  },
  {
    id: "xiaomi-mimo",
    name: "Xiaomi MiMo",
    baseUrl: "https://api.xiaomimimo.com/anthropic",
    model: "mimo-v2.5-pro",
    modelHaiku: "mimo-v2.5-pro",
    modelSonnet: "mimo-v2.5-pro",
    modelOpus: "mimo-v2.5-pro",
    websiteUrl: "https://platform.xiaomimimo.com",
    apiKeyUrl: "https://platform.xiaomimimo.com/#/console/api-keys",
    icon: "xiaomimimo",
  },
  {
    id: "xiaomi-mimo-token-plan",
    name: "Xiaomi MiMo Token Plan",
    baseUrl: "https://token-plan-cn.xiaomimimo.com/anthropic",
    model: "mimo-v2.5-pro",
    modelHaiku: "mimo-v2.5-pro",
    modelSonnet: "mimo-v2.5-pro",
    modelOpus: "mimo-v2.5-pro",
    websiteUrl: "https://platform.xiaomimimo.com/#/token-plan",
    apiKeyUrl: "https://platform.xiaomimimo.com/#/console/plan-manage",
    icon: "xiaomimimo",
  },
  {
    // xAI 官方 REST API 兼容 Anthropic SDK：base_url 用根域，
    // Claude Code 自动追加 /v1/messages（核对日期 2026-07-20）。
    id: "xai-grok",
    name: "xAI Grok",
    baseUrl: "https://api.x.ai",
    model: "grok-4.5",
    modelHaiku: "grok-code-fast-1",
    modelSonnet: "grok-4.5",
    modelOpus: "grok-4.5",
    websiteUrl: "https://x.ai",
    apiKeyUrl: "https://console.x.ai",
    icon: "grok",
  },
  {
    // DeepInfra 官方提供 Anthropic Messages 兼容层（/anthropic），鉴权走 x-api-key。
    id: "deepinfra",
    name: "DeepInfra",
    baseUrl: "https://api.deepinfra.com/anthropic",
    apiKeyField: "api-key",
    model: "deepseek-ai/DeepSeek-V4-Pro",
    modelHaiku: "deepseek-ai/DeepSeek-V4-Flash",
    modelSonnet: "deepseek-ai/DeepSeek-V4-Pro",
    modelOpus: "deepseek-ai/DeepSeek-V4-Pro",
    websiteUrl: "https://deepinfra.com",
    apiKeyUrl: "https://deepinfra.com/dash/api_keys",
    icon: "deepinfra",
  },
];

/**
 * Codex 侧预设。Codex 2026-02 起 `wire_api = "chat"` 是硬错误（openai/codex#7782），
 * live 一律写 responses：原生支持 /responses 的端点直连（经官方文档或「/responses
 * 401 而伪路由 404」探测核验，注释写明来源）；只有 Chat 端点的厂商标 `wireApi: "chat"`，
 * 须开启本地路由由 MCPane 转换（ADR-022），未开启时切换会被拦下。
 */
const CODEX_PRESETS: RelayPreset[] = [
  {
    // NewAPI 统一网关，OpenAI 兼容端点在 /v1（Responses 走 /v1/responses）。
    id: "computetoken",
    name: "Compute Token",
    baseUrl: "https://computetoken.ai/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "gpt-5.6-sol",
    recommended: true,
    websiteUrl: "https://computetoken.ai",
    icon: "computetoken",
  },
  {
    id: "volcengine-agentplan",
    name: "火山 Agentplan",
    baseUrl: "https://ark.cn-beijing.volces.com/api/coding/v3",
    extraSettings: { model_reasoning_effort: "high" },
    model: "ark-code-latest",
    // 官方 Codex 文档（volcengine.com/docs/82379/2556056，2026-08-04 更新）：
    // Coding Plan /api/coding/v3 原生支持 Responses API，缺省 responses 即可。
    websiteUrl: "https://www.volcengine.com/activity/codingplan",
    icon: "doubao",
  },
  {
    id: "byteplus",
    name: "BytePlus",
    // BytePlus 官方 Codex 文档（docs.byteplus.com/en/docs/ModelArk/2556056，2026-09 核验）：Coding Plan 支持 Responses。
    baseUrl: "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
    extraSettings: { model_reasoning_effort: "high" },
    model: "ark-code-latest",
    websiteUrl: "https://www.byteplus.com/en/product/modelark",
  },
  {
    id: "doubao-seed",
    name: "DouBaoSeed",
    baseUrl: "https://ark.cn-beijing.volces.com/api/v3",
    extraSettings: { model_reasoning_effort: "high" },
    model: "doubao-seed-2-1-pro-260628",
    websiteUrl: "https://console.volcengine.com/ark",
    icon: "doubao",
  },
  {
    id: "azure-openai",
    name: "Azure OpenAI",
    // 占位资源名，选择预设后把 YOUR_RESOURCE_NAME 换成自己的 Azure 资源名。
    baseUrl: "https://YOUR_RESOURCE_NAME.openai.azure.com/openai",
    extraSettings: { model_reasoning_effort: "high" },
    model: "gpt-5.6-sol",
    queryParams: { "api-version": "2025-04-01-preview" },
    websiteUrl: "https://learn.microsoft.com/en-us/azure/ai-foundry/openai/how-to/codex",
    icon: "azure",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    // DeepSeek 官方 Responses API（api-docs.deepseek.com/guides/responses_api，2026-09 核验）：base_url 不变，缺省 responses 直连。
    baseUrl: "https://api.deepseek.com",
    extraSettings: { model_reasoning_effort: "high" },
    model: "deepseek-v4-flash",
    contextWindow1M: true,
    websiteUrl: "https://platform.deepseek.com",
    apiKeyUrl: "https://platform.deepseek.com/api_keys",
    icon: "deepseek",
  },
  {
    id: "zhipu-glm",
    name: "Zhipu GLM",
    // 智谱官方 Codex 文档（docs.bigmodel.cn/cn/coding-plan/tool/codex，2026-09 核验）：Responses 专属端点 /api/v1，默认 glm-5.3。
    baseUrl: "https://open.bigmodel.cn/api/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "glm-5.3",
    websiteUrl: "https://open.bigmodel.cn",
    apiKeyUrl: "https://www.bigmodel.cn/claude-code",
    icon: "zhipu",
  },
  {
    id: "zhipu-glm-en",
    name: "Zhipu GLM en",
    // Z.AI 官方 Codex 文档（docs.z.ai/devpack/tool/codex，2026-09 核验）：Responses 专属端点 /api/v1，默认 glm-5.3。
    baseUrl: "https://api.z.ai/api/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "glm-5.3",
    websiteUrl: "https://z.ai",
    apiKeyUrl: "https://z.ai/subscribe",
    icon: "zhipu",
  },
  {
    id: "bailian",
    name: "阿里云百炼",
    baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "qwen3.8-max",
    websiteUrl: "https://bailian.console.aliyun.com",
    apiKeyUrl: "https://bailian.console.aliyun.com/#/api-key",
    icon: "bailian",
  },
  {
    // 百炼 Token Plan（help.aliyun.com/zh/model-studio/codex，2026-09-15 版，2026-09 核验）：
    // 订阅制专属端点 token-plan.cn-beijing，Responses 协议，文档示例模型 qwen3.7-max。
    // 按量与订阅的 Key/端点互不通用，故与 bailian 分开成两条预设。
    id: "bailian-token-plan",
    name: "百炼 Token Plan",
    baseUrl: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "qwen3.7-max",
    websiteUrl: "https://bailian.console.aliyun.com",
    apiKeyUrl: "https://bailian.console.aliyun.com/#/api-key",
    icon: "bailian",
  },
  {
    // 国际站 Token Plan 团队版（alibabacloud.com/help/en/model-studio/codex，2026-09 核验）：
    // 新加坡固定域名 token-plan.ap-southeast-1，Responses 协议，文档示例 qwen3.7-max。
    // 国际站按量端点要带 {WorkspaceId} 模板，不符合 base_url + Key 模型，故只收这条。
    id: "qwencloud-token-plan",
    name: "QwenCloud Token Plan",
    baseUrl: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "qwen3.7-max",
    websiteUrl: "https://www.qwencloud.com/pricing/token-plan",
    apiKeyUrl: "https://home.qwencloud.com/api-keys",
    icon: "bailian",
  },
  {
    id: "tencent-hunyuan",
    name: "Tencent Hunyuan",
    // 腾讯 TokenHub 官方 Codex 文档（cloud.tencent.com/document/product/1823/133532）：
    // hy3 原生 Responses（wire_api=responses），官方硬性要求 disable_response_storage
    // （切换臂对 relay 恒写，无需在此声明）。⚠️ 须用 TokenHub API Key（创建时
    // 可访问范围需勾选 Hy3）；Coding Plan / Token Plan 订阅 Key 对 /v1 不通。
    baseUrl: "https://tokenhub.tencentmaas.com/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "hy3",
    // 官方备用域名 tencentmaas.cn；国际站 tokenhub-intl 属不同地域（Key 不通用），不作候选。
    endpointCandidates: [
      "https://tokenhub.tencentmaas.com/v1",
      "https://tokenhub.tencentmaas.cn/v1",
    ],
    websiteUrl: "https://cloud.tencent.com/product/tokenhub",
    apiKeyUrl: "https://console.cloud.tencent.com/tokenhub/apikey",
    icon: "hunyuan",
  },
  {
    id: "kimi",
    name: "Kimi",
    // Kimi 开放平台 Responses API（platform.kimi.com/docs/api/responses，2026-09 核验）：/v1/responses 原生可用，默认 kimi-k3。
    baseUrl: "https://api.moonshot.cn/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "kimi-k3",
    websiteUrl: "https://platform.kimi.com",
    apiKeyUrl: "https://platform.kimi.com/console/api-keys",
    icon: "kimi",
  },
  {
    id: "kimi-coding",
    name: "Kimi For Coding",
    // Kimi Code 官方 Codex 文档（kimi.com/code/docs/third-party-tools/codex，2026-09 核验）：原生 Responses，推荐 k3-256k（1M 权限用 k3）。
    baseUrl: "https://api.kimi.com/coding/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "k3-256k",
    websiteUrl: "https://www.kimi.com/code/",
    icon: "kimi",
  },
  {
    id: "stepfun",
    name: "StepFun",
    // 2026-09 探测：/responses 401 而伪路由 404，端点存在（官方文档未单列 Codex）。
    baseUrl: "https://api.stepfun.com/step_plan/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "step-3.7-flash",
    websiteUrl: "https://platform.stepfun.com/step-plan",
    apiKeyUrl: "https://platform.stepfun.com/interface-key",
    icon: "stepfun",
  },
  {
    id: "stepfun-en",
    name: "StepFun en",
    // 2026-09 探测：/responses 401 而伪路由 404，端点存在（官方文档未单列 Codex）。
    baseUrl: "https://api.stepfun.ai/step_plan/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "step-3.7-flash",
    websiteUrl: "https://platform.stepfun.ai/step-plan",
    apiKeyUrl: "https://platform.stepfun.ai/interface-key",
    icon: "stepfun",
  },
  {
    id: "modelscope",
    name: "ModelScope",
    // 2026-09 探测：/responses 400 而伪路由 404，端点存在。
    baseUrl: "https://api-inference.modelscope.cn/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "ZhipuAI/GLM-5.1",
    websiteUrl: "https://modelscope.cn",
    apiKeyUrl: "https://modelscope.cn/my/myaccesstoken",
    icon: "modelscope",
  },
  {
    id: "longcat",
    name: "Longcat",
    // LongCat 官方 Codex 文档（longcat.chat/platform/docs/codex，2026-09 核验）：/openai/v1 原生 Responses。
    baseUrl: "https://api.longcat.chat/openai/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "LongCat-2.0",
    websiteUrl: "https://longcat.chat/platform",
    apiKeyUrl: "https://longcat.chat/platform/api_keys",
    icon: "longcat",
  },
  {
    id: "minimax",
    name: "MiniMax",
    // MiniMax 官方 Codex 文档（platform.minimaxi.com/docs/token-plan/codex，2026-09 核验）：/v1 原生 Responses。
    baseUrl: "https://api.minimaxi.com/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "MiniMax-M3",
    contextWindow1M: true,
    websiteUrl: "https://platform.minimaxi.com",
    apiKeyUrl: "https://platform.minimaxi.com/subscribe/coding-plan",
    icon: "minimax",
  },
  {
    id: "minimax-en",
    name: "MiniMax en",
    baseUrl: "https://api.minimax.io/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "MiniMax-M3",
    contextWindow1M: true,
    websiteUrl: "https://platform.minimax.io",
    apiKeyUrl: "https://platform.minimax.io/subscribe/coding-plan",
    icon: "minimax",
  },
  {
    id: "xiaomi-mimo",
    name: "Xiaomi MiMo",
    baseUrl: "https://api.xiaomimimo.com/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "mimo-v2.5-pro",
    websiteUrl: "https://platform.xiaomimimo.com",
    apiKeyUrl: "https://platform.xiaomimimo.com/#/console/api-keys",
    icon: "xiaomimimo",
  },
  {
    id: "xiaomi-mimo-token-plan",
    name: "Xiaomi MiMo Token Plan",
    baseUrl: "https://token-plan-cn.xiaomimimo.com/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "mimo-v2.5-pro",
    websiteUrl: "https://platform.xiaomimimo.com/#/token-plan",
    apiKeyUrl: "https://platform.xiaomimimo.com/#/console/plan-manage",
    icon: "xiaomimimo",
  },
  {
    id: "novita",
    name: "Novita AI",
    // 2026-09 探测：/responses 401 而伪路由 404，端点存在。
    baseUrl: "https://api.novita.ai/openai/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "zai-org/glm-5.1",
    websiteUrl: "https://novita.ai",
    icon: "novita",
  },
  {
    id: "aihubmix",
    name: "AiHubMix",
    baseUrl: "https://aihubmix.com/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "gpt-5.6-sol",
    endpointCandidates: ["https://aihubmix.com/v1", "https://api.aihubmix.com/v1"],
    websiteUrl: "https://aihubmix.com",
    icon: "aihubmix",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "gpt-5.6-sol",
    websiteUrl: "https://openrouter.ai",
    apiKeyUrl: "https://openrouter.ai/keys",
    icon: "openrouter",
  },
  // ---- 以下厂商端点经官方文档核实（2026-07-20）：全部 OpenAI-compatible，
  //      走 Codex 原生 wire_api = "chat"。未收录 Perplexity：其 chat
  //      completions 已进 maintenance mode，且为联网检索模型非编码后端。
  {
    id: "xai-grok",
    name: "xAI Grok",
    // xAI 官方文档（docs.x.ai，2026-09 核验）：Responses API 为推荐接口，/v1/responses。
    baseUrl: "https://api.x.ai/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "grok-4.5",
    websiteUrl: "https://x.ai",
    apiKeyUrl: "https://console.x.ai",
    icon: "grok",
  },
  {
    id: "groq",
    name: "Groq",
    // Groq 官方文档（console.groq.com/docs/responses-api，2026-09 核验）：/openai/v1/responses。
    baseUrl: "https://api.groq.com/openai/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "moonshotai/kimi-k2-instruct",
    websiteUrl: "https://groq.com",
    apiKeyUrl: "https://console.groq.com/keys",
    icon: "groq",
  },
  {
    id: "together",
    name: "Together AI",
    // 2026-09 探测：/responses 401 而伪路由 404，端点存在（官方文档未单列）。
    baseUrl: "https://api.together.ai/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "moonshotai/Kimi-K2.7-Code",
    websiteUrl: "https://www.together.ai",
    apiKeyUrl: "https://api.together.ai/settings/api-keys",
    icon: "together",
  },
  {
    // model id 必须是完整 accounts/fireworks/models/<name> 格式。
    id: "fireworks",
    name: "Fireworks AI",
    // Fireworks 官方文档（docs.fireworks.ai/guides/response-api，2026-09 核验）：/inference/v1/responses。
    baseUrl: "https://api.fireworks.ai/inference/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "accounts/fireworks/models/deepseek-v4-pro",
    websiteUrl: "https://fireworks.ai",
    apiKeyUrl: "https://app.fireworks.ai/settings/users/api-keys",
    icon: "fireworks",
  },
  // ---- 只有 Chat Completions 端点的厂商（2026-09 探测 /responses 均 404 或无法判定）：
  //      wireApi: "chat" 需开启本地路由，由 MCPane 转换为 Responses（ADR-022）。
  {
    id: "qianfan",
    name: "Baidu Qianfan",
    baseUrl: "https://qianfan.baidubce.com/v2/coding",
    extraSettings: { model_reasoning_effort: "high" },
    model: "qianfan-code-latest",
    wireApi: "chat",
    websiteUrl: "https://cloud.baidu.com/product/qianfan_modelbuilder",
    apiKeyUrl: "https://console.bce.baidu.com/qianfan/ais/console/applicationConsole/application",
    icon: "baidu",
  },
  {
    id: "bailing",
    name: "BaiLing",
    baseUrl: "https://api.tbox.cn/api/llm/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "Ling-2.6-1T",
    wireApi: "chat",
    websiteUrl: "https://alipaytbox.yuque.com/sxs0ba/ling/get_started",
    apiKeyUrl: "https://ling.tbox.cn/open",
  },
  {
    id: "siliconflow",
    name: "SiliconFlow",
    baseUrl: "https://api.siliconflow.cn/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "Pro/MiniMaxAI/MiniMax-M2.7",
    wireApi: "chat",
    websiteUrl: "https://siliconflow.cn",
    apiKeyUrl: "https://cloud.siliconflow.cn",
    icon: "siliconflow",
  },
  {
    id: "siliconflow-en",
    name: "SiliconFlow en",
    baseUrl: "https://api.siliconflow.com/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "MiniMaxAI/MiniMax-M2.7",
    wireApi: "chat",
    websiteUrl: "https://siliconflow.com",
    apiKeyUrl: "https://cloud.siliconflow.cn",
    icon: "siliconflow",
  },
  {
    id: "nvidia",
    name: "Nvidia",
    baseUrl: "https://integrate.api.nvidia.com/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "moonshotai/kimi-k2.5",
    wireApi: "chat",
    websiteUrl: "https://build.nvidia.com",
    apiKeyUrl: "https://build.nvidia.com/settings/api-keys",
    icon: "nvidia",
  },
  {
    // 生产级仅 gpt-oss-120b；glm-4.7 等尚为 Preview，暂不作默认。
    id: "cerebras",
    name: "Cerebras",
    baseUrl: "https://api.cerebras.ai/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "gpt-oss-120b",
    wireApi: "chat",
    websiteUrl: "https://www.cerebras.ai",
    apiKeyUrl: "https://cloud.cerebras.ai",
    icon: "cerebras",
  },
  {
    id: "deepinfra",
    name: "DeepInfra",
    baseUrl: "https://api.deepinfra.com/v1/openai",
    extraSettings: { model_reasoning_effort: "high" },
    model: "deepseek-ai/DeepSeek-V4-Pro",
    wireApi: "chat",
    websiteUrl: "https://deepinfra.com",
    apiKeyUrl: "https://deepinfra.com/dash/api_keys",
    icon: "deepinfra",
  },
  {
    // La Plateforme 统一 key 走 api.mistral.ai；编码专精 Devstral 2。
    id: "mistral",
    name: "Mistral",
    baseUrl: "https://api.mistral.ai/v1",
    extraSettings: { model_reasoning_effort: "high" },
    model: "devstral-2512",
    wireApi: "chat",
    websiteUrl: "https://mistral.ai",
    apiKeyUrl: "https://console.mistral.ai/api-keys",
    icon: "mistral",
  },
];

/**
 * Gemini 侧预设。落盘形态：`~/.gemini/.env` 的 GOOGLE_GEMINI_BASE_URL /
 * GEMINI_API_KEY / GEMINI_MODEL 三键 + settings.json selectedType。
 * cc-switch 的 Gemini 预设含 13 个 isPartner 推广中转站，全部不收录；
 * 中立聚合仅 OpenRouter 提供 Gemini 原生兼容端点。
 */
const GEMINI_PRESETS: RelayPreset[] = [
  {
    // 官方 API Key 形态（区别于官方 OAuth 登录）：AI Studio 免费/按量 Key。
    id: "google-ai-studio",
    name: "Google AI Studio",
    baseUrl: "https://generativelanguage.googleapis.com",
    websiteUrl: "https://aistudio.google.com",
    apiKeyUrl: "https://aistudio.google.com/apikey",
    icon: "gemini",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    baseUrl: "https://openrouter.ai/api",
    model: "gemini-3.6-flash",
    websiteUrl: "https://openrouter.ai",
    apiKeyUrl: "https://openrouter.ai/keys",
    icon: "openrouter",
  },
];

/* ---------- Grok CLI 预设 ---------- */

/**
 * Grok CLI 走 OpenAI 兼容的 chat completions（模型表里的 `api_backend = "chat"`），
 * 所以能接的就是同一批兼容端点。官方形态（xAI OAuth 登录）不在这里——那是官方项。
 */
const GROK_PRESETS: RelayPreset[] = [
  {
    id: "xai-api",
    name: "xAI API",
    baseUrl: "https://api.x.ai/v1",
    model: "grok-4.5",
    websiteUrl: "https://x.ai/api",
    apiKeyUrl: "https://console.x.ai",
    icon: "grok",
    recommended: true,
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    model: "x-ai/grok-4.5",
    websiteUrl: "https://openrouter.ai",
    apiKeyUrl: "https://openrouter.ai/keys",
    icon: "openrouter",
  },
];

/* ---------- OpenCode / OpenClaw / Hermes（additive 三家）预设 ---------- */

/**
 * additive 客户端的供应商预设。三家的字段名不同（OpenCode 写 AI SDK 包名、
 * OpenClaw 写 `api`、Hermes 写 `api_mode`），但要的信息同一套：端点 + 模型组 +
 * 上游协议 + 上下文窗口，故共用这一份，由后端各 adapter 映射。
 *
 * 数值按各厂商官方文档与各客户端实测逐条核验（2026-07-26）：
 * - `models` 声明**全部**可用模型而非只一个——additive 客户端只显示显式声明过的
 *   模型，少声明就等于用户在客户端里选不到；
 * - `contextLength` 对应各客户端读取的 `contextWindow`
 *   （客户端拿它判断何时压缩会话，不给就吃客户端默认值）。
 *
 * ⚠️ 智谱走的是 **coding 专属路径** `/api/coding/paas/v4`：官方明确 coding-plan
 * 必须走该路径，误用通用 `/api/paas/v4` 会用不上套餐额度或直接失败（这条在
 * 2026-07-22 的 Codex 侧核验里已确认，additive 侧此前漏了）。
 *
 * **一律不带 `upstreamApi`（全部 OpenAI 兼容）**，两个原因：
 * 1. 这六家全部可走 `openai-completions`，包括
 *    OpenRouter（其 `/api/v1` 本身就是 OpenAI 兼容口，`anthropic/claude-*`
 *    走它没问题）；OpenRouter 也可以用 `@ai-sdk/anthropic`
 *    ——同一家两种接法都成立，OpenAI 兼容是三家客户端的公共最小面。
 * 2. 一份预设服务三家客户端，**只在三家的正确取值一致时才能写进来**。
 *    `Kimi For Coding` 就是反例：OpenCode 侧走
 *    `api.kimi.com/coding/v1` + anthropic 包、OpenClaw 侧走 `api.kimi.com/v1` +
 *    openai-completions，端点与协议**两处都不同**，无法用一条共享预设表达
 *    （硬选一边就会让另一边用不上套餐路径）。故该家不进 additive 预设，
 *    需要的用户手动添加；Claude / Codex 侧的 `kimi-coding` 预设不受影响。
 */
const OPENCODE_PRESETS: RelayPreset[] = [
  {
    id: "kimi",
    name: "Kimi",
    baseUrl: "https://api.moonshot.cn/v1",
    model: "kimi-k2.7-code",
    models: ["kimi-k2.7-code", "kimi-k3"],
    contextLength: 262144,
    websiteUrl: "https://platform.kimi.com",
    apiKeyUrl: "https://platform.kimi.com/console/api-keys",
    icon: "kimi",
    recommended: true,
  },
  {
    id: "zhipu",
    name: "智谱 GLM",
    baseUrl: "https://open.bigmodel.cn/api/coding/paas/v4",
    model: "glm-5.1",
    models: ["glm-5.1"],
    contextLength: 128000,
    websiteUrl: "https://open.bigmodel.cn",
    apiKeyUrl: "https://open.bigmodel.cn/usercenter/apikeys",
    icon: "zhipu",
  },
  {
    id: "zhipu-en",
    name: "智谱 GLM en",
    baseUrl: "https://api.z.ai/api/coding/paas/v4",
    model: "glm-5.1",
    models: ["glm-5.1"],
    contextLength: 128000,
    websiteUrl: "https://z.ai",
    apiKeyUrl: "https://z.ai/subscribe",
    icon: "zhipu",
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    baseUrl: "https://api.deepseek.com/v1",
    model: "deepseek-v4-pro",
    models: ["deepseek-v4-pro", "deepseek-v4-flash"],
    contextLength: 1000000,
    websiteUrl: "https://platform.deepseek.com",
    apiKeyUrl: "https://platform.deepseek.com/api_keys",
    icon: "deepseek",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    model: "anthropic/claude-sonnet-5",
    models: ["anthropic/claude-sonnet-5", "anthropic/claude-opus-5"],
    contextLength: 1000000,
    websiteUrl: "https://openrouter.ai",
    apiKeyUrl: "https://openrouter.ai/keys",
    icon: "openrouter",
  },
];

/* ---------- Claude Desktop 预设 ---------- */

/**
 * Desktop fail-all 校验器接受的四档角色 route ID（Desktop 1.12603.1+ 白名单
 * 含 fable；与后端 claude_desktop::DEFAULT_PROXY_ROUTE_IDS 同源同序）。
 */
export const DESKTOP_ROLE_ROUTE_IDS = {
  sonnet: "claude-sonnet-5",
  opus: "claude-opus-5",
  haiku: "claude-haiku-4-5",
  fable: "claude-fable-5",
} as const;

export type DesktopRoleId = keyof typeof DESKTOP_ROLE_ROUTE_IDS;

/** 透传路由（direct 或「上游本身接受 claude-* 名」的 proxy 端点）。 */
const desktopPassthroughRoutes = (supports1m = false): DesktopRoute[] =>
  (["sonnet", "opus", "haiku"] as const).map((role) => ({
    routeId: DESKTOP_ROLE_ROUTE_IDS[role],
    upstreamModel: DESKTOP_ROLE_ROUTE_IDS[role],
    supports1m,
  }));

/** 品牌路由：三档映射到厂商真实模型，菜单显示品牌名。 */
const desktopBrandedRoutes = (
  sonnet: string,
  opus: string,
  haiku: string,
  supports1m = false,
): DesktopRoute[] => {
  const seen = new Set<string>();
  return [
    { role: "sonnet" as const, model: sonnet },
    { role: "opus" as const, model: opus },
    { role: "haiku" as const, model: haiku },
  ]
    .filter(({ model }) => {
      if (seen.has(model)) return false;
      seen.add(model);
      return true;
    })
    .map(({ role, model }) => ({
      routeId: DESKTOP_ROLE_ROUTE_IDS[role],
      upstreamModel: model,
      labelOverride: model,
      supports1m,
    }));
};

/** 映射路由（OpenRouter 类：上游用 anthropic/claude-* 命名空间）。 */
const desktopMappedRoutes = (
  sonnet: string,
  opus: string,
  haiku: string,
  supports1m = false,
): DesktopRoute[] =>
  [
    { role: "sonnet" as const, model: sonnet },
    { role: "opus" as const, model: opus },
    { role: "haiku" as const, model: haiku },
  ].map(({ role, model }) => ({
    routeId: DESKTOP_ROLE_ROUTE_IDS[role],
    upstreamModel: model,
    supports1m,
  }));

/**
 * 从 Claude 预设派生 Desktop 预设（端点/图标/认证形态单一来源），
 * 叠加 Desktop 专属的模式与路由。id 不存在时抛错（presets 测试兜底）。
 */
function desktopFromClaude(
  id: string,
  desktop: { mode: DesktopMode; routes: DesktopRoute[] },
): RelayPreset {
  const source = CLAUDE_PRESETS.find((p) => p.id === id);
  if (!source) throw new Error(`Claude 预设不存在：${id}`);
  return {
    id: source.id,
    name: source.name,
    baseUrl: source.baseUrl,
    apiKeyField: source.apiKeyField,
    // 候选端点随源预设走（AiHubMix 双端点等，cc-switch desktop 预设同样携带）。
    endpointCandidates: source.endpointCandidates,
    websiteUrl: source.websiteUrl,
    apiKeyUrl: source.apiKeyUrl,
    icon: source.icon,
    recommended: source.recommended,
    desktopMode: desktop.mode,
    desktopRoutes: desktop.routes,
  };
}

/**
 * Claude Desktop 预设。模式与路由值按 Desktop 3P 网关实测行为整理
 * （2026-07-22 核对）：
 * - direct：供应商直接卖 claude-* 模型（AiHubMix 为已验证的 direct；
 *   Compute Token 为我们的 NewAPI 网关默认首推，同 anthropic 面）；
 * - proxy：非 Claude 厂商，经本地网关映射。我们收录的 proxy
 *   预设上游全部是 anthropic 格式，故全量可用；openai_chat/gemini_native 上游
 *   （Nvidia / Gemini Native / OpenCode Go）不在收录面，无需格式转换层。
 * - 未收录 xai-grok / deepinfra（Claude 侧有、Desktop 侧未验证；
 *   可用自定义供应商手动接入）。
 */
const CLAUDE_DESKTOP_PRESETS: RelayPreset[] = [
  desktopFromClaude("computetoken", {
    mode: "direct",
    routes: desktopPassthroughRoutes(),
  }),
  desktopFromClaude("volcengine-agentplan", {
    mode: "proxy",
    routes: desktopBrandedRoutes("ark-code-latest", "ark-code-latest", "ark-code-latest"),
  }),
  desktopFromClaude("byteplus", {
    mode: "proxy",
    routes: desktopBrandedRoutes("ark-code-latest", "ark-code-latest", "ark-code-latest"),
  }),
  desktopFromClaude("doubao-seed", {
    mode: "proxy",
    routes: desktopBrandedRoutes(
      "doubao-seed-2-1-pro-260628",
      "doubao-seed-2-1-pro-260628",
      "doubao-seed-2-1-pro-260628",
    ),
  }),
  desktopFromClaude("deepseek", {
    mode: "proxy",
    routes: desktopBrandedRoutes("deepseek-v4-pro", "deepseek-v4-pro", "deepseek-v4-flash"),
  }),
  desktopFromClaude("zhipu-glm", {
    mode: "proxy",
    routes: desktopBrandedRoutes("glm-5.1", "glm-5.1", "glm-5.1"),
  }),
  desktopFromClaude("zhipu-glm-en", {
    mode: "proxy",
    routes: desktopBrandedRoutes("glm-5.1", "glm-5.1", "glm-5.1"),
  }),
  desktopFromClaude("qianfan", {
    mode: "proxy",
    routes: desktopBrandedRoutes(
      "qianfan-code-latest",
      "qianfan-code-latest",
      "qianfan-code-latest",
    ),
  }),
  // Bailian 系 anthropic 端点原生接受 claude-* 名（cc-switch 验证为透传）；
  // 仍走 proxy：Coding Plan 端点按 claude-code beta 标放行，需网关注入。
  desktopFromClaude("bailian", {
    mode: "proxy",
    routes: desktopPassthroughRoutes(),
  }),
  desktopFromClaude("bailian-coding", {
    mode: "proxy",
    routes: desktopPassthroughRoutes(),
  }),
  desktopFromClaude("kimi", {
    mode: "proxy",
    routes: desktopBrandedRoutes("kimi-k2.7-code", "kimi-k2.7-code", "kimi-k2.7-code"),
  }),
  desktopFromClaude("kimi-coding", {
    mode: "proxy",
    routes: desktopPassthroughRoutes(),
  }),
  desktopFromClaude("stepfun", {
    mode: "proxy",
    routes: desktopBrandedRoutes(
      "step-3.5-flash-2603",
      "step-3.5-flash-2603",
      "step-3.5-flash-2603",
    ),
  }),
  desktopFromClaude("stepfun-en", {
    mode: "proxy",
    routes: desktopBrandedRoutes(
      "step-3.5-flash-2603",
      "step-3.5-flash-2603",
      "step-3.5-flash-2603",
    ),
  }),
  desktopFromClaude("modelscope", {
    mode: "proxy",
    routes: desktopBrandedRoutes("ZhipuAI/GLM-5.1", "ZhipuAI/GLM-5.1", "ZhipuAI/GLM-5.1"),
  }),
  desktopFromClaude("longcat", {
    mode: "proxy",
    routes: desktopBrandedRoutes("LongCat-2.0", "LongCat-2.0", "LongCat-2.0"),
  }),
  desktopFromClaude("minimax", {
    mode: "proxy",
    routes: desktopBrandedRoutes("MiniMax-M3", "MiniMax-M3", "MiniMax-M3"),
  }),
  desktopFromClaude("minimax-en", {
    mode: "proxy",
    routes: desktopBrandedRoutes("MiniMax-M3", "MiniMax-M3", "MiniMax-M3"),
  }),
  desktopFromClaude("bailing", {
    mode: "proxy",
    routes: desktopBrandedRoutes("Ling-2.5-1T", "Ling-2.5-1T", "Ling-2.5-1T"),
  }),
  desktopFromClaude("aihubmix", {
    mode: "direct",
    routes: desktopPassthroughRoutes(),
  }),
  desktopFromClaude("siliconflow", {
    mode: "proxy",
    routes: desktopBrandedRoutes(
      "Pro/MiniMaxAI/MiniMax-M2.7",
      "Pro/MiniMaxAI/MiniMax-M2.7",
      "Pro/MiniMaxAI/MiniMax-M2.7",
    ),
  }),
  desktopFromClaude("siliconflow-en", {
    mode: "proxy",
    routes: desktopBrandedRoutes(
      "MiniMaxAI/MiniMax-M2.7",
      "MiniMaxAI/MiniMax-M2.7",
      "MiniMaxAI/MiniMax-M2.7",
    ),
  }),
  desktopFromClaude("openrouter", {
    mode: "proxy",
    routes: desktopMappedRoutes(
      "anthropic/claude-sonnet-5",
      "anthropic/claude-opus-5",
      "anthropic/claude-haiku-4.5",
      true,
    ),
  }),
  desktopFromClaude("novita", {
    mode: "proxy",
    routes: desktopBrandedRoutes("zai-org/glm-5.1", "zai-org/glm-5.1", "zai-org/glm-5.1"),
  }),
  desktopFromClaude("xiaomi-mimo", {
    mode: "proxy",
    routes: desktopBrandedRoutes("mimo-v2.5-pro", "mimo-v2.5-pro", "mimo-v2.5-pro"),
  }),
  desktopFromClaude("xiaomi-mimo-token-plan", {
    mode: "proxy",
    routes: desktopBrandedRoutes("mimo-v2.5-pro", "mimo-v2.5-pro", "mimo-v2.5-pro"),
  }),
];

export function relayPresetsFor(app: ClientApp): RelayPreset[] {
  switch (app) {
    case "claude":
      return CLAUDE_PRESETS;
    case "codex":
      return CODEX_PRESETS;
    case "gemini":
      return GEMINI_PRESETS;
    case "grok":
      return GROK_PRESETS;
    case "opencode":
    // OpenClaw / Hermes 接的也是 OpenAI 兼容端点，预设与 OpenCode 同一批。
    case "openclaw":
    case "hermes":
      return OPENCODE_PRESETS;
    case "claude-desktop":
      return CLAUDE_DESKTOP_PRESETS;
  }
}

/**
 * 预设分类（选择器分组用）：
 * - vendor 模型厂商：一方厂商自家模型端点
 * - aggregator 聚合平台：路由多家模型 / 托管开源模型的推理平台
 * - gateway 网关：NewAPI 自建网关、Azure 自有资源
 */
export type PresetCategory = "vendor" | "aggregator" | "gateway";

/** 分组展示顺序。 */
export const PRESET_CATEGORY_ORDER: PresetCategory[] = ["vendor", "aggregator", "gateway"];

// 聚合/多模型平台。新增此类预设时把 id 加进来（app 无关，claude/codex 同 id 同类）。
const AGGREGATOR_PRESET_IDS = new Set([
  "openrouter",
  "aihubmix",
  "modelscope",
  "siliconflow",
  "siliconflow-en",
  "novita",
  "deepinfra",
  "nvidia",
  "groq",
  "together",
  "fireworks",
  "cerebras",
]);

// 自建/统一网关（NewAPI 网关、Azure 自有资源）。
const GATEWAY_PRESET_IDS = new Set(["computetoken", "azure-openai"]);

/** 按 id 归类；未列入上面两集合的一律视为模型厂商。 */
export function presetCategory(preset: RelayPreset): PresetCategory {
  if (GATEWAY_PRESET_IDS.has(preset.id)) return "gateway";
  if (AGGREGATOR_PRESET_IDS.has(preset.id)) return "aggregator";
  return "vendor";
}

/** 统一供应商预设（对齐 cc-switch universalProviderPresets）。 */
export interface UnifiedPreset {
  id: string;
  name: string;
  /** i18n key（见 lib/i18n.ts）——描述是 UI 文案，跟随界面语言。 */
  descriptionKey: string;
  websiteUrl?: string;
  icon?: string;
  /** 每 app 默认模型。 */
  claudeModel?: string;
  claudeModelHaiku?: string;
  claudeModelSonnet?: string;
  claudeModelOpus?: string;
  claudeModelFable?: string;
  codexModel?: string;
}

export const UNIFIED_PRESETS: UnifiedPreset[] = [
  {
    id: "newapi",
    name: "NewAPI",
    descriptionKey: "unifiedPreset.newapi.desc",
    websiteUrl: "https://www.newapi.pro",
    icon: "openai",
    claudeModel: "claude-sonnet-5",
    claudeModelHaiku: "claude-haiku-4-5-20251001",
    claudeModelSonnet: "claude-sonnet-5",
    claudeModelOpus: "claude-opus-5",
    claudeModelFable: "claude-fable-5",
    codexModel: "gpt-5.6-sol",
  },
  {
    id: "custom-gateway",
    name: "自定义网关",
    descriptionKey: "unifiedPreset.customGateway.desc",
    claudeModel: "claude-sonnet-5",
    claudeModelHaiku: "claude-haiku-4-5-20251001",
    claudeModelSonnet: "claude-sonnet-5",
    claudeModelOpus: "claude-opus-5",
    claudeModelFable: "claude-fable-5",
    codexModel: "gpt-5.6-sol",
  },
];

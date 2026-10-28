import { describe, expect, it } from "vitest";
import {
  DESKTOP_ROLE_ROUTE_IDS,
  PRESET_CATEGORY_ORDER,
  presetCategory,
  relayPresetsFor,
  UNIFIED_PRESETS,
} from "./providerPresets";

const MANAGED_ENV_KEYS = [
  "ANTHROPIC_BASE_URL",
  "ANTHROPIC_AUTH_TOKEN",
  "ANTHROPIC_API_KEY",
  "ANTHROPIC_MODEL",
  "ANTHROPIC_DEFAULT_HAIKU_MODEL",
  "ANTHROPIC_DEFAULT_SONNET_MODEL",
  "ANTHROPIC_DEFAULT_OPUS_MODEL",
  "ANTHROPIC_DEFAULT_FABLE_MODEL",
  "CLAUDE_CODE_SUBAGENT_MODEL",
];

describe("relayPresets 数据健全性", () => {
  for (const app of ["claude", "codex", "gemini", "claude-desktop"] as const) {
    const presets = relayPresetsFor(app);

    it(`${app}: id 唯一且非空`, () => {
      const ids = presets.map((p) => p.id);
      expect(ids.length).toBeGreaterThan(0);
      expect(new Set(ids).size).toBe(ids.length);
    });

    it(`${app}: URL 均为 https 且不携带推广/邀请参数`, () => {
      for (const p of presets) {
        for (const url of [p.baseUrl, p.websiteUrl, p.apiKeyUrl].filter(Boolean) as string[]) {
          expect(url, `${p.id} 的 URL`).toMatch(/^https:\/\//);
          expect(url, `${p.id} 的 URL 不应携带跟踪参数`).not.toMatch(
            /[?&](aff|ref|utm_\w+|invitecode|ic|code|source|ytag|from|ac|rc)=/i,
          );
        }
      }
    });

    it(`${app}: 附加片段不包含受管键`, () => {
      const codexManagedTopKeys = [
        "model_provider",
        "model_providers",
        "disable_response_storage",
        "model",
        "model_context_window",
        "model_auto_compact_token_limit",
      ];
      for (const p of presets) {
        if (app === "claude") {
          const env = (p.extraSettings?.env ?? {}) as Record<string, unknown>;
          for (const key of Object.keys(env)) {
            expect(MANAGED_ENV_KEYS, `${p.id} 的 extraSettings.env`).not.toContain(key);
          }
        } else {
          for (const key of Object.keys(p.extraSettings ?? {})) {
            expect(codexManagedTopKeys, `${p.id} 的 extraSettings`).not.toContain(key);
          }
        }
      }
    });

    it(`${app}: 端点候选均为 https`, () => {
      for (const p of presets) {
        for (const url of p.endpointCandidates ?? []) {
          expect(url, `${p.id} 的候选端点`).toMatch(/^https:\/\//);
        }
      }
    });

    it(`${app}: 每个预设归入合法分类`, () => {
      for (const p of presets) {
        expect(PRESET_CATEGORY_ORDER, `${p.id} 的分类`).toContain(presetCategory(p));
      }
    });
  }

  // Chat 预设依赖本地路由转换协议；名单锁死，原生支持 /responses 的厂商不得标 chat。
  it("codex: 只有已核实无 /responses 的厂商标 chat（需本地路由）", () => {
    const chat = relayPresetsFor("codex")
      .filter((p) => p.wireApi === "chat")
      .map((p) => p.id)
      .sort();
    expect(chat).toEqual(
      [
        "bailing",
        "cerebras",
        "deepinfra",
        "mistral",
        "nvidia",
        "qianfan",
        "siliconflow",
        "siliconflow-en",
      ].sort(),
    );
    for (const p of relayPresetsFor("codex")) {
      expect([undefined, "responses", "chat"], `${p.id} 的 wireApi`).toContain(p.wireApi);
    }
  });

  it("contextWindow1M 只出现在 Codex 预设", () => {
    for (const app of ["claude", "gemini", "claude-desktop"] as const) {
      for (const p of relayPresetsFor(app)) {
        expect(p.contextWindow1M, `${app}/${p.id}`).toBeUndefined();
      }
    }
  });

  it("codex: 默认开启 1M 的预设是 DeepSeek 与 MiniMax", () => {
    const enabled = relayPresetsFor("codex")
      .filter((p) => p.contextWindow1M)
      .map((p) => p.id)
      .sort();
    expect(enabled).toEqual(["deepseek", "minimax", "minimax-en"]);
  });

  it("统一供应商预设含 NewAPI 与自定义网关", () => {
    const ids = UNIFIED_PRESETS.map((p) => p.id);
    expect(ids).toContain("newapi");
    expect(ids).toContain("custom-gateway");
  });
});

describe("additive 三家（OpenCode / OpenClaw / Hermes）预设专属约束", () => {
  // 三家共用同一份预设，取任一即可。
  const presets = relayPresetsFor("opencode");

  it("三家取到的是同一份预设", () => {
    expect(relayPresetsFor("openclaw")).toBe(presets);
    expect(relayPresetsFor("hermes")).toBe(presets);
  });

  it("每个预设都声明了模型组（客户端只显示声明过的模型）", () => {
    for (const p of presets) {
      expect(p.models?.length, `${p.id} 缺 models`).toBeGreaterThan(0);
      // 单值 model 必须是模型组的成员，否则「兜底模型」指向一个客户端里
      // 选不到的模型。
      expect(p.models, `${p.id} 的 model 不在 models 里`).toContain(p.model);
    }
  });

  it("上下文窗口为正整数（不写就该缺省，不能是 0/负数）", () => {
    for (const p of presets) {
      if (p.contextLength !== undefined) {
        expect(Number.isInteger(p.contextLength), `${p.id}`).toBe(true);
        expect(p.contextLength, `${p.id}`).toBeGreaterThan(0);
      }
    }
  });

  it("智谱走 coding 专属路径（通用路径用不上套餐额度）", () => {
    for (const p of presets.filter((x) => x.id.startsWith("zhipu"))) {
      expect(p.baseUrl, `${p.id}`).toContain("/coding/");
    }
  });

  // 一份预设服务三家客户端，协议只有在三家取值一致时才能写进来。反例：
  // cc-switch 的 Kimi For Coding 在 opencode 是 `/coding/v1` + anthropic 包、
  // 在 openclaw 是 `/v1` + openai-completions，端点与协议两处都不同 ——
  // 硬选一边会让另一边用不上套餐路径。它的 openclaw 预设这六家全是
  // openai-completions，那就是三家的公共最小面。
  it("共享预设不携带 upstreamApi（三家取值不一致的项无法用共享预设表达）", () => {
    for (const p of presets) {
      expect(
        p.upstreamApi,
        `${p.id} 声明了 upstreamApi——先确认 OpenCode/OpenClaw/Hermes 三家取值一致`,
      ).toBeUndefined();
    }
  });
});

describe("claude-desktop 预设专属约束", () => {
  const presets = relayPresetsFor("claude-desktop");
  const VALID_ROUTE_IDS = Object.values(DESKTOP_ROLE_ROUTE_IDS) as string[];

  it("每个预设都声明模式与路由", () => {
    for (const p of presets) {
      expect(["direct", "proxy"], `${p.id} 的模式`).toContain(p.desktopMode);
      expect(p.desktopRoutes?.length, `${p.id} 的路由数`).toBeGreaterThan(0);
    }
  });

  it("routeId 必须是标准四档（fail-all 校验器白名单）", () => {
    for (const p of presets) {
      for (const route of p.desktopRoutes ?? []) {
        expect(VALID_ROUTE_IDS, `${p.id} 的 routeId`).toContain(route.routeId);
        expect(route.upstreamModel, `${p.id} 的上游模型`).not.toBe("");
      }
    }
  });

  it("direct 预设必须透传（Desktop 侧无映射能力）", () => {
    for (const p of presets.filter((p) => p.desktopMode === "direct")) {
      for (const route of p.desktopRoutes ?? []) {
        expect(route.upstreamModel, `${p.id} 的 direct 路由`).toBe(route.routeId);
      }
    }
  });

  it("与 Claude 预设同 id 同端点（派生一致性）", () => {
    const claudeById = new Map(relayPresetsFor("claude").map((p) => [p.id, p]));
    for (const p of presets) {
      const source = claudeById.get(p.id);
      expect(source, `${p.id} 应存在于 Claude 预设`).toBeDefined();
      expect(p.baseUrl, `${p.id} 的端点`).toBe(source?.baseUrl);
    }
  });

  it("默认首推 Compute Token 保持 recommended", () => {
    expect(presets.find((p) => p.id === "computetoken")?.recommended).toBe(true);
  });
});

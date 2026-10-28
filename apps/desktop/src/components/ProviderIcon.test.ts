import { describe, expect, it } from "vitest";
import { relayPresetsFor } from "../lib/providerPresets";
import { hasBuiltinIcon, inferProviderIcon } from "./ProviderIcon";

describe("inferProviderIcon", () => {
  it("按名称子串命中品牌图标（大小写不敏感）", () => {
    expect(inferProviderIcon("DeepSeek")).toBe("deepseek");
    expect(inferProviderIcon("Zhipu GLM")).toBe("zhipu");
    expect(inferProviderIcon("glm-relay")).toBe("zhipu");
    expect(inferProviderIcon("Kimi")).toBe("kimi");
    expect(inferProviderIcon("moonshot-cn")).toBe("kimi");
    expect(inferProviderIcon("阿里云百炼")).toBe("bailian");
    expect(inferProviderIcon("MiniMax")).toBe("minimax");
    expect(inferProviderIcon("OpenRouter")).toBe("openrouter");
  });

  it("新增品牌：豆包/火山、MiMo、Novita、NVIDIA、AiHubMix", () => {
    expect(inferProviderIcon("Doubao Seed")).toBe("doubao");
    expect(inferProviderIcon("火山方舟")).toBe("doubao");
    expect(inferProviderIcon("volcengine-ark")).toBe("doubao");
    expect(inferProviderIcon("Xiaomi MiMo")).toBe("xiaomimimo");
    expect(inferProviderIcon("小米大模型")).toBe("xiaomimimo");
    expect(inferProviderIcon("Novita AI")).toBe("novita");
    expect(inferProviderIcon("NVIDIA NIM")).toBe("nvidia");
    expect(inferProviderIcon("AiHubMix")).toBe("aihubmix");
  });

  it("claude/codex 别名归到官方品牌", () => {
    expect(inferProviderIcon("Claude 官方")).toBe("anthropic");
    expect(inferProviderIcon("codex mirror")).toBe("openai");
  });

  it("无匹配返回 null（走首字母兜底）", () => {
    expect(inferProviderIcon("My Relay")).toBeNull();
    expect(inferProviderIcon("")).toBeNull();
  });
});

describe("图标注册表守卫", () => {
  it("所有预设携带的 icon key 都有内置 SVG", () => {
    for (const app of ["claude", "codex"] as const) {
      for (const p of relayPresetsFor(app)) {
        if (p.icon) {
          expect(hasBuiltinIcon(p.icon), `预设 ${p.id} 的图标 ${p.icon} 缺 SVG`).toBe(true);
        }
      }
    }
  });
});

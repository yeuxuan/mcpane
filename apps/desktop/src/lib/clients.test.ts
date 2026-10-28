import { describe, expect, it } from "vitest";
import { BRAND_NAMES } from "../components/BrandIcon";
import { CLIENT_META, CLIENTS, hasOfficial, isAdditiveApp, PROVIDER_APPS } from "./clients";
import { CLIENT_TYPE_IDS } from "./ipc";

/**
 * 这张表是客户端元数据的单一事实源，新增客户端只该改它。
 * 下面几条守卫的是"改了表却漏了配套"——尤其是品牌图标：`BrandName` 与
 * `ClientApp` 是两个类型，缺图标时 TypeScript 不会报错，只会在界面上渲染成空白。
 */
describe("客户端元数据表", () => {
  it("每个客户端都有同名品牌图标", () => {
    for (const meta of CLIENTS) {
      expect(BRAND_NAMES, `${meta.app} 缺品牌图标`).toContain(meta.app);
    }
  });

  /**
   * 客户端页的 MCP 客户端是另一套 id（后端 `mcp_clients::CLIENTS` + claude/codex
   * 两个专用 adapter），漏配图标同样不报错——`isBrandName` 认不出就不画，
   * 卡片上留一个空色块，得有人肉眼发现。
   */
  it("每个内置 MCP 客户端 id 都有品牌图标", () => {
    for (const id of CLIENT_TYPE_IDS) {
      expect(BRAND_NAMES, `${id} 缺品牌图标`).toContain(id);
    }
  });

  it("app 唯一，官方 id 唯一", () => {
    const apps = CLIENTS.map((m) => m.app);
    expect(new Set(apps).size).toBe(apps.length);
    const ids = CLIENTS.map((m) => m.officialId);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("官方 id 与后端约定的 official-<app> 一致", () => {
    for (const meta of CLIENTS) {
      expect(meta.officialId).toBe(`official-${meta.app}`);
    }
  });

  it("additive 客户端没有官方形态", () => {
    for (const meta of CLIENTS) {
      expect(hasOfficial(meta.app)).toBe(!isAdditiveApp(meta.app));
    }
  });

  it("协议维度只属于配置里真有这个字段的客户端", () => {
    for (const meta of CLIENTS) {
      if (meta.supportsUpstreamApi) {
        expect(meta.additive, `${meta.app}: 协议维度目前只存在于 additive 客户端`).toBe(true);
      }
    }
    // 三家 additive 都有协议字段，只是名字不同：OpenCode `npm`、
    // OpenClaw `api`、Hermes `api_mode`（cc-switch 对 Hermes 恒显式写出）。
    expect(CLIENT_META.opencode.supportsUpstreamApi).toBe(true);
    expect(CLIENT_META.openclaw.supportsUpstreamApi).toBe(true);
    expect(CLIENT_META.hermes.supportsUpstreamApi).toBe(true);
  });

  it("上下文长度维度属于 Hermes 与 OpenClaw（切换式客户端配置里没有这个字段）", () => {
    // Hermes `models.<id>.context_length`、OpenClaw `models[].contextWindow`。
    expect(CLIENT_META.hermes.supportsContextLength).toBe(true);
    expect(CLIENT_META.openclaw.supportsContextLength).toBe(true);
    for (const meta of CLIENTS) {
      if (meta.app !== "hermes" && meta.app !== "openclaw") {
        expect(meta.supportsContextLength, `${meta.app} 不该有这一维度`).toBeFalsy();
      }
    }
  });

  it("都有安装引导入口（GUI 应用可以没有命令行安装方式）", () => {
    for (const meta of CLIENTS) {
      expect(meta.install.url, `${meta.app} 缺官网`).toMatch(/^https:\/\//);
    }
  });

  it("PROVIDER_APPS 与 CLIENT_META 同集同序", () => {
    expect(PROVIDER_APPS).toEqual(CLIENTS.map((m) => m.app));
    for (const app of PROVIDER_APPS) {
      expect(CLIENT_META[app].app).toBe(app);
    }
  });
});

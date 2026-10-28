import i18next from "i18next";
import { describe, expect, it } from "vitest";
import "../lib/i18n";
import type { CatalogTool } from "../lib/ipc";
import {
  buildServerSnippet,
  joinArgs,
  mcpaneHubSlug,
  mergeServerTools,
  parseServerSnippet,
  recordToRows,
  rowsToRecord,
  splitArgs,
} from "./ServerSetupModal";

/** 用真实的 t：既省得造假，又顺带验证解析错误的文案确实接上了 i18n。 */
const t = (key: string, options?: Record<string, unknown>) => i18next.t(key, options);

// 与后端 commands.rs mcpane_hub_slug 同规则（前端仅做预览，落库以后端为准）
describe("mcpaneHubSlug", () => {
  it("小写化并折叠非字母数字为连字符", () => {
    expect(mcpaneHubSlug("Context7")).toBe("context7");
    expect(mcpaneHubSlug("My MCP Server!")).toBe("my-mcp-server");
    expect(mcpaneHubSlug("a__b..c")).toBe("a-b-c");
  });

  it("去除首尾连字符，空结果回退为 server", () => {
    expect(mcpaneHubSlug("--hello--")).toBe("hello");
    expect(mcpaneHubSlug("！！！")).toBe("server");
    expect(mcpaneHubSlug("")).toBe("server");
  });
});

describe("键值行序列化", () => {
  it("rowsToRecord 跳过空 key 并去首尾空白", () => {
    const rows = recordToRows({ API_KEY: "secret" });
    rows.push({ rowId: 999, key: "  X  ", value: "v" }, { rowId: 1000, key: "   ", value: "忽略" });
    expect(rowsToRecord(rows)).toEqual({ API_KEY: "secret", X: "v" });
  });

  it("recordToRows 与 rowsToRecord 往返一致", () => {
    const record = { A: "1", B: "2" };
    expect(rowsToRecord(recordToRows(record))).toEqual(record);
  });
});

describe("mergeServerTools", () => {
  const catalog: CatalogTool[] = [
    {
      serverId: "srv1",
      serverSlug: "gh",
      originalName: "search",
      exposedName: "gh__search",
      description: "搜索",
      schemaHash: "h1",
    },
    {
      serverId: "srv2",
      serverSlug: "fs",
      originalName: "read",
      exposedName: "fs__read",
      description: null,
      schemaHash: "h2",
    },
  ];

  it("只取本 server 的启用工具，并补上禁用覆盖行", () => {
    const overrides = [
      {
        serverId: "srv1",
        originalName: "dangerous",
        enabled: false,
        alias: null,
        descriptionSuffix: null,
      },
      {
        serverId: "srv2",
        originalName: "other",
        enabled: false,
        alias: null,
        descriptionSuffix: null,
      },
      {
        serverId: "srv1",
        originalName: "search",
        enabled: true,
        alias: "gh_search",
        descriptionSuffix: null,
      },
    ];
    const rows = mergeServerTools(catalog, overrides, "srv1", "gh");
    expect(rows.map((r) => [r.originalName, r.enabled])).toEqual([
      ["dangerous", false],
      ["search", true],
    ]);
    expect(rows[0]?.exposedName).toBe("gh__dangerous");
  });

  it("禁用行与启用行同名时不重复", () => {
    const overrides = [
      {
        serverId: "srv1",
        originalName: "search",
        enabled: false,
        alias: null,
        descriptionSuffix: null,
      },
    ];
    // catalog 里还在（Hub 尚未重建）——以 catalog 为准只出现一次
    const rows = mergeServerTools(catalog, overrides, "srv1", "gh");
    expect(rows).toHaveLength(1);
  });
});

describe("JSON 片段构建/解析", () => {
  it("stdio 表单 → 片段 → 表单 往返", () => {
    const snippet = buildServerSnippet({
      name: "puppeteer",
      transport: "stdio",
      command: "npx",
      args: "-y demo-mcp",
      env: { KEY: "v" },
      cwd: "/tmp/p",
      url: "",
      bearer: "",
      bearerEnv: "",
      headers: {},
    });
    const parsed = JSON.parse(snippet);
    expect(parsed.mcpServers.puppeteer.command).toBe("npx");
    expect(parsed.mcpServers.puppeteer.args).toEqual(["-y", "demo-mcp"]);

    const form = parseServerSnippet(snippet, t);
    expect(form.name).toBe("puppeteer");
    expect(form.transport).toBe("stdio");
    expect(form.args).toBe("-y demo-mcp");
    expect(form.env).toEqual({ KEY: "v" });
    expect(form.cwd).toBe("/tmp/p");
  });

  it("http 片段包含 bearer/headers 且可解析（含 bearer_env 蛇形兼容）", () => {
    const snippet = buildServerSnippet({
      name: "remote",
      transport: "http",
      command: "",
      args: "",
      env: {},
      cwd: "",
      url: "https://example.com/mcp",
      bearer: "tok",
      bearerEnv: "MY_TOKEN",
      headers: { "X-API-Key": "k" },
    });
    const form = parseServerSnippet(snippet, t);
    expect(form.transport).toBe("http");
    expect(form.bearer).toBe("tok");
    expect(form.bearerEnv).toBe("MY_TOKEN");
    expect(form.headers).toEqual({ "X-API-Key": "k" });

    const snake = parseServerSnippet(
      '{"remote": {"url": "https://e.com/mcp", "bearer_env": "TOK"}}',
      t,
    );
    expect(snake.bearerEnv).toBe("TOK");
  });

  it("sse 片段带 type 标注且往返保持；未标注的远程 URL 仍按 http", () => {
    const snippet = buildServerSnippet({
      name: "legacy",
      transport: "sse",
      command: "",
      args: "",
      env: {},
      cwd: "",
      url: "https://example.com/sse",
      bearer: "",
      bearerEnv: "",
      headers: {},
    });
    expect(JSON.parse(snippet).mcpServers.legacy.type).toBe("sse");
    expect(parseServerSnippet(snippet, t).transport).toBe("sse");

    // "transport" 键写法（部分客户端）同样识别。
    expect(
      parseServerSnippet('{"x": {"url": "https://e.com/sse", "transport": "sse"}}', t).transport,
    ).toBe("sse");
    // 未标注 → Streamable HTTP。
    expect(parseServerSnippet('{"x": {"url": "https://e.com/mcp"}}', t).transport).toBe("http");
  });

  it("裸 map 与 mcpServers 包裹层都能解析", () => {
    expect(parseServerSnippet('{"a": {"command": "npx"}}', t).name).toBe("a");
    expect(parseServerSnippet('{"mcpServers": {"b": {"url": "https://x/mcp"}}}', t).name).toBe("b");
  });

  it("非法输入抛中文错误", () => {
    expect(() => parseServerSnippet("not json", t)).toThrow(/JSON 解析失败/);
    expect(() => parseServerSnippet("[1]", t)).toThrow(/顶层应为 JSON 对象/);
    expect(() => parseServerSnippet("{}", t)).toThrow(/没有可解析的服务器条目/);
    expect(() => parseServerSnippet('{"x": {"note": "无 command/url"}}', t)).toThrow(
      /command（stdio）或 url（HTTP）/,
    );
  });
});

/**
 * 参数里的空格是真会发生的：macOS 上 `~/Library/Application Support`、
 * Windows 上 `C:\Program Files\...`，而 filesystem / git 这类 Server 收的正是
 * 路径。此前是朴素 `split(" ")`，这类路径会被静默切成两个参数。
 */
describe("splitArgs / joinArgs", () => {
  it("按空格切分普通参数", () => {
    expect(splitArgs("-y @modelcontextprotocol/server-memory")).toEqual([
      "-y",
      "@modelcontextprotocol/server-memory",
    ]);
  });

  it("引号内的空格不切", () => {
    expect(splitArgs('-y pkg "/Users/me/Library/Application Support"')).toEqual([
      "-y",
      "pkg",
      "/Users/me/Library/Application Support",
    ]);
  });

  it("多余空白与换行不产生空参数", () => {
    expect(splitArgs("  -y   pkg \n ")).toEqual(["-y", "pkg"]);
    expect(splitArgs("")).toEqual([]);
    expect(splitArgs("   ")).toEqual([]);
  });

  it("显式的空字符串参数得以保留", () => {
    expect(splitArgs('--flag "" next')).toEqual(["--flag", "", "next"]);
  });

  it("含空格的参数往返不损坏", () => {
    const args = ["-y", "pkg", "/Users/me/My Projects/repo"];
    expect(splitArgs(joinArgs(args))).toEqual(args);
  });

  it("含引号与反斜杠的参数往返不损坏", () => {
    const args = ['--json={"a": 1}', String.raw`C:\Program Files\tool`];
    expect(splitArgs(joinArgs(args))).toEqual(args);
  });

  /**
   * 往返测试测不到这条：joinArgs 会主动加引号，绕开了「用户直接手输」这条路。
   * Windows 用户往参数框里贴的就是裸路径，反斜杠必须原样留下——
   * 按 POSIX shell 那套把 `\` 当转义符的话，这里会被吃成 `C:Usersme`。
   */
  it("裸 Windows 路径的反斜杠原样保留", () => {
    expect(splitArgs(String.raw`C:\Users\me\project`)).toEqual([String.raw`C:\Users\me\project`]);
    expect(splitArgs(String.raw`--repository C:\tools\repo`)).toEqual([
      "--repository",
      String.raw`C:\tools\repo`,
    ]);
  });

  /**
   * 「引号包着、以路径分隔符结尾」在 Windows 上很常见。把 `\` 当转义符的话，
   * 这里的 `\"` 会被吃成字面引号、引号就闭合不上了。
   */
  it("引号包着以反斜杠结尾的路径也能还原", () => {
    expect(splitArgs('"C:\\My Docs\\"')).toEqual(["C:\\My Docs\\"]);
  });

  it('引号内的 "" 是一个字面引号', () => {
    expect(splitArgs('"--json={""a"":1}"')).toEqual(['--json={"a":1}']);
  });

  /** 未闭合的引号不该吞掉后面的一切或抛错——尽力而为地收尾。 */
  it("未闭合的引号不崩", () => {
    expect(splitArgs('-y pkg "unclosed')).toEqual(["-y", "pkg", "unclosed"]);
  });

  it("不含特殊字符时不多加引号", () => {
    expect(joinArgs(["-y", "pkg"])).toBe("-y pkg");
  });

  it("JSON 片段里的路径参数保持完整", () => {
    const snippet = buildServerSnippet({
      name: "filesystem",
      transport: "stdio",
      command: "npx",
      args: '-y @modelcontextprotocol/server-filesystem "/Users/me/My Docs"',
      env: {},
      cwd: "",
      url: "",
      bearer: "",
      bearerEnv: "",
      headers: {},
    });
    expect(JSON.parse(snippet).mcpServers.filesystem.args).toEqual([
      "-y",
      "@modelcontextprotocol/server-filesystem",
      "/Users/me/My Docs",
    ]);
  });
});

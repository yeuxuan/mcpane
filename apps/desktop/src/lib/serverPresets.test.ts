import { expect, test } from "vitest";
import {
  applyPreset,
  presetInputsSatisfied,
  RUNTIME_LABEL_KEY,
  SERVER_PRESETS,
  type ServerPreset,
} from "./serverPresets";

function byId(id: string): ServerPreset {
  const preset = SERVER_PRESETS.find((p) => p.id === id);
  if (!preset) throw new Error(`预设 ${id} 不存在`);
  return preset;
}

/**
 * 本文件最重要的一条。
 *
 * 直觉上最该做预设的那批（github / puppeteer / slack / postgres…）**恰恰全被
 * 官方归档了**，凭印象加进来 = 让用户装一个没人维护的包，比不给预设更糟。
 * 上游把它们移出了 `src/`，进了 `servers-archived`。
 */
test("不收录任何已归档的官方包", () => {
  const archived = [
    "@modelcontextprotocol/server-github",
    "@modelcontextprotocol/server-puppeteer",
    "@modelcontextprotocol/server-slack",
    "@modelcontextprotocol/server-postgres",
    "@modelcontextprotocol/server-sqlite",
    "@modelcontextprotocol/server-gitlab",
    "@modelcontextprotocol/server-google-maps",
    "@modelcontextprotocol/server-brave-search",
  ];
  // 连 command / url 一起扫：归档包名写进任一字段都算，只扫 args 会漏。
  const shipped = SERVER_PRESETS.flatMap((p) => [...(p.args ?? []), p.command ?? "", p.url ?? ""]);
  for (const pkg of archived) {
    expect(shipped, `${pkg} 已被官方归档，不该出现在预设里`).not.toContain(pkg);
  }
});

/** GitHub 走官方远端 HTTP：npm 上那个已停止支持，而远端端点零本地依赖。 */
test("GitHub 预设指向官方远端端点而非归档的 npm 包", () => {
  const github = byId("github");
  expect(github.transport).toBe("http");
  expect(github.url).toBe("https://api.githubcopilot.com/mcp/");
  expect(github.runtime).toBe("none");
  expect(github.command).toBeUndefined();
});

test("id 唯一", () => {
  const ids = SERVER_PRESETS.map((p) => p.id);
  expect(new Set(ids).size).toBe(ids.length);
});

/** 每条预设都得能真的跑起来：stdio 要有命令，http 要有地址。 */
test("每条预设的必要字段都齐全", () => {
  for (const preset of SERVER_PRESETS) {
    expect(preset.name, `${preset.id} 缺 name`).toBeTruthy();
    expect(preset.descriptionKey, `${preset.id} 缺 descriptionKey`).toBeTruthy();
    expect(preset.docsUrl, `${preset.id} 缺 docsUrl`).toMatch(/^https:\/\//);
    if (preset.transport === "stdio") {
      expect(preset.command, `${preset.id} 是 stdio 却没有命令`).toBeTruthy();
      expect(preset.url, `${preset.id} 是 stdio 不该有 url`).toBeUndefined();
    } else {
      expect(preset.url, `${preset.id} 是 http 却没有地址`).toMatch(/^https:\/\//);
    }
  }
});

/** runtime 标注错了，UI 会把前置条件说反。 */
test("runtime 与命令自洽", () => {
  for (const preset of SERVER_PRESETS) {
    if (preset.command === "npx") expect(preset.runtime, preset.id).toBe("node");
    if (preset.command === "uvx") expect(preset.runtime, preset.id).toBe("uv");
    if (preset.transport === "http") expect(preset.runtime, preset.id).toBe("none");
    expect(RUNTIME_LABEL_KEY[preset.runtime]).toBeTruthy();
  }
});

/** env 类输入必须给出变量名，否则值无处可去、静默丢失。 */
test("env 类输入都带变量名", () => {
  for (const preset of SERVER_PRESETS) {
    for (const input of preset.inputs ?? []) {
      if (input.kind === "env")
        expect(input.envKey, `${preset.id} 的 env 输入缺 envKey`).toBeTruthy();
    }
  }
});

test("无参数的预设一键即可添加", () => {
  const preset = byId("sequentialthinking");
  expect(presetInputsSatisfied(preset, [])).toBe(true);

  const form = applyPreset(preset, []);
  expect(form.command).toBe("npx");
  expect(form.args).toEqual(["-y", "@modelcontextprotocol/server-sequential-thinking"]);
});

test("用户填的目录追加在固定参数之后", () => {
  const form = applyPreset(byId("filesystem"), ["/Users/me/code"]);
  expect(form.args).toEqual(["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/code"]);
});

/** git 的路径要紧跟在 --repository 后面，顺序错了参数就废了。 */
test("git 的仓库路径接在 --repository 之后", () => {
  const form = applyPreset(byId("git"), ["/Users/me/repo"]);
  expect(form.args).toEqual(["mcp-server-git", "--repository", "/Users/me/repo"]);
});

test("bearer 类输入落进 bearer 而非参数", () => {
  const form = applyPreset(byId("github"), ["ghp_secret"]);
  expect(form.bearer).toBe("ghp_secret");
  expect(form.url).toBe("https://api.githubcopilot.com/mcp/");
  expect(form.args).toEqual([]);
});

test("env 类输入落进 env", () => {
  const form = applyPreset(byId("memory"), ["/tmp/mem.json"]);
  expect(form.env).toEqual({ MEMORY_FILE_PATH: "/tmp/mem.json" });
  expect(form.args).toEqual(["-y", "@modelcontextprotocol/server-memory"]);
});

/**
 * 空字符串传给 npx 会变成一个多余的空 argv，和「不传」不是一回事。
 * memory 的 MEMORY_FILE_PATH 是可选的，留空必须当没填。
 */
test("留空的可选项被丢弃，不落成空参数或空环境变量", () => {
  const form = applyPreset(byId("memory"), [""]);
  expect(form.env).toEqual({});
  expect(form.args).toEqual(["-y", "@modelcontextprotocol/server-memory"]);
});

test("首尾空白被裁掉", () => {
  const form = applyPreset(byId("filesystem"), ["  /Users/me/code  "]);
  expect(form.args.at(-1)).toBe("/Users/me/code");
});

test("必填项没填时拦住添加，可选项不拦", () => {
  expect(presetInputsSatisfied(byId("filesystem"), [""])).toBe(false);
  expect(presetInputsSatisfied(byId("filesystem"), ["   "])).toBe(false);
  expect(presetInputsSatisfied(byId("filesystem"), ["/tmp"])).toBe(true);
  // memory 的唯一输入是可选的。
  expect(presetInputsSatisfied(byId("memory"), [""])).toBe(true);
});

/** 预设不该改动 SERVER_PRESETS 里的数组——组装一次就把源数据弄脏了。 */
test("组装表单不会污染预设本身", () => {
  const preset = byId("filesystem");
  const before = [...(preset.args ?? [])];
  applyPreset(preset, ["/tmp/a"]);
  applyPreset(preset, ["/tmp/b"]);
  expect(preset.args).toEqual(before);
});

/**
 * MCP Server 预设。
 *
 * ## 收录原则
 *
 * 只收**官方**：modelcontextprotocol/servers 里仍在维护的 reference server，
 * 以及厂商自家维护的官方 server（GitHub）。不收第三方商业服务——那等于替用户
 * 挑赢家，且与供应商预设「拒收推广中转站」的口径一致（见 providerPresets.ts）。
 *
 * ## 已归档的一律不收（这里最容易凭直觉写错）
 *
 * 直觉上最该做预设的那批，**恰恰全是归档的**：
 *
 * | 包 | 状态 |
 * |---|---|
 * | `@modelcontextprotocol/server-github` | 归档，2025-04 起停止支持；官方替代见下方 github 预设 |
 * | `@modelcontextprotocol/server-puppeteer` | 归档 |
 * | `@modelcontextprotocol/server-slack` | 归档（现由 Zencoder 维护，非官方） |
 * | `@modelcontextprotocol/server-postgres` | 归档 |
 * | `@modelcontextprotocol/server-sqlite` | 归档 |
 * | `@modelcontextprotocol/server-gitlab` | 归档 |
 * | `@modelcontextprotocol/server-google-maps` | 归档 |
 * | `@modelcontextprotocol/server-brave-search` | 归档（Brave 另有官方 server） |
 *
 * 它们都已移出 `src/` 进 `servers-archived`。收录 = 让用户装一个没人维护的包，
 * 比不给预设更糟。**新增预设前务必先核对上游是否仍在 `src/` 下**，别凭印象。
 *
 * ## 不要照抄任何现成预设表（实测：抄了会错）
 *
 * 某竞品预设里 `time` 一条写的是
 * `npx -y @modelcontextprotocol/server-time` —— **该 npm 包不存在**
 *（`registry.npmjs.org` 返回 404），点了只会得到 `npm error 404`。官方的 time
 * server 是 Python 包，走 `uvx mcp-server-time`（它自己的 docs 链接指的正是
 * Python 那个，包名与链接自相矛盾）。另有竞品还在拿已归档的
 * puppeteer 当范例。别家的清单可能是照记忆写的，不是事实源。
 *
 * ## 核验方法（新增/改动预设时照做一遍，2026-07-15 全表验过）
 *
 * - npm：`curl -o /dev/null -w "%{http_code}" https://registry.npmjs.org/<包名>` → 200
 * - PyPI（uvx）：`curl -o /dev/null -w "%{http_code}" https://pypi.org/pypi/<包名>/json` → 200
 * - HTTP 端点：curl 该 URL，**401 是对的**（端点活着、要鉴权），404 才是地址写错
 * - 是否仍在维护：确认它还在 modelcontextprotocol/servers 的 `src/` 下
 *
 * 这些不做成自动化测试——那会让 CI 依赖外网且受 registry 抖动影响。
 * `serverPresets.test.ts` 只锁「不收已归档包」这条离线可判的红线。
 *
 * ## 运行时前置
 *
 * - `npx`：需要 Node.js；
 * - `uvx`：需要 uv（astral.sh/uv）；
 * - `http`：无本地依赖 —— 这也是 GitHub 预设优先走远端端点的原因。
 */

/** 预设需要用户填的一项参数。 */
interface PresetInput {
  /** 填进哪里：stdio 的某个参数占位符，或 http 的 bearer。 */
  kind: "arg" | "bearer" | "env";
  /** kind=env 时的变量名。 */
  envKey?: string;
  /** i18n key（见 lib/i18n.ts 的 serverPreset.*）。 */
  labelKey: string;
  /** 占位符字面量（示例路径 / Token 前缀之类，不必翻译）。 */
  placeholder?: string;
  /** 需要翻译的占位符走 key（如「留空则用默认位置」）。 */
  placeholderKey?: string;
  /** i18n key。 */
  hintKey?: string;
  /** 缺省值（如 filesystem 的目录）。 */
  defaultValue?: string;
  /** 留空是否可以（可选参数）。 */
  optional?: boolean;
}

export type PresetRuntime = "node" | "uv" | "none";

export interface ServerPreset {
  id: string;
  /** 默认 server 名（用户可改）。不翻译——它同时是 Server 的标识。 */
  name: string;
  /** i18n key（见 lib/i18n.ts 的 serverPreset.<id>.desc）。 */
  descriptionKey: string;
  /** 上游文档，供「了解更多」外链。 */
  docsUrl: string;
  runtime: PresetRuntime;
  transport: "stdio" | "http" | "sse";
  /** stdio：命令与固定参数。用户参数按 inputs 顺序追加在其后。 */
  command?: string;
  args?: string[];
  /** http：端点。 */
  url?: string;
  /** 需要用户填的参数；无则一键即可添加。 */
  inputs?: PresetInput[];
}

/**
 * 官方 reference server 中仍在维护的全部 7 个，加 GitHub 官方远端。
 * 顺序按「零参数、装完即用」优先，让最容易成功的排在最前。
 */
export const SERVER_PRESETS: ServerPreset[] = [
  {
    id: "sequentialthinking",
    name: "sequential-thinking",
    descriptionKey: "serverPreset.sequentialthinking.desc",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/sequentialthinking",
    runtime: "node",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-sequential-thinking"],
  },
  {
    id: "fetch",
    name: "fetch",
    descriptionKey: "serverPreset.fetch.desc",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch",
    runtime: "uv",
    transport: "stdio",
    command: "uvx",
    args: ["mcp-server-fetch"],
  },
  {
    id: "time",
    name: "time",
    descriptionKey: "serverPreset.time.desc",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/time",
    runtime: "uv",
    transport: "stdio",
    command: "uvx",
    args: ["mcp-server-time"],
  },
  {
    id: "memory",
    name: "memory",
    descriptionKey: "serverPreset.memory.desc",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/memory",
    runtime: "node",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-memory"],
    inputs: [
      {
        kind: "env",
        envKey: "MEMORY_FILE_PATH",
        labelKey: "serverPreset.memory.input.path",
        placeholderKey: "serverPreset.memory.input.pathPlaceholder",
        hintKey: "serverPreset.memory.input.pathHint",
        optional: true,
      },
    ],
  },
  {
    id: "filesystem",
    name: "filesystem",
    descriptionKey: "serverPreset.filesystem.desc",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem",
    runtime: "node",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-filesystem"],
    inputs: [
      {
        kind: "arg",
        labelKey: "serverPreset.filesystem.input.dir",
        placeholder: "/Users/you/projects",
        // 上游支持多个目录（`server-filesystem /a /b`）。这里只收一个，
        // 因为预设填完会落回参数框，要更多目录在那里追加即可——限制是软的。
        hintKey: "serverPreset.filesystem.input.dirHint",
      },
    ],
  },
  {
    id: "git",
    name: "git",
    descriptionKey: "serverPreset.git.desc",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/git",
    runtime: "uv",
    transport: "stdio",
    command: "uvx",
    args: ["mcp-server-git", "--repository"],
    inputs: [
      {
        kind: "arg",
        labelKey: "serverPreset.git.input.repo",
        placeholder: "/Users/you/projects/my-repo",
      },
    ],
  },
  {
    id: "github",
    name: "github",
    // 走官方远端端点而非本地 Docker：不装 Docker、不装 Node 即可用，
    // 且 npm 上那个 @modelcontextprotocol/server-github 已于 2025-04 停止支持。
    descriptionKey: "serverPreset.github.desc",
    docsUrl: "https://github.com/github/github-mcp-server",
    runtime: "none",
    transport: "http",
    url: "https://api.githubcopilot.com/mcp/",
    inputs: [
      {
        kind: "bearer",
        labelKey: "serverPreset.github.input.token",
        placeholder: "github_pat_… / ghp_…",
        hintKey: "serverPreset.github.input.tokenHint",
      },
    ],
  },
  {
    id: "everything",
    name: "everything",
    descriptionKey: "serverPreset.everything.desc",
    docsUrl: "https://github.com/modelcontextprotocol/servers/tree/main/src/everything",
    runtime: "node",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-everything"],
  },
];

/** 运行时前置的 i18n key（UI 提示用）。 */
export const RUNTIME_LABEL_KEY: Record<PresetRuntime, string> = {
  node: "serverPreset.runtime.node",
  uv: "serverPreset.runtime.uv",
  none: "serverPreset.runtime.none",
};

/**
 * 把预设 + 用户填的值组装成表单初值。
 *
 * `values` 按 `preset.inputs` 同序；留空的可选项直接丢弃，不落成空参数
 * ——空字符串传给 npx 会变成一个多余的空 argv，行为与「不传」并不等价。
 */
export function applyPreset(
  preset: ServerPreset,
  values: string[],
): {
  name: string;
  transport: "stdio" | "http" | "sse";
  command: string;
  args: string[];
  url: string;
  bearer: string;
  env: Record<string, string>;
} {
  const args = [...(preset.args ?? [])];
  const env: Record<string, string> = {};
  let bearer = "";

  (preset.inputs ?? []).forEach((input, index) => {
    const value = (values[index] ?? "").trim();
    if (!value) return;
    if (input.kind === "arg") args.push(value);
    else if (input.kind === "bearer") bearer = value;
    else if (input.kind === "env" && input.envKey) env[input.envKey] = value;
  });

  return {
    name: preset.name,
    transport: preset.transport,
    command: preset.command ?? "",
    args,
    url: preset.url ?? "",
    bearer,
    env,
  };
}

/** 必填项是否都填了（决定「添加」按钮可否点）。 */
export function presetInputsSatisfied(preset: ServerPreset, values: string[]): boolean {
  return (preset.inputs ?? []).every(
    (input, index) => input.optional || (values[index] ?? "").trim().length > 0,
  );
}

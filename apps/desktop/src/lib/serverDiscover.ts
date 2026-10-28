import type { PresetFill } from "../components/ServerPresetPicker";
import type { DiscoverCandidate, DiscoverField, DiscoverInstall } from "./ipc";

/** 运行时提示复用预设的文案 key。 */
export const DISCOVER_RUNTIME_LABEL_KEY = {
  node: "serverPreset.runtime.node",
  uv: "serverPreset.runtime.uv",
} as const;

/** 候选的「启动方式」一行摘要，展示给用户核对。 */
export function installSummary(install: DiscoverInstall): string {
  switch (install.kind) {
    case "stdio":
      return [install.command, ...install.args].join(" ");
    case "remote":
      return install.url;
    case "unsupported":
      return install.reason;
  }
}

/** 表单里要预填的行：必填或有值的才带。可选且无值的空串会被部分服务器当成「已配置」，不预填。 */
function prefilled(fields: DiscoverField[]): DiscoverField[] {
  return fields.filter((f) => f.required || f.value !== "");
}

/** 可选但没预填的变量名，提示给用户（如 `CONTEXT7_API_KEY`）。 */
export function optionalFieldNames(install: DiscoverInstall): string[] {
  if (install.kind === "unsupported") return [];
  const fields = install.kind === "stdio" ? install.env : install.headers;
  return fields.filter((f) => !f.required && f.value === "").map((f) => f.name);
}

function toRecord(fields: DiscoverField[]): Record<string, string> {
  return Object.fromEntries(prefilled(fields).map((f) => [f.name, f.value]));
}

/** 候选 → 表单预填值；暂不支持的候选返回 null（界面不给「使用」按钮）。 */
export function candidateToFill(candidate: DiscoverCandidate): PresetFill | null {
  const { install } = candidate;
  switch (install.kind) {
    case "stdio":
      return {
        name: candidate.name,
        transport: "stdio",
        command: install.command,
        args: install.args,
        url: "",
        bearer: "",
        env: toRecord(install.env),
      };
    case "remote":
      return {
        name: candidate.name,
        transport: install.transport === "sse" ? "sse" : "http",
        command: "",
        args: [],
        url: install.url,
        bearer: "",
        env: {},
        headers: toRecord(install.headers),
      };
    case "unsupported":
      return null;
  }
}

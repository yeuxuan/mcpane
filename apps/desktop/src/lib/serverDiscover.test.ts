import { expect, test } from "vitest";
import type { DiscoverCandidate, DiscoverInstall } from "./ipc";
import { candidateToFill, installSummary, optionalFieldNames } from "./serverDiscover";

function candidate(install: DiscoverInstall): DiscoverCandidate {
  return {
    id: "x#0",
    source: "registry",
    name: "context7",
    title: null,
    description: null,
    namespace: "io.github.upstash",
    repositoryUrl: null,
    websiteUrl: null,
    exact: true,
    install,
    todo: [],
  };
}

const field = (name: string, over: Partial<{ value: string; required: boolean }> = {}) => ({
  name,
  value: "",
  secret: true,
  required: false,
  description: null,
  ...over,
});

test("stdio 候选填成命令与参数，必填变量留空行、可选且无值的不预填", () => {
  const c = candidate({
    kind: "stdio",
    runtime: "node",
    command: "npx",
    args: ["-y", "@upstash/context7-mcp@4.1.1"],
    env: [
      field("API_KEY", { required: true }),
      field("OPTIONAL_KEY"),
      field("MODE", { value: "x" }),
    ],
  });
  expect(candidateToFill(c)).toEqual({
    name: "context7",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@upstash/context7-mcp@4.1.1"],
    url: "",
    bearer: "",
    env: { API_KEY: "", MODE: "x" },
  });
  expect(optionalFieldNames(c.install)).toEqual(["OPTIONAL_KEY"]);
});

test("远程候选：http / sse 各归各的传输，不带命令", () => {
  const http = candidateToFill(
    candidate({
      kind: "remote",
      transport: "http",
      url: "https://mcp.notion.com/mcp",
      headers: [],
    }),
  );
  expect(http).toMatchObject({ transport: "http", url: "https://mcp.notion.com/mcp", command: "" });
  const sse = candidateToFill(
    candidate({ kind: "remote", transport: "sse", url: "https://x.example/sse", headers: [] }),
  );
  expect(sse?.transport).toBe("sse");
});

test("暂不支持的候选没有预填值", () => {
  expect(
    candidateToFill(candidate({ kind: "unsupported", reason: "暂不支持 NuGet 类型的包" })),
  ).toBeNull();
});

test("启动方式摘要就是用户会在表单里看到的完整命令", () => {
  expect(
    installSummary({
      kind: "stdio",
      runtime: "uv",
      command: "uvx",
      args: ["--from", "a==1", "a"],
      env: [],
    }),
  ).toBe("uvx --from a==1 a");
  expect(
    installSummary({
      kind: "remote",
      transport: "http",
      url: "https://a.example/mcp",
      headers: [],
    }),
  ).toBe("https://a.example/mcp");
});

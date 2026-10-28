import { describe, expect, it } from "vitest";
import { clientDisplayName } from "../lib/clients";
import { groupConnections } from "./Projects";

describe("工作区卡上的客户端 chip", () => {
  it("同一客户端多条连接合成一个 chip，顺序按首次出现", () => {
    const groups = groupConnections([
      { clientConnectionId: "a", clientType: "codex", label: "Codex", tokenPrefix: "aaaa" },
      { clientConnectionId: "b", clientType: "claude", label: "Claude Code", tokenPrefix: "bbbb" },
      { clientConnectionId: "c", clientType: "codex", label: "Codex", tokenPrefix: "cccc" },
    ]);
    expect(groups.map((g) => [g.clientType, g.items.length])).toEqual([
      ["codex", 2],
      ["claude", 1],
    ]);
  });

  it("显示名：专用探测的两家写死，其余取 mcp_clients_status，自定义客户端也能翻出来", () => {
    const list = [
      { id: "cursor", display: "Cursor" },
      { id: "my-tool", display: "我的工具" },
    ];
    expect(clientDisplayName("claude", list)).toBe("Claude Code");
    expect(clientDisplayName("codex", undefined)).toBe("Codex");
    expect(clientDisplayName("cursor", list)).toBe("Cursor");
    expect(clientDisplayName("my-tool", list)).toBe("我的工具");
    // 查不到时退回原始 id，不留空白
    expect(clientDisplayName("ghost", list)).toBe("ghost");
  });
});

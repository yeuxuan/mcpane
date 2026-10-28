import { describe, expect, it } from "vitest";
import type { HubConnection } from "../lib/ipc";
import {
  clientVisibilityForServer,
  groupConnectionsByType,
  visibilityCounts,
} from "./ServerVisibility";

function conn(id: string, clientType: string, enabled = true): HubConnection {
  return {
    clientConnectionId: id,
    clientType: clientType as HubConnection["clientType"],
    tokenPrefix: id.slice(0, 4),
    enabled,
    defaultWorkspaceId: null,
    toolExposureMode: "auto",
  } as HubConnection;
}

const GROUPS = groupConnectionsByType(
  [conn("c1", "claude"), conn("x1", "codex"), conn("x2", "codex"), conn("dead", "cursor", false)],
  [{ id: "cursor", display: "Cursor" }],
);

describe("客户端可见性", () => {
  it("分组只收启用连接，显示名走 clientDisplayName", () => {
    expect(GROUPS.map((g) => [g.clientType, g.label, g.connections.length])).toEqual([
      ["claude", "Claude Code", 1],
      ["codex", "Codex", 2],
    ]);
  });

  it("同类型多连接：任一拒绝即灰", () => {
    const denied = { c1: [], x1: ["s1"], x2: [] };
    const v = clientVisibilityForServer("s1", GROUPS, denied);
    expect(v.map((x) => [x.clientType, x.visible, x.deniedCount])).toEqual([
      ["claude", true, 0],
      ["codex", false, 1],
    ]);
    // deny-list 还没拉回来（undefined）按未拒绝处理，不闪一下灰
    expect(clientVisibilityForServer("s1", GROUPS, {}).every((x) => x.visible)).toBe(true);
  });

  it("计数条按全量 Server 统计，每个类型独立", () => {
    const denied = { c1: ["s2"], x1: ["s1"], x2: ["s3"] };
    expect(visibilityCounts(["s1", "s2", "s3"], GROUPS, denied)).toEqual([
      { clientType: "claude", label: "Claude Code", visible: 2, total: 3 },
      { clientType: "codex", label: "Codex", visible: 1, total: 3 },
    ]);
  });
});

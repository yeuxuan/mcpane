import { describe, expect, it } from "vitest";
import { shortConnectionId } from "./utils";

describe("shortConnectionId", () => {
  it("截短 uuid 形态的连接 ID，保留头尾", () => {
    expect(shortConnectionId("conn_6b2d7fddb58d459a8c9f6129127250817")).toBe("conn_6b2d7f…0817");
  });

  it("dev 引导那类可读短 ID 原样返回", () => {
    expect(shortConnectionId("conn_dev_claude")).toBe("conn_dev_claude");
  });
});

import { describe, expect, it } from "vitest";
import { livePoll, QUERY_STALE_MS } from "./liveQuery";

describe("livePoll", () => {
  it("固定间隔带上 staleTime 与 retry: false", () => {
    expect(livePoll(5000)).toEqual({
      staleTime: QUERY_STALE_MS,
      refetchInterval: 5000,
      retry: false,
    });
  });

  it("staleTime 为 2 秒，给同屏多订阅去重窗口", () => {
    expect(QUERY_STALE_MS).toBe(2_000);
  });
});

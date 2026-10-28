import { describe, expect, it } from "vitest";
import type { ServerDef } from "../components/ServerSetupModal";
import { cardStatus, type Downstreams, diagnosticsView } from "./Servers";

/** 只有 cardStatus 用到的字段有意义，其余取合法默认值。 */
function server(overrides: Partial<ServerDef> = {}): ServerDef {
  return {
    id: "srv_1",
    slug: "demo",
    name: "Demo",
    transport: "stdio",
    command: "npx",
    args: [],
    env: {},
    cwd: null,
    url: null,
    bearer: null,
    bearerEnv: null,
    headers: {},
    runtimePolicy: "shared",
    enabled: true,
    ...overrides,
  };
}

/** cardStatus 收 t 而非返回 key，测试里把 key 原样回显即可断言。 */
const t = (key: string, options?: Record<string, unknown>) =>
  options?.status ? `${key}:${options.status}` : key;

/**
 * 这套状态被服务器页、项目页、总览页三处共用（cardStatus 是共享的纯函数），
 * 任何一条改错都会同时在三个页面上说谎。
 */
describe("cardStatus", () => {
  it("停用优先于一切：即便 Hub 里还挂着实例也报停用", () => {
    const downstreams: Downstreams = { srv_1: { status: "ready" } };
    expect(cardStatus(server({ enabled: false }), downstreams, true, t)).toEqual({
      tone: "stop",
      label: "server.status.disabled",
    });
  });

  it("诊断里的已知状态原样映射", () => {
    const downstreams: Downstreams = { srv_1: { status: "ready" } };
    expect(cardStatus(server(), downstreams, true, t)).toEqual({
      tone: "run",
      label: "server.status.ready",
    });
    expect(cardStatus(server(), { srv_1: { status: "failed" } }, true, t)).toEqual({
      tone: "danger",
      label: "server.status.failed",
    });
  });

  /** 需要 OAuth 登录不是故障：用警示色而不是报错色，文案也要说明白。 */
  it("needs-auth 映射为「需要登录」，不是失败", () => {
    expect(
      cardStatus(server({ transport: "http" }), { srv_1: { status: "needs-auth" } }, true, t),
    ).toEqual({
      tone: "warn",
      label: "server.status.needsAuth",
    });
  });

  /** 失败说具体原因（uvx 没装 / 进程退出 / 连接被拒 / HTTP 状态），认不出才回笼统的「连接失败」。 */
  it("失败时优先显示具体原因", () => {
    const failed = (lastErrorKind: string | null) =>
      cardStatus(server(), { srv_1: { status: "failed", lastErrorKind } }, true, t);
    expect(failed("command_not_found")).toEqual({
      tone: "danger",
      label: "server.reason.commandNotFound",
    });
    expect(failed("exited").label).toBe("server.reason.exited");
    expect(failed("no_response").label).toBe("server.reason.noResponse");
    expect(failed("connection_refused").label).toBe("server.reason.connectionRefused");
    // 测试里的 t 会把 status 插值回显在 key 后面，借此确认状态码被带进了文案。
    expect(failed("http_404").label).toBe("server.reason.http:404");
    // 认不出 / 没有代号：沿用笼统说法，原文仍在 tooltip。
    expect(failed("other").label).toBe("server.status.failed");
    expect(failed(null).label).toBe("server.status.failed");
  });

  it("降级态同样说具体原因，但保持警示色", () => {
    expect(
      cardStatus(
        server(),
        { srv_1: { status: "degraded", lastErrorKind: "no_response" } },
        true,
        t,
      ),
    ).toEqual({ tone: "warn", label: "server.reason.noResponse" });
  });

  it("运行中的服务器不受失败代号残留影响", () => {
    expect(
      cardStatus(server(), { srv_1: { status: "ready", lastErrorKind: "exited" } }, true, t).label,
    ).toBe("server.status.ready");
  });

  it("未知状态串透传给用户，不吞成「正常」", () => {
    expect(cardStatus(server(), { srv_1: { status: "wat" } }, true, t)).toEqual({
      tone: "warn",
      label: "server.status.unknown:wat",
    });
  });

  it("Hub 在跑但诊断里还没有该下游 = 正在连接", () => {
    expect(cardStatus(server(), {}, true, t)).toEqual({
      tone: "warn",
      label: "server.status.starting",
    });
  });

  /**
   * 回归：Hub 不可达时不得报绿。此时一个工具都调不动，若整页卡片反而全绿，
   * 用户会以为问题出在别处。编辑弹窗对同一状态早就是灰色「未连接」，两边必须同口径。
   */
  it("Hub 不可达 → 灰色「未连接」，绝不是绿色「已启用」", () => {
    const status = cardStatus(server(), undefined, false, t);
    expect(status).toEqual({ tone: "stop", label: "server.status.disconnected" });
    expect(status.tone).not.toBe("run");
  });
});

describe("diagnosticsView", () => {
  const staleData = { downstreams: { srv_1: { status: "ready" } } };

  /**
   * 回归：Hub 挂掉后诊断查询进入 error 态，但 react-query 的 `data` 仍是
   * 最后一次成功的缓存。此前直接透传缓存，卡片拿着陈旧 status 永远报绿——
   * error 时必须把陈旧诊断整个丢掉，让 cardStatus 落进「未连接」分支。
   */
  it("查询 error 时丢弃陈旧诊断数据，判 Hub 不可达", () => {
    const view = diagnosticsView({ isError: true, data: staleData });
    expect(view.downstreams).toBeUndefined();
    expect(view.hubReachable).toBe(false);
    expect(cardStatus(server(), view.downstreams, view.hubReachable, t)).toEqual({
      tone: "stop",
      label: "server.status.disconnected",
    });
  });

  it("初次加载（无数据也无错误）不算不可达：按「启动中」而非「未连接」", () => {
    const view = diagnosticsView({ isError: false, data: undefined });
    expect(view.hubReachable).toBe(true);
    expect(cardStatus(server(), view.downstreams, view.hubReachable, t)).toEqual({
      tone: "warn",
      label: "server.status.starting",
    });
  });

  it("正常态原样透传诊断数据", () => {
    const view = diagnosticsView({ isError: false, data: staleData });
    expect(view.downstreams).toEqual(staleData.downstreams);
    expect(view.hubReachable).toBe(true);
  });
});

import { beforeEach, describe, expect, it } from "vitest";
import {
  finishTour,
  finishWizard,
  isTourDone,
  isTourSeen,
  readOnboarding,
  resetAll,
  saveTourStep,
  TOUR_DONE,
  TOUR_IDS,
} from "./onboarding";

const KEY = "mcpane:tours-v1";

describe("导览进度", () => {
  beforeEach(() => localStorage.clear());

  it("全新用户：九套都没看过，自动触发开着", () => {
    const state = readOnboarding();
    expect(state.autoStart).toBe(true);
    for (const id of TOUR_IDS) expect(isTourSeen(state.progress, id)).toBe(false);
  });

  it("看过 = 有记录，哪怕只停在第一站", () => {
    const progress = saveTourStep("servers", "add");
    expect(isTourSeen(progress, "servers")).toBe(true);
    // 但没走完——两者是不同的问题：调度看前者，"总览讲完没"看后者。
    expect(isTourDone(progress, "servers")).toBe(false);
    expect(isTourSeen(progress, "clients")).toBe(false);
  });

  it("中断可续：停在哪一站就记哪一站", () => {
    saveTourStep("providers", "unified");
    expect(readOnboarding().progress.providers).toBe("unified");
  });

  it("走完标记成哨兵值，与任何站点 id 都不会撞", () => {
    const progress = finishTour("logs");
    expect(progress.logs).toBe(TOUR_DONE);
    expect(isTourDone(progress, "logs")).toBe(true);
  });

  // 没有任何"重看"入口了，看过就是永久的——这条守的正是它：一套的记录不会被另一套覆盖掉。
  it("各套进度互不干扰", () => {
    finishTour("usage");
    saveTourStep("projects", "card");
    const progress = finishTour("projects");
    expect(isTourDone(progress, "usage")).toBe(true);
    expect(isTourDone(progress, "projects")).toBe(true);
  });

  // 关过自动触发的老用户不该因为升级又被弹一遍——写入口已经没了，读侧的兼容还得在。
  it("旧存档里关掉的自动触发依然生效", () => {
    localStorage.setItem(KEY, JSON.stringify({ v: 1, tours: {}, autoStart: false }));
    expect(readOnboarding().autoStart).toBe(false);
  });

  it("存档损坏当全新处理，不抛异常", () => {
    localStorage.setItem(KEY, "{ 不是 JSON");
    expect(() => readOnboarding()).not.toThrow();
    expect(readOnboarding().progress).toEqual({});
  });

  it("版本号对不上就丢弃整份存档——形状变了，逐字段猜不如重来", () => {
    localStorage.setItem(KEY, JSON.stringify({ v: 99, tours: { servers: "add" } }));
    expect(readOnboarding().progress).toEqual({});
  });

  it("全新用户要先过首启向导", () => {
    expect(readOnboarding().wizardDone).toBe(false);
    expect(finishWizard().wizardDone).toBe(true);
    expect(readOnboarding().wizardDone).toBe(true);
  });

  // 向导上线之前存下的存档里没有 wizard 字段。已经看过导览的老用户不该被拦一道。
  it("旧存档有导览痕迹就当向导走过了", () => {
    localStorage.setItem(KEY, JSON.stringify({ v: 1, tours: { servers: "*" }, autoStart: true }));
    expect(readOnboarding().wizardDone).toBe(true);
  });

  it("旧存档关过自动触发的，同样不再弹向导", () => {
    localStorage.setItem(KEY, JSON.stringify({ v: 1, tours: {}, autoStart: false }));
    expect(readOnboarding().wizardDone).toBe(true);
  });

  // 真·全新用户（没有任何存档）才该看到向导，上面两条不能把它一起放过。
  it("空存档不算走过向导", () => {
    localStorage.setItem(KEY, JSON.stringify({ v: 1, tours: {}, autoStart: true }));
    expect(readOnboarding().wizardDone).toBe(false);
  });

  it("重跑清空全部进度，并把自动触发打开", () => {
    finishTour("usage");
    finishWizard();
    localStorage.setItem(
      KEY,
      JSON.stringify({ v: 1, tours: { usage: "*" }, autoStart: false, wizard: true }),
    );
    const state = resetAll();
    expect(state.progress).toEqual({});
    expect(state.wizardDone).toBe(false);
    expect(state.autoStart).toBe(true);
    expect(readOnboarding().wizardDone).toBe(false);
  });

  it("不认得的 tourId 与非字符串的值都被过滤掉", () => {
    localStorage.setItem(
      KEY,
      JSON.stringify({ v: 1, tours: { servers: "add", 不存在的一套: "x", clients: 42 } }),
    );
    expect(readOnboarding().progress).toEqual({ servers: "add" });
  });
});

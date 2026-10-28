/**
 * 导览的触发与调度。谁在跑、什么时候自动开始、进度往哪写，都收在这里。
 *
 * 放 context 而不是让各页面自己触发，有两个硬原因：外壳用 `key={page}` 渲染内容区，
 * 切页会把页面子树整棵重挂，状态挂在页面里活不过一次导航；而覆盖层必须渲染在外壳
 * 之外，页面也够不着它。放外壳里同样不行——外壳随 hub 状态五秒一轮重渲染，不该再背
 * 一个状态中枢。
 */
import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";
import type { PageKey } from "../components/Shell";
import {
  isTourDone,
  isTourSeen,
  finishTour as persistFinish,
  finishWizard as persistFinishWizard,
  readOnboarding,
  resetAll,
  saveTourStep,
  type TourId,
} from "./onboarding";
import { TOUR_FOR_PAGE, TOURS } from "./tours";

/**
 * 自动开始前的等待。
 *
 * 一箭双雕：用户连点几个侧栏项时，effect 的 cleanup 会把还没触发的定时器清掉，导览
 * 不会一路闪现；同时这段时间也够首屏那次 IPC 返回、列表挂上——锚点在了，导览才不会
 * 一开就降级成居中说明。别为了后者去调大 TARGET_TIMEOUT_MS，那个值管的是"用户对着
 * 空遮罩能忍多久"，是另一回事。
 */
const AUTO_START_DELAY_MS = 500;

/** 每套导览的第一站 id，自动触发落盘时用。 */
const FIRST_STOP = Object.fromEntries(
  Object.entries(TOURS).map(([id, stops]) => [id, stops[0]?.id ?? ""]),
) as Record<TourId, string>;

/**
 * 页面导览不做"中断续读"，每次都从第一站开始。
 *
 * 2-4 站的东西，看到第二站就走人的用户，下次更想要的是"别再弹了"，而不是"从第三站
 * 接着讲"。所以自动触发只看**有没有触发过**，进度里那个 stopId 只是"来过"的痕迹，
 * 不参与调度。
 */
interface ActiveTour {
  id: TourId;
}

interface TourContextValue {
  active: ActiveTour | null;
  /** 记住停在哪一站。 */
  step: (stopId: string) => void;
  /** 收工：走完、跳过、按 ESC 都走这里。 */
  close: () => void;
  /** 首启向导是否正在显示。它开着的时候不自动弹任何导览。 */
  wizardOpen: boolean;
  /**
   * 向导收工。`markOverviewSeen` 为真表示用户选了直奔某项任务——总览导览
   * 随即标记为看过，免得刚关掉向导又被六站的总览接上；选「先看一遍界面」
   * 则留着不标，交给自动触发照常开讲。
   */
  finishWizard: (markOverviewSeen: boolean) => void;
  /** 设置页的「重新运行引导」：清空进度，从向导重来。 */
  restart: () => void;
}

const TourContext = createContext<TourContextValue | null>(null);

export function TourProvider({ page, children }: { page: PageKey; children: ReactNode }) {
  const [state, setState] = useState(() => readOnboarding());
  const [active, setActive] = useState<ActiveTour | null>(null);
  /**
   * 本次挂载内已经自动开过的几套。overview 走完后用户多半立刻点侧栏，若不记着，
   * 页面导览会紧跟着弹出来，连着两个模态。手动重放不看这个集合。
   */
  const [autoOpened] = useState(() => new Set<TourId>());
  const [wizardOpen, setWizardOpen] = useState(() => !readOnboarding().wizardDone);
  const { autoStart, progress } = state;

  const step = useCallback(
    (stopId: string) => {
      if (!active) return;
      setState((prev) => ({ ...prev, progress: saveTourStep(active.id, stopId) }));
    },
    [active],
  );

  const close = useCallback(() => {
    if (!active) return;
    setState((prev) => ({ ...prev, progress: persistFinish(active.id) }));
    setActive(null);
  }, [active]);

  const finishWizard = useCallback((markOverviewSeen: boolean) => {
    const next = persistFinishWizard();
    setState(markOverviewSeen ? { ...next, progress: persistFinish("overview") } : next);
    setWizardOpen(false);
  }, []);

  const restart = useCallback(() => {
    // 本次挂载"已自动开过"的记录也要清掉，否则重跑之后页面导览一套都不会再出来。
    autoOpened.clear();
    setState(resetAll());
    setActive(null);
    setWizardOpen(true);
  }, [autoOpened]);

  // 自动触发。依赖里带上 active 是为了"跑着的时候不抢"：总览导览会切页，切页又会
  // 命中这里，两个模态就叠上了。向导开着时同理——它才是首启该看到的东西。
  useEffect(() => {
    if (active || wizardOpen || !autoStart) return;
    const id = TOUR_FOR_PAGE[page];
    if (!id || isTourSeen(progress, id) || autoOpened.has(id)) return;
    // 总览没走完之前不插播页面导览：先把整体讲清楚，再讲某一块。
    if (id !== "overview" && !isTourDone(progress, "overview")) return;

    const timer = setTimeout(() => {
      autoOpened.add(id);
      // 自动开的这一刻就落盘。不等用户点「下一步」——他要是直接关掉窗口，下次进来
      // 还会弹，那就成了骚扰。
      setState((prev) => ({ ...prev, progress: saveTourStep(id, FIRST_STOP[id]) }));
      setActive({ id });
    }, AUTO_START_DELAY_MS);
    return () => clearTimeout(timer);
  }, [page, active, wizardOpen, autoStart, progress, autoOpened]);

  const value = useMemo<TourContextValue>(
    () => ({ active, step, close, wizardOpen, finishWizard, restart }),
    [active, step, close, wizardOpen, finishWizard, restart],
  );

  return <TourContext.Provider value={value}>{children}</TourContext.Provider>;
}

export function useTour() {
  const value = useContext(TourContext);
  if (!value) throw new Error("useTour 必须在 TourProvider 内使用");
  return value;
}

/**
 * 导览进度：哪几套看过、停在哪一站。
 *
 * 分成九套而不是一条长线：首启只讲总览，进入某个模块时才讲那个模块。一条 27 站的
 * 直线没人看得完，拆开之后每次只有 2-6 站，总覆盖面反而更大（见 lib/tours.ts）。
 *
 * 三态用一个字符串表达——键缺席 = 没看过，值是 stopId = 停在那一站，值是 "*" = 走完
 * 或跳过。不用 `{done, step}` 那种两字段结构：done 与 step 可以互相矛盾，而这里
 * 表达不出非法状态。存 stopId 而非数字下标同理，站点表增删后旧下标会静默错位。
 *
 * 设备级偏好，与主题/语言同惯例走 localStorage（后端 settings 只管数据目录/端口等
 * 真实配置）。
 */

/** 九套导览，顺序与侧栏一致。 */
export const TOUR_IDS = [
  "overview",
  "servers",
  "providers",
  "clients",
  "projects",
  "workspaces",
  "usage",
  "logs",
  "settings",
] as const;
export type TourId = (typeof TOUR_IDS)[number];

/** 走完（或跳过）的哨兵值，与任何 stopId 都不会撞。 */
export const TOUR_DONE = "*";

/** tourId → 停在哪一站；缺席表示没看过。 */
export type TourProgress = Partial<Record<TourId, string>>;

export interface OnboardingState {
  progress: TourProgress;
  /**
   * 首启向导是否走完（跳过也算）。它比导览先跑：导览讲界面，向导负责把人配起来
   * （迁移 cc-switch、选一条上手路径），没走完之前不自动弹任何导览。
   */
  wizardDone: boolean;
  /**
   * 关掉后不再自动弹任何导览，但页头按钮仍能手动重放。
   *
   * 已经没有写入口——设置页那块导览管理拿掉了。留着读侧有两个真实作用：旧版本里
   * 关过的用户不该因为升级又被弹一遍；localStorage 不可用时兜底也走这条路。
   */
  autoStart: boolean;
}

const STORAGE_KEY = "mcpane:tours-v1";
/** 存档结构版本。将来改形状时用它判断，而不是猜字段。 */
const SCHEMA = 1;

const FRESH: OnboardingState = { progress: {}, autoStart: true, wizardDone: false };
/** localStorage 不可用（隐私模式/存档损坏）时的兜底：不打扰，当全部看过。 */
const SKIP: OnboardingState = { progress: {}, autoStart: false, wizardDone: true };

const isTourId = (value: unknown): value is TourId => TOUR_IDS.includes(value as TourId);

/** 读进度。无记录 = 全新用户，九套都没看过。 */
export function readOnboarding(): OnboardingState {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { ...FRESH, progress: {} };
    const parsed = JSON.parse(raw) as {
      v?: number;
      tours?: Record<string, unknown>;
      autoStart?: unknown;
      wizard?: unknown;
    };
    if (parsed.v !== SCHEMA) return { ...FRESH, progress: {} };
    const progress: TourProgress = {};
    // 只收认得的 tourId，值必须是字符串。不认得的整条丢掉——它要么是拼错的，
    // 要么来自更新的版本，两种情况留着都只会让后面的代码猜。
    for (const [key, value] of Object.entries(parsed.tours ?? {})) {
      if (isTourId(key) && typeof value === "string") progress[key] = value;
    }
    // 没有 wizard 字段 = 向导上线之前就存下的存档。已经看过导览的老用户不该因为
    // 升级被拦一道首启向导，所以有任何导览痕迹就当它走过了。
    const wizardDone =
      typeof parsed.wizard === "boolean"
        ? parsed.wizard
        : Object.keys(progress).length > 0 || parsed.autoStart === false;
    return { progress, autoStart: parsed.autoStart !== false, wizardDone };
  } catch {
    return { ...SKIP, progress: {} };
  }
}

function write(state: OnboardingState) {
  try {
    localStorage.setItem(
      STORAGE_KEY,
      JSON.stringify({
        v: SCHEMA,
        tours: state.progress,
        autoStart: state.autoStart,
        wizard: state.wizardDone,
      }),
    );
  } catch {
    // 忽略：本次会话内 React 状态仍生效，好过让导览直接起不来。
  }
}

/** 看过 = 有记录，哪怕只停在第一站。自动触发的那一刻就落盘，否则用户关掉窗口下次还弹。 */
export const isTourSeen = (progress: TourProgress, id: TourId) => progress[id] !== undefined;

export const isTourDone = (progress: TourProgress, id: TourId) => progress[id] === TOUR_DONE;

/** 记住停在哪一站（每次翻页调用，用于中断后续读）。 */
export function saveTourStep(id: TourId, stopId: string): TourProgress {
  const state = readOnboarding();
  const progress = { ...state.progress, [id]: stopId };
  write({ ...state, progress });
  return progress;
}

/** 标记这一套结束（走完或跳过都算）。 */
export function finishTour(id: TourId): TourProgress {
  return saveTourStep(id, TOUR_DONE);
}

/** 向导收工（走完或跳过都算）。 */
export function finishWizard(): OnboardingState {
  const next = { ...readOnboarding(), wizardDone: true };
  write(next);
  return next;
}

/**
 * 全部重来：清进度、重开向导。设置页那个「重新运行引导」是唯一入口。
 *
 * 顺带把 autoStart 打开——它没有 UI 开关（见上），但显式点重跑的人要的就是
 * "从头再来一遍"，此时还压着自动触发只会让页面导览再也不出现。
 */
export function resetAll(): OnboardingState {
  const next: OnboardingState = { progress: {}, autoStart: true, wizardDone: false };
  write(next);
  return next;
}

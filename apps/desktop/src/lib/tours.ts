/**
 * 九套导览的站点表。纯数据，渲染机制在 components/OnboardingTour.tsx。
 *
 * 拆成两层：`overview` 首启自动开始，只讲"这是什么、界面分几块"；其余八套在**首次
 * 进入对应页面时**才开始，只讲那一页。一条三十站的直线没人看得完，拆开之后每次
 * 2-6 站，总覆盖面反而从 8 站扩到 30 站。
 *
 * 锚点选择的两条规矩，都是被真实情况逼出来的：
 *  1. **能选恒存在的元素就不选列表项**。页头按钮、页签、分区选择器在任何数据状态下
 *     都在 DOM 里；列表项对新用户往往是空的，那一站就只能降级成居中说明。
 *  2. 确实要指列表时，指**首项**而非容器。空容器宽度满格、高度为零，画出来是一条
 *     缝；首项不存在则干脆降级，这是明确的行为。
 *
 * 因此凡是指列表项的站，文案必须在"没有那张卡片"时也读得通——写"每个服务一张卡片，
 * 开关控制启停"，而不是"点这张卡上的开关"。
 */
import type { PageKey } from "../components/Shell";
import type { TourId } from "./onboarding";

export type Placement = "right" | "bottom" | "center";

interface BaseStop {
  /** 在本套导览内唯一即可；同时是 i18n key 的一段：`tour.<tourId>.<id>.title`。 */
  id: string;
  /** 目标的 data-tour 值；不给（或找不到）就退化成居中说明。 */
  target?: string;
  placement: Placement;
  /** 补一行"做完会怎样"。只给动作站：细节挪到这行，正文才短得下来。 */
  outcome?: boolean;
}

/**
 * 页面导览的站点：**不带 navigateTo**。
 *
 * 这不是疏忽而是禁止——Shell 用 `key={page}` 渲染内容区，切页会让整棵子树重挂，
 * 锚点瞬间消失重建，而 useTargetBox 只在 target 变化时重启轮询，洞会停在旧坐标。
 * 页面导览本来就只讲当前这一页，没有切页的理由。
 */
export type PageStop = BaseStop;

/** 总览导览的站点：允许切页，也允许用 hero 形态承载叙述。 */
export interface OverviewStop extends BaseStop {
  navigateTo?: PageKey;
  /** 开场/收尾用宽卡，各带一个视觉主体：开场画产品本身，收尾给能接着做的三步。 */
  hero?: "diagram" | "steps";
}

export type TourStop = OverviewStop;

/**
 * 首启总览。全程留在概览页，只有开场那一站把人带回概览——用户可能从别的页面重放它。
 * 讲的是"界面分几块"，具体每块怎么用交给各自的页面导览。
 */
const OVERVIEW: OverviewStop[] = [
  { id: "intro", placement: "center", hero: "diagram", navigateTo: "dashboard" },
  { id: "cards", target: "dashboard-cards", placement: "bottom" },
  { id: "nav", target: "nav-list", placement: "right" },
  { id: "workspace", target: "workspace-switcher", placement: "bottom" },
  { id: "hub", target: "hub-status", placement: "right", outcome: true },
  { id: "outro", placement: "center", hero: "steps" },
];

const SERVERS: PageStop[] = [
  { id: "add", target: "servers-add", placement: "bottom", outcome: true },
  { id: "import", target: "servers-import", placement: "bottom" },
  { id: "card", target: "servers-card", placement: "bottom" },
];

const PROVIDERS: PageStop[] = [
  { id: "tabs", target: "providers-tabs", placement: "bottom" },
  { id: "add", target: "providers-add", placement: "bottom" },
  { id: "card", target: "providers-card", placement: "bottom", outcome: true },
  { id: "unified", target: "providers-unified", placement: "bottom" },
];

const CLIENTS: PageStop[] = [
  { id: "card", target: "clients-card", placement: "right" },
  { id: "connect", target: "clients-connect", placement: "bottom", outcome: true },
  { id: "actions", target: "clients-actions", placement: "bottom" },
];

const PROJECTS: PageStop[] = [
  { id: "card", target: "projects-card", placement: "right" },
  { id: "create", target: "projects-create", placement: "right" },
  { id: "manage", target: "projects-manage", placement: "bottom", outcome: true },
];

/**
 * 工作区设置页不在侧栏，从顶部切换器或工作区总览页的「管理」进来。但它管的是**工具级**的精细
 * 控制——单个工具停用、起别名、要不要向客户端广播工具变更——恰恰是最不直观的一层，
 * 没有导览就只能靠猜。
 */
const WORKSPACES: PageStop[] = [
  { id: "create", target: "workspaces-create", placement: "bottom" },
  { id: "discovery", target: "workspaces-discovery", placement: "bottom", outcome: true },
  { id: "tools", target: "workspaces-tools", placement: "bottom" },
];

const USAGE: PageStop[] = [
  { id: "range", target: "usage-range", placement: "bottom" },
  { id: "stats", target: "usage-stats", placement: "bottom" },
  { id: "breakdown", target: "usage-breakdown", placement: "bottom", outcome: true },
];

const LOGS: PageStop[] = [
  { id: "tabs", target: "logs-tabs", placement: "bottom" },
  { id: "stats", target: "logs-stats", placement: "bottom" },
  { id: "table", target: "logs-table", placement: "bottom" },
];

/**
 * 设置页只讲分区选择器这一站。
 *
 * 不逐个分区切进去讲：四个分区靠 `section` state 切换，非当前分区的卡片根本不渲染，
 * 而这个选择还持久化在 localStorage——用户上次停在"关于"，导览一开就会整套降级。
 * 设置项本身也都自带标题和说明，再讲一遍是重复。
 */
const SETTINGS: PageStop[] = [{ id: "sections", target: "settings-sections", placement: "bottom" }];

export const TOURS: Record<TourId, TourStop[]> = {
  overview: OVERVIEW,
  servers: SERVERS,
  providers: PROVIDERS,
  clients: CLIENTS,
  projects: PROJECTS,
  workspaces: WORKSPACES,
  usage: USAGE,
  logs: LOGS,
  settings: SETTINGS,
};

/**
 * 哪一页对应哪一套导览。
 *
 * `dashboard` 归 overview（首启就在那讲）。`workspaces`（工作区设置页）不占侧栏，从顶部
 * 切换器或工作区总览页的「管理」进入，但它自己有一套——那一页管的是工具级控制，不讲没人找得到。
 * 用 Partial 而非 Record，新增页面时不会被编译器逼着编一套导览出来。
 */
export const TOUR_FOR_PAGE: Partial<Record<PageKey, TourId>> = {
  dashboard: "overview",
  "mcp-servers": "servers",
  providers: "providers",
  clients: "clients",
  projects: "projects",
  workspaces: "workspaces",
  usage: "usage",
  logs: "logs",
  settings: "settings",
};

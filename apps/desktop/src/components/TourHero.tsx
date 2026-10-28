/**
 * 首启总览开场与收尾的视觉主体。只服务 overview 那一套，与导览引擎
 * （components/OnboardingTour.tsx）分开放：那边三百多行是被 WKWebView 逼出来的
 * 测量与插值，混进来会把那些"别改回去"的事故注释淹掉。
 */
import { Globe, type LucideIcon, Terminal } from "lucide-react";
import { useTranslation } from "react-i18next";
import { BrandGlyph, type BrandName } from "./BrandIcon";
import { BrandMark } from "./ui";

/**
 * 收束图的六条连线。左半段一律从 x=28 画向中枢、右半段从中枢画向外——光点的
 * 流向直接由绘制顺序给出（dashoffset 沿 path 正方向走），不必为方向另写动画。
 * 坐标系即那张 SVG 的 viewBox（400×96），与端点的百分比定位一一对应。
 */
const DIAGRAM_PATHS = [
  "M28 13 C 100 13 130 48 181 48",
  "M28 48 H181",
  "M28 83 C 100 83 130 48 181 48",
  "M219 48 C 270 48 300 13 372 13",
  "M219 48 H372",
  "M219 48 C 270 48 300 83 372 83",
];

/**
 * 图上六个端点。左边是 MCP 服务的两种形态（本地命令 / 远程地址，与服务页同一套
 * 图标），右边是客户端的官方 logo。位置按 viewBox 折算成百分比，SVG 随卡片缩放
 * 时端点跟着走：13/96≈13.5%，83/96≈86.5%，两侧 15/400≈3.75% 与 385/400≈96.25%。
 */
const DIAGRAM_ENDS = [
  { key: "svc-1", icon: Terminal, side: "left", top: "13.5%" },
  { key: "svc-2", icon: Globe, side: "left", top: "50%" },
  { key: "svc-3", icon: Terminal, side: "left", top: "86.5%" },
  { key: "claude", brand: "claude", side: "right", top: "13.5%" },
  { key: "codex", brand: "codex", side: "right", top: "50%" },
  { key: "cursor", brand: "cursor", side: "right", top: "86.5%" },
] as const satisfies readonly {
  key: string;
  icon?: LucideIcon;
  brand?: BrandName;
  side: "left" | "right";
  top: string;
}[];

/**
 * 开场屏的主体：散着的 MCP 服务收进中间的 MCPane，再从一个地址分发给所有客户端。
 *
 * 这是图不是插画——产品的一句话解释，画出来比写出来快，也省掉了那段四行的自我
 * 介绍。端点用真实图标而非示意方块：右边就是用户自己装的 Claude Code、Codex、
 * Cursor，一眼认出"说的是我"，这是抽象图形换不来的。
 *
 * 线画在 SVG 里、端点是 HTML（图标组件进不了 viewBox），两者靠同一套坐标对齐：
 * path 端点坐标除以 viewBox 尺寸就是 HTML 端点的百分比。改一处必须改另一处。
 */
export function ConvergenceDiagram() {
  const { t } = useTranslation();
  return (
    <div className="tour-diagram">
      {/* 端点的百分比必须相对这张 SVG，所以定位上下文只包 SVG——把标签行也圈进来
          的话，容器变高，最下面一排端点会顺着百分比压到标签上。 */}
      <div className="relative">
        <svg
          viewBox="0 0 400 96"
          aria-hidden="true"
          role="presentation"
          className="w-full"
          fill="none"
        >
          <g className="tour-dg-line">
            {DIAGRAM_PATHS.map((d) => (
              <path key={d} d={d} />
            ))}
          </g>
          {/* 沿线跑的光点。错开出发时间，六条线才像各自在流，而不是一起闪。 */}
          <g className="tour-dg-flow">
            {DIAGRAM_PATHS.map((d, i) => (
              <path key={d} d={d} style={{ animationDelay: `${i * 0.42}s` }} />
            ))}
          </g>
        </svg>

        {DIAGRAM_ENDS.map((end) => (
          <span
            key={end.key}
            aria-hidden
            className="tour-dg-chip"
            style={{ top: end.top, left: end.side === "left" ? "3.75%" : "96.25%" }}
          >
            {/* logo 走墨色而非各自的品牌色：三个彩色端点会跟中枢抢眼，而这张图
                要说的恰恰是"都汇到中间那个"。形状本身已经够认了。 */}
            {"brand" in end ? (
              <span className="flex text-ink">
                <BrandGlyph name={end.brand} size={15} />
              </span>
            ) : (
              <end.icon size={14} strokeWidth={2} className="text-faint" />
            )}
          </span>
        ))}
        {/* 中枢用 MCPane 自己的标记，实心底：两侧是"别人家的"，中间这个是本产品。 */}
        <span aria-hidden className="tour-dg-chip tour-dg-hub" style={{ top: "50%", left: "50%" }}>
          <BrandMark size={21} />
        </span>
      </div>

      {/* 三列对齐图上的三处：左端点、中枢、右端点。 */}
      <div className="mt-2 grid grid-cols-3 text-[11px] text-faint">
        <span>{t("tour.diagram.servers")}</span>
        <span className="text-center font-medium text-accent">{t("tour.diagram.hub")}</span>
        <span className="text-right">{t("tour.diagram.clients")}</span>
      </div>
    </div>
  );
}

/**
 * 收尾屏的主体：接下来该做的三步。
 *
 * 编号是真的顺序——没装服务就没有工具可接，没接客户端就用不上——所以序号带信息，
 * 不是装饰性的 01/02/03。
 */
export function NextSteps() {
  const { t } = useTranslation();
  return (
    <ol className="mt-3.5 space-y-2">
      {[1, 2, 3].map((n) => (
        <li key={n} className="flex items-start gap-2.5">
          <span className="tour-step-num">{n}</span>
          <span className="text-[13px] leading-6 text-ink">{t(`tour.steps.${n}`)}</span>
        </li>
      ))}
    </ol>
  );
}

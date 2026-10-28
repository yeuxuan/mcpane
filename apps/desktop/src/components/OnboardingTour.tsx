/**
 * 导览引擎：遮罩挖洞的聚光灯。站点表是纯数据，在 lib/tours.ts。
 *
 * 不用居中弹窗讲解——那只是把说明书搬进应用里，看完仍然不知道点哪。这里让引导直接
 * 落在真实界面上：整屏压暗，只把当下要认识的那个控件留在光里，气泡从旁边指过去。
 * 聚光灯在站点之间平滑滑动，用户的视线被牵着走一遍，走完就记住了每样东西在哪。
 *
 * 气泡分两种形态，别再统一回一种：开场/收尾是宽卡，各自有视觉主体（见 TourHero），
 * 承载的是叙述；其余站是窄卡，只说一句指认。全都长一个样时，"整个产品是什么"和
 * "这个按钮叫什么"用同样的视觉重量，读起来就没有节奏。
 *
 * 三处实现是被 WKWebView（Tauri 在 macOS 上的引擎）逼出来的，别改回"更简洁"的写法：
 *  1. 挖洞用 SVG mask，不用铺满视口的超大 box-shadow。后者 computed style 完全正确，
 *     WebKit 却根本不绘制——Chromium 下看着好好的，装出来一片遮罩都没有。
 *  2. 滑动用 rAF 手动插值，不用 CSS transition。洞在 SVG 里、描边在 div 上，两者靠
 *     共用同一组插值坐标才严丝合缝；SVG 几何属性的过渡支持又各引擎不一。
 *  3. 气泡高度必须实测再拿去钳位置与箭头，见 bubbleH。
 */
import { useEffect, useId, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { TourId } from "../lib/onboarding";
import { type Placement, TOURS, type TourStop } from "../lib/tours";
import { cn } from "../lib/utils";
import type { PageKey } from "./Shell";
import { ConvergenceDiagram, NextSteps } from "./TourHero";
import { Button } from "./ui";

/** 高亮框相对目标外扩，留一点呼吸；也让圆角看起来包住控件而不是贴着它。 */
const HALO = 8;
/** 气泡与高亮框的间距。 */
const GAP = 14;
/** 气泡贴边时与视口的最小距离。 */
const MARGIN = 16;
const BUBBLE_W = 328;
const HERO_BUBBLE_W = 440;
/**
 * 等目标元素出现的上限。切页后 DOM 挂载只需一两帧，这里给的余量已是数十倍；
 * 等待期间气泡不画（见 useTargetBox），所以上限也是"最坏情况下用户对着空遮罩
 * 的时长"——不能设得慷慨，超时就该退回居中说明把话讲完。等首屏数据的活不归它，
 * 那是触发侧的延迟负责的（见 TourProvider）。
 */
const TARGET_TIMEOUT_MS = 600;
/** 聚光灯滑到下一站的时长。 */
const SLIDE_MS = 380;
/** 洞的圆角，与描边框一致才严丝合缝。 */
const HOLE_RADIUS = 12;
/**
 * 小于这个尺寸就当作"还没就绪"。
 *
 * 不是保守起见——空列表的网格容器宽度满格、高度为零，只判 `width === 0 && height === 0`
 * 会放它过关，于是画出一条 16px 高（HALO 的两倍）的缝，看着像渲染坏了。
 */
const MIN_TARGET_PX = 8;

interface Box {
  top: number;
  left: number;
  width: number;
  height: number;
}

/**
 * `bringIntoView`：目标被视口裁掉时先滚动露出来再测。
 *
 * 导览期间 scrim 挡住了滚轮，用户没法自己滚——长页面靠后的锚点（日志表、用量分组）
 * 在小窗口下会落到视口外，聚光灯就打在看不见的地方。只在 layout effect 里带这个
 * 参数：渲染期那次调用保持纯读。`block: "nearest"` 做最小滚动，比顶靠上边缘的高
 * 元素（如整条侧栏导航）用 center 稳。scrollIntoView 同步更新布局，紧接着的
 * getBoundingClientRect 拿到的就是滚动后的坐标。
 */
function measure(target: string | undefined, bringIntoView = false): Box | null {
  if (!target) return null;
  const el = document.querySelector<HTMLElement>(`[data-tour="${target}"]`);
  if (!el) return null;
  let r = el.getBoundingClientRect();
  if (r.width < MIN_TARGET_PX || r.height < MIN_TARGET_PX) return null;
  // 只在真被裁掉时滚（上缘出界或下缘出界），差一点点不动。
  if (bringIntoView && (r.top < 0 || r.bottom > window.innerHeight)) {
    el.scrollIntoView({ block: "nearest", inline: "nearest" });
    r = el.getBoundingClientRect();
  }
  return { top: r.top, left: r.left, width: r.width, height: r.height };
}

/**
 * 追踪当前站目标的位置。目标可能要等一会儿才挂上，这里用 rAF 轮询到出现为止；
 * 超时则返回 null，由调用方降级成居中说明——宁可少一个高亮，不要一个空框。
 *
 * 等待期间刻意**保留上一站的 box**：聚光灯因此留在原地，等新坐标到达再由 useSlide
 * 滑过去，那段滑行正是导览的视线牵引。气泡则不同——它必须靠 pending 藏起来，否则会
 * 先按旧坐标画一帧、再跳到新位置，读起来像抽搐。
 */
function useTargetBox(target: string | undefined): { box: Box | null; pending: boolean } {
  const snapshot = (next: string | undefined) => {
    const box = measure(next);
    return { target: next, box, pending: Boolean(next) && !box };
  };
  const [snap, setSnap] = useState(() => snapshot(target));

  // 换站时**在渲染阶段**就重测，而不是等 effect。多数目标（侧栏项、页头按钮）一直在
  // DOM 里，等到 effect 再测就会先拿上一站的坐标画一帧，气泡看着跳了一下。
  // 这里读的是上一次已提交的 DOM，与本次渲染无关，故渲染期读取是安全的。
  if (snap.target !== target) setSnap(snapshot(target));

  // snapshot 每次渲染新建，作依赖会让 effect 每帧重挂；它只读 target，无闭包陷阱。
  // biome-ignore lint/correctness/useExhaustiveDependencies: 见上
  useLayoutEffect(() => {
    let raf = 0;
    const deadline = performance.now() + TARGET_TIMEOUT_MS;

    const poll = () => {
      const next = measure(target, true);
      if (next || !target) {
        setSnap({ target, box: next, pending: false });
        return;
      }
      if (performance.now() < deadline) raf = requestAnimationFrame(poll);
      // 等不到就放弃高亮，退回居中说明——绝不指着空处说话。
      else setSnap({ target, box: null, pending: false });
    };
    // 提交后再测一次；渲染期已拿到就此刻确认，没拿到的则由此进入轮询。
    poll();

    // 窗口尺寸变了要重新对准；导览期间页面不滚动，故不监听 scroll。
    const remeasure = () => setSnap(snapshot(target));
    window.addEventListener("resize", remeasure);
    return () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("resize", remeasure);
    };
  }, [target]);

  return { box: snap.box, pending: snap.pending };
}

const prefersReducedMotion = () =>
  typeof window !== "undefined" && window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;

/** easeOutCubic：起步快、收尾稳，聚光灯滑到位时不会"撞停"。 */
const easeOut = (t: number) => 1 - (1 - t) ** 3;

/**
 * 把高亮框滑到下一站，而不是瞬移。
 *
 * 刻意手写插值而非交给 CSS transition：洞画在 SVG 里、描边画在 div 上，两者必须逐帧
 * 对齐，共用一组插值坐标是唯一稳妥的对齐方式（各引擎对 SVG 几何属性的过渡支持还不
 * 一致）。没有起点（首次出现、上一站无高亮）或用户要求减少动效时直接到位。
 */
function useSlide(target: Box | null): Box | null {
  const [box, setBox] = useState<Box | null>(target);
  // 起点取自"当前画到哪了"，走 ref 以免把它列进依赖——那会让动画每帧自我重启。
  const currentRef = useRef<Box | null>(target);
  currentRef.current = box;
  const rafRef = useRef(0);

  useEffect(() => {
    cancelAnimationFrame(rafRef.current);
    const from = currentRef.current;
    if (!target || !from || prefersReducedMotion()) {
      setBox(target);
      return;
    }
    const startedAt = performance.now();
    const tick = () => {
      const t = Math.min(1, (performance.now() - startedAt) / SLIDE_MS);
      const k = easeOut(t);
      setBox({
        top: from.top + (target.top - from.top) * k,
        left: from.left + (target.left - from.left) * k,
        width: from.width + (target.width - from.width) * k,
        height: from.height + (target.height - from.height) * k,
      });
      if (t < 1) rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafRef.current);
  }, [target]);

  // 不要动效时直接给目标坐标，不等上面那轮 effect。effect 跑在 paint 之后，
  // 等它意味着先按旧坐标画一帧——有滑行动画时那是起点，没有动画时就是一次闪跳。
  return prefersReducedMotion() ? target : box;
}

const clamp = (value: number, min: number, max: number) =>
  Math.min(Math.max(value, min), Math.max(min, max));

/**
 * 首帧的气泡高度回退值。真实高度由 useLayoutEffect 测出（见组件内 bubbleH），
 * 这里只是"还没量到"时的估计：窄卡长满的样子。低估会让气泡溢出视口，宁可留富余。
 */
const BUBBLE_MAX_H = 268;

type ArrowSide = "left" | "right" | "top" | "bottom";

/**
 * 气泡位置：贴着高亮框放，越界则钳回视口内。
 *
 * 钳过之后气泡不再对齐目标中心（贴视口下缘的目标尤其明显），所以箭头位置单独算：
 * 它始终指回目标中心，让"这句话在说哪个控件"不因排版让步而含糊。
 *
 * height 必须是气泡的**实测**高度。用固定上界估的时候，比估值高的那几站箭头会被钳在
 * 估值处，指到目标上方几十像素的空处去。
 */
function bubbleLayout(box: Box | null, placement: Placement, width: number, height: number) {
  if (!box || placement === "center") {
    return { style: { left: "50%", top: "50%", transform: "translate(-50%, -50%)" }, arrow: null };
  }
  if (placement === "right") {
    const centerY = box.top + box.height / 2;
    // 顶部对齐目标顶部而非中心对中心：贴着视口下缘的目标若居中摆放，气泡有一半会
    // 掉到窗口外面。
    const top = clamp(box.top - HALO, MARGIN, window.innerHeight - height - MARGIN);
    // 右边放不下就翻到左边。分组卡这类目标占了半个主区，右侧余量不够 328px，
    // 不翻的话气泡整个落在视口外——点不到，也看不见。
    const rightEdge = box.left + box.width + HALO + GAP;
    const flipped = rightEdge + width + MARGIN > window.innerWidth;
    const left = flipped ? box.left - HALO - GAP - width : rightEdge;
    return {
      style: { left: clamp(left, MARGIN, window.innerWidth - width - MARGIN), top },
      arrow: {
        side: (flipped ? "right" : "left") as ArrowSide,
        offset: clamp(centerY - top, 20, height - 20),
      },
    };
  }

  const centerX = box.left + box.width / 2;
  const left = clamp(centerX - width / 2, MARGIN, window.innerWidth - width - MARGIN);
  // bottom 放不下就翻到目标上方。统计块、日志表格这类目标很高，硬放在下面气泡会被
  // 钳到贴着视口底，箭头也就指不回目标了。
  const below = box.top + box.height + HALO + GAP;
  const flipped = below + height + MARGIN > window.innerHeight;
  const top = flipped ? box.top - HALO - GAP - height : below;
  return {
    style: { left, top: clamp(top, MARGIN, window.innerHeight - height - MARGIN) },
    arrow: {
      side: (flipped ? "bottom" : "top") as ArrowSide,
      offset: clamp(centerX - left, 20, width - 20),
    },
  };
}

/** 气泡尖角：从气泡边缘顶出去指向目标，靠旋转方块 + 只留两条外侧边实现。 */
function BubbleArrow({ side, offset }: { side: ArrowSide; offset: number }) {
  const edge =
    side === "left"
      ? "-left-[6px] border-b border-l"
      : side === "right"
        ? "-right-[6px] border-t border-r"
        : side === "top"
          ? "-top-[6px] border-l border-t"
          : "-bottom-[6px] border-r border-b";
  const vertical = side === "left" || side === "right";
  return (
    <span
      aria-hidden
      className={cn("absolute h-2.5 w-2.5 rotate-45 border-line bg-card", edge)}
      // offset 是箭头中心相对气泡该边起点的距离，减半宽换成左上角坐标。
      style={vertical ? { top: offset - 5 } : { left: offset - 5 }}
    />
  );
}

/** 进度点：走过的填实，当前的拉长，没到的留空——一眼看得出还剩几站。 */
function StopDots({ stops, index, label }: { stops: TourStop[]; index: number; label: string }) {
  return (
    <div className="flex items-center gap-1.5" role="status" aria-label={label}>
      {stops.map((stop, i) => (
        <span
          key={stop.id}
          aria-hidden
          className={cn(
            "h-1.5 rounded-full transition-all duration-300",
            i === index
              ? "w-4 bg-accent"
              : i < index
                ? "w-1.5 bg-accent/45"
                : "w-1.5 bg-line-strong",
          )}
        />
      ))}
    </div>
  );
}

export function OnboardingTour({
  tourId,
  onNavigate,
  onStep,
  onClose,
}: {
  tourId: TourId;
  /** 只有总览导览会切页，页面导览的站点类型里禁掉了 navigateTo。 */
  onNavigate: (page: PageKey) => void;
  /** 每翻一页回报当前站，由外层落盘——组件自己不碰 storage。 */
  onStep: (stopId: string) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const stops = TOURS[tourId];
  const [index, setIndex] = useState(0);
  const stop = stops[index] ?? stops[0];

  // 切页要在测量之前发生：布局副作用里做，才能赶在同一帧把目标挂上。
  const navigateRef = useRef(onNavigate);
  navigateRef.current = onNavigate;
  useLayoutEffect(() => {
    if (stop?.navigateTo) navigateRef.current(stop.navigateTo);
  }, [stop?.navigateTo]);

  const { box, pending } = useTargetBox(stop?.target);
  // 洞用插值坐标（滑过去），气泡用目标坐标（直接落位）：滑动中的文字不好读，
  // 而聚光灯滑行本身已经把视线带到了新位置。
  const slid = useSlide(box);
  const hole = slid
    ? {
        top: slid.top - HALO,
        left: slid.left - HALO,
        width: slid.width + HALO * 2,
        height: slid.height + HALO * 2,
      }
    : null;
  const maskId = useId();
  const width = stop?.hero ? HERO_BUBBLE_W : BUBBLE_W;

  // 气泡实测高度。各站高矮不一（结果行、三步清单都会长个几十像素），拿固定估值去钳
  // 位置和箭头就会算偏。在 layout effect 里量并回写，React 会在同一帧内同步重渲染，
  // 浏览器绘不到中间态——所以这多出来的一轮不会看成跳动。
  const bubbleRef = useRef<HTMLDivElement>(null);
  const [bubbleH, setBubbleH] = useState(BUBBLE_MAX_H);
  useLayoutEffect(() => {
    const h = bubbleRef.current?.offsetHeight;
    // 量到相同值时 setState 会被 React 跳过，故无需依赖数组也不会自循环。
    if (h) setBubbleH(h);
  });

  // 目标没等到时退回居中：宁可少一个高亮，也不要指着空处说话。
  const placement = box ? (stop?.placement ?? "center") : "center";
  const { style, arrow } = bubbleLayout(box, placement, width, bubbleH);

  const go = (next: number) => {
    const target = stops[next];
    if (!target) {
      onClose();
      return;
    }
    setIndex(next);
    onStep(target.id);
  };

  // ESC 随时退出：被引导困住是比没有引导更糟的体验。
  const closeRef = useRef(onClose);
  closeRef.current = onClose;
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") closeRef.current();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  // TOURS 是静态数据、每套都非空，正常到不了这里；真遇到（空导览定义）就
  // 什么都不渲染，别把整个应用抛进崩溃边界。ESC 监听已挂好，用户仍可退出。
  if (!stop) return null;

  const isLast = index === stops.length - 1;
  const key = (field: string) => `tour.${tourId}.${stop.id}.${field}`;

  return (
    <div
      className="fixed inset-0 z-50"
      role="dialog"
      aria-modal="true"
      aria-label={t("tour.label")}
    >
      {/* 压暗整屏；有目标就在 mask 上挖一个圆角洞。
          用 SVG 而非超大 box-shadow：后者在 WKWebView 下一片都画不出来（见文件头）。 */}
      {/* 纯视觉层，不进无障碍树：要读的内容全在气泡里。 */}
      <svg
        aria-hidden="true"
        role="presentation"
        className="tour-scrim absolute inset-0 h-full w-full"
      >
        <mask id={maskId}>
          <rect width="100%" height="100%" fill="#fff" />
          {hole ? (
            <rect
              x={hole.left}
              y={hole.top}
              width={hole.width}
              height={hole.height}
              rx={HOLE_RADIUS}
              fill="#000"
            />
          ) : null}
        </mask>
        <rect width="100%" height="100%" className="tour-scrim-fill" mask={`url(#${maskId})`} />
      </svg>

      {/* 洞的描边与光晕。与洞共用同一组插值坐标，故始终严丝合缝。
          key 绑当前站：换站重挂，"打光"那下脉冲才会重新播一次。 */}
      {hole ? (
        <div
          // 必须与气泡的 key 不同名：两者是同层兄弟，同名 key 会让 React 复用错节点，
          // 结果是页面上同时留着两个描边框。
          key={`ring-${stop.id}`}
          aria-hidden
          className="tour-ring pointer-events-none absolute"
          style={{
            top: hole.top,
            left: hole.left,
            width: hole.width,
            height: hole.height,
            borderRadius: HOLE_RADIUS,
          }}
        />
      ) : null}

      {/* 目标还没量到就先不画气泡：拿旧坐标顶上会先闪一帧错位再跳回来。
          等到了才挂载，淡入动画因此落在正确位置上，而不是白跑在隐藏元素上。 */}
      {pending ? null : (
        <div
          key={stop.id}
          ref={bubbleRef}
          className={cn(
            "tour-bubble absolute rounded-xl border border-line bg-card shadow-[var(--shadow-pop)]",
            stop.hero ? "p-5" : "p-4",
          )}
          style={{ ...style, width }}
        >
          {/* 计数与「跳过」同处顶行：一个是你在哪，一个是随时能走，都是元信息。
              底部因此只剩前进/后退两键，主行动不再被第三种按钮样式稀释。 */}
          <div className="tour-line flex items-baseline justify-between gap-3">
            <p className="font-mono text-[11px] tracking-tight text-faint tabular-nums">
              <span className="text-accent">{index + 1}</span>
              <span className="mx-0.5">/</span>
              {stops.length}
            </p>
            {isLast ? null : (
              <button
                type="button"
                onClick={onClose}
                className="-mr-1 rounded px-1 py-0.5 text-[11px] text-faint transition-colors hover:text-ink"
              >
                {t("tour.skip")}
              </button>
            )}
          </div>

          {stop.hero === "diagram" ? (
            <div className="tour-line mt-3">
              <ConvergenceDiagram />
            </div>
          ) : null}

          {/* 区域标签：先告诉你现在看的是界面哪一块，再说它干什么。有 hero 的两站
              讲的是整个产品，没有"哪一块"可指，故不显示。
              这里不用 mono：等宽字体会把中文字距撑开，「服务 · 添加」散成一排。 */}
          {stop.hero ? null : (
            <p className="tour-line mt-3 text-[11px] font-medium text-accent">{t(key("where"))}</p>
          )}

          <h2
            className={cn(
              "tour-line font-semibold text-ink",
              stop.hero ? "mt-4 text-[19px] leading-tight tracking-tight" : "mt-1.5 text-[15px]",
            )}
          >
            {t(key("title"))}
          </h2>
          <p className="tour-line mt-2 text-[13px] leading-relaxed text-muted">{t(key("desc"))}</p>

          {stop.hero === "steps" ? (
            <div className="tour-line">
              <NextSteps />
            </div>
          ) : null}

          {/* 结果行：这一步做完会怎样。压到正文之后、页脚之前，靠一条 accent 竖线
              与正文分层——它是承诺，不是补充说明。 */}
          {stop.outcome ? <p className="tour-line tour-outcome mt-3">{t(key("outcome"))}</p> : null}

          <div className="tour-line mt-4 flex items-center gap-2 border-t border-line pt-3.5">
            <StopDots
              stops={stops}
              index={index}
              label={t("tour.progress", { current: index + 1, total: stops.length })}
            />
            <div className="flex-1" />
            {index > 0 ? (
              <Button size="sm" onClick={() => go(index - 1)}>
                {t("tour.back")}
              </Button>
            ) : null}
            <Button
              variant="primary"
              size="sm"
              onClick={() => (isLast ? onClose() : go(index + 1))}
            >
              {isLast ? t("tour.finish") : t("tour.next")}
            </Button>
          </div>

          {/* 箭头绝对定位，摆在最后只为让上面那串 .tour-line 的 nth-child 序号
              不因它的有无而错位——逐行入场的延迟就是按序号排的。 */}
          {arrow ? <BubbleArrow side={arrow.side} offset={arrow.offset} /> : null}
        </div>
      )}
    </div>
  );
}

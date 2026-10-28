import { cva, type VariantProps } from "class-variance-authority";
import type { LucideIcon } from "lucide-react";
import {
  Check,
  ChevronRight,
  Copy,
  Loader2,
  Search,
  Sparkles,
  TriangleAlert,
  X,
} from "lucide-react";
import type {
  ButtonHTMLAttributes,
  FieldsetHTMLAttributes,
  FocusEventHandler,
  HTMLAttributes,
  InputHTMLAttributes,
  ReactNode,
  TextareaHTMLAttributes,
} from "react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "../lib/utils";
import { Accordion, AccordionContent, AccordionItem, AccordionTrigger } from "./shadcn/accordion";
import { Input } from "./shadcn/input";
import { Label } from "./shadcn/label";
import { Popover, PopoverAnchor, PopoverContent } from "./shadcn/popover";

/* ---------------- Button ---------------- */
const buttonVariants = cva(
  "inline-flex items-center justify-center gap-1.5 whitespace-nowrap rounded-lg text-[13px] font-medium transition-colors disabled:pointer-events-none disabled:opacity-40",
  {
    variants: {
      variant: {
        primary: "bg-accent-solid text-on-accent-solid hover:bg-accent-solid-hover shadow-sm",
        secondary: "border border-line-strong bg-card text-ink hover:bg-hover",
        ghost: "text-muted hover:bg-hover hover:text-ink",
        danger: "border border-line-strong bg-card text-danger hover:bg-danger-soft",
      },
      size: {
        md: "h-9 px-3.5",
        sm: "h-8 px-3 text-xs",
        icon: "h-9 w-9",
        iconSm: "h-8 w-8",
      },
    },
    defaultVariants: { variant: "secondary", size: "md" },
  },
);

export function Button({
  className,
  variant,
  size,
  icon: Icon,
  loading,
  disabled,
  children,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> &
  VariantProps<typeof buttonVariants> & {
    icon?: LucideIcon;
    /** 点击会触发耗时的下游启停（如 ReloadServers 重连）时置位，替换图标为转圈并阻止重复点击。 */
    loading?: boolean;
  }) {
  return (
    <button
      type="button"
      disabled={disabled || loading}
      aria-busy={loading}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    >
      {loading ? (
        <Loader2 size={16} strokeWidth={2} aria-hidden className="animate-spin" />
      ) : Icon ? (
        <Icon size={16} strokeWidth={2} aria-hidden />
      ) : null}
      {children}
    </button>
  );
}

/* ---------------- IconButton（无边框图标动作按钮） ---------------- */
export function IconButton({
  icon: Icon,
  tone = "neutral",
  loading,
  disabled,
  className,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  icon: LucideIcon;
  tone?: "neutral" | "danger";
  /** 点击会触发耗时的下游启停（如 ReloadServers 重连）时置位，图标换转圈并阻止重复点击。 */
  loading?: boolean;
}) {
  return (
    <button
      type="button"
      disabled={disabled || loading}
      aria-busy={loading}
      className={cn(
        "flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-faint transition-colors disabled:pointer-events-none disabled:opacity-40",
        tone === "danger"
          ? "hover:bg-danger-soft hover:text-danger"
          : "hover:bg-hover hover:text-ink",
        className,
      )}
      {...props}
    >
      {loading ? (
        <Loader2 size={15} strokeWidth={2} aria-hidden className="animate-spin" />
      ) : (
        <Icon size={15} strokeWidth={2} aria-hidden />
      )}
    </button>
  );
}

/* ---------------- Switch (iOS) ---------------- */
export function Switch({
  checked,
  onChange,
  label,
  disabled,
  loading,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  disabled?: boolean;
  /** 切换会触发耗时的下游进程启停（如 MCP server 拉起/关闭）时置位，显示旋转指示并阻止重复点击。 */
  loading?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-busy={loading}
      aria-label={label}
      disabled={disabled || loading}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative inline-flex h-[26px] w-[44px] shrink-0 items-center rounded-full transition-colors disabled:opacity-40",
        checked ? "bg-on" : "bg-switch-off",
      )}
    >
      <span
        className={cn(
          "inline-flex h-[22px] w-[22px] items-center justify-center rounded-full bg-white shadow transition-transform",
          checked ? "translate-x-[20px]" : "translate-x-[2px]",
        )}
      >
        {loading ? (
          <Loader2
            size={12}
            strokeWidth={2.5}
            aria-hidden
            className={cn("animate-spin", checked ? "text-on" : "text-faint")}
          />
        ) : null}
      </span>
    </button>
  );
}

/* ---------------- SegmentedControl ---------------- */
/**
 * 分段选择器：一组互斥选项，选中项高亮。设置页的外观 / 语言 / 代理三处共用。
 *
 * **用原生 radio 而不是一排 button**：单选语义、方向键切换、roving tabindex、
 * 「已选中」的播报，原生全部免费且正确。一排 button 对读屏用户只是「一堆按钮」，
 * 听不出这是单选、更听不出哪个选中了——选中态只写在背景色里，那只对视力用户有效。
 *
 * `fieldset` + `aria-label` 而不是 `div` + `role="group"`：这就是「一组表单控件」
 * 的原生表达。不用 `legend` 是因为它在 fieldset 里的渲染有历史包袱（浮在 border 上），
 * sr-only 定位后各引擎表现不一；fieldset 上的 aria-label 则各 AT 支持一致。
 *
 * 选中/悬停由 JS 算类名而非 `peer-checked:`：两者特异性相同、只靠源码顺序分
 * 胜负，hover 压掉 checked 是一次样式重排就会发生的事。peer 只用来把键盘焦点
 * 从透明的 radio 引到可见的 span 上——这条必须留，否则键盘用户看不见焦点。
 *
 * radio 铺满整个选项（`absolute inset-0` + `opacity-0`）而不是 `sr-only`：
 * sr-only 会把它缩成 1px，鼠标与自动化点击都会落到 span 上、被当成点击被遮挡。
 * 透明但铺满则点击命中的就是 radio 本身。opacity-0 不影响可访问性树，AT 照常读到。
 * 相应地，悬停必须走 `group-hover`——鼠标停在铺满的 radio 上时，它的**兄弟** span
 * 并不处于 `:hover`（CSS 只把 :hover 给到鼠标下元素及其祖先），写 `hover:` 会静默失效。
 */
export function SegmentedControl<T extends string>({
  label,
  value,
  options,
  onChange,
  disabled,
  className,
  ...props
}: Omit<FieldsetHTMLAttributes<HTMLFieldSetElement>, "onChange"> & {
  /** 无障碍名称。同页多组分段选择器常有同名项（如外观与代理都有「跟随系统」），靠它区分。 */
  label: string;
  value: T;
  options: readonly { value: T; label: string }[];
  onChange: (next: T) => void;
  disabled?: boolean;
}) {
  // 同页三组 radio 必须各有各的 name，否则它们会被浏览器当成同一组、互相取消选中。
  const name = useId();
  // 开发期护栏：`value` 落在 `options` 之外时，这个控件会**静默地一项都不高亮**，
  // 界面上不留任何信号——用户读不出当前是什么状态，而调用方也收不到任何报错。
  // 云同步的 provider 就是这么栽的（后端全新安装回空串，既不等于 "off" 也不匹配
  // 任何一项）。组件没法凭空发明一个选中项，那就至少在控制台吼一声。
  // matched / known 都取成基本类型再进依赖数组：直接依赖 options 会因每次渲染
  // 都是新数组而反复触发，绕开它也不必再玩「join 一个分隔符再 split 回来」。
  const matched = options.some((option) => option.value === value);
  const known = options.map((option) => option.value).join(" / ");
  useEffect(() => {
    if (!import.meta.env.DEV || matched) return;
    console.warn(
      `[SegmentedControl] value ${JSON.stringify(value)} 不在 options（${known}）中，将没有任何一项高亮。控件：${label}`,
    );
  }, [matched, value, known, label]);
  return (
    // 透传 fieldset 属性：导览要往分段器上挂 data-tour 锚点，不透传就只能在外面白包一层。
    <fieldset
      aria-label={label}
      className={cn("inline-flex rounded-lg border border-line-strong p-0.5", className)}
      {...props}
    >
      {options.map((option) => {
        const selected = option.value === value;
        return (
          <label key={option.value} className="group relative">
            <input
              type="radio"
              name={name}
              value={option.value}
              checked={selected}
              disabled={disabled}
              onChange={() => onChange(option.value)}
              className="peer absolute inset-0 cursor-pointer opacity-0 disabled:cursor-not-allowed"
            />
            <span
              className={cn(
                "block rounded-md px-3 py-1.5 text-xs font-medium transition-colors",
                selected
                  ? "bg-accent-solid text-on-accent-solid"
                  : "text-muted group-hover:text-ink",
                disabled && "opacity-60",
                "peer-focus-visible:ring-2 peer-focus-visible:ring-accent-solid peer-focus-visible:ring-offset-1",
              )}
            >
              {option.label}
            </span>
          </label>
        );
      })}
    </fieldset>
  );
}

/* ---------------- Card ---------------- */
/** 透传 div 属性：导览要往卡片上挂 `data-tour` 锚点，不透传就只能在外面白包一层。 */
export function Card({
  className,
  interactive,
  children,
  ...props
}: HTMLAttributes<HTMLDivElement> & { interactive?: boolean }) {
  return (
    <div
      className={cn(
        "rounded-xl border border-line bg-card shadow-[var(--shadow-card)]",
        interactive && "transition-shadow hover:shadow-[var(--shadow-pop)]",
        className,
      )}
      {...props}
    >
      {children}
    </div>
  );
}

/* ---------------- IconChip（彩色图标胶囊） ---------------- */
/** 品牌 logo 的托底：只衬不抢，50 档足够（色彩由 logo 自己承担）。
 * 暗色不挑更深的色阶，改用半透明彩底 + 提亮字色：
 * 固定色阶在暗底上要么发灰要么发亮，半透明能自然融进任意底色（macOS 同款做法）。 */
const chipTone = {
  blue: "bg-blue-50 text-blue-600 dark:bg-blue-400/15 dark:text-blue-300",
  green: "bg-green-50 text-green-600 dark:bg-green-400/15 dark:text-green-300",
  // 暗色字取 orange-300：amber-300 提亮后发黄，Claude 这类暖橙品牌会被染成柠檬色。
  amber: "bg-amber-50 text-amber-600 dark:bg-amber-400/15 dark:text-orange-300",
  rose: "bg-rose-50 text-rose-600 dark:bg-rose-400/15 dark:text-rose-300",
  sky: "bg-sky-50 text-sky-600 dark:bg-sky-400/15 dark:text-sky-300",
  slate: "bg-slate-100 text-slate-600 dark:bg-slate-400/15 dark:text-slate-300",
  teal: "bg-teal-50 text-teal-600 dark:bg-teal-400/15 dark:text-teal-300",
} as const;

export function IconChip({
  icon: Icon,
  glyph,
  tone = "blue",
  size = 36,
}: {
  icon?: LucideIcon;
  /** 品牌图标等自定义节点；提供时取代 `icon`。 */
  glyph?: ReactNode;
  tone?: keyof typeof chipTone;
  size?: number;
}) {
  return (
    <span
      className={cn("flex shrink-0 items-center justify-center rounded-[9px]", chipTone[tone])}
      style={{ width: size, height: size }}
    >
      {glyph ?? (Icon ? <Icon size={Math.round(size * 0.5)} strokeWidth={2} aria-hidden /> : null)}
    </span>
  );
}

/* ---------------- StatusBadge ---------------- */
export function StatusBadge({
  tone,
  children,
}: {
  tone: "run" | "stop" | "danger" | "warn" | "pending";
  children: ReactNode;
}) {
  if (tone === "pending") {
    return (
      <span className="inline-flex items-center gap-1.5 text-xs font-medium text-muted">
        <Loader2 size={12} strokeWidth={2} aria-hidden className="animate-spin" />
        <span>{children}</span>
      </span>
    );
  }
  const map = {
    run: { dot: "bg-run-dot", text: "text-run" },
    stop: { dot: "bg-faint", text: "text-muted" },
    danger: { dot: "bg-danger", text: "text-danger" },
    warn: { dot: "bg-warn", text: "text-warn" },
  } as const;
  const s = map[tone];
  return (
    <span className="inline-flex items-center gap-1.5 text-xs font-medium">
      <span className={cn("h-2 w-2 rounded-full", s.dot)} />
      <span className={s.text}>{children}</span>
    </span>
  );
}

/* ---------------- Tag（小标签） ---------------- */
export function Tag({
  children,
  tone = "slate",
}: {
  children: ReactNode;
  tone?: "slate" | "blue" | "green" | "amber" | "rose" | "violet";
}) {
  const tones = {
    slate: "bg-slate-100 text-slate-600 dark:bg-slate-400/15 dark:text-slate-300",
    // 蓝色标签只表示「当前/选中」，与交互主色同源（信息类标签一律中性 slate）。
    blue: "bg-accent-soft text-accent",
    // 绿色标签表示「在线/已连接」，走状态语义 token，暗色自动翻转。
    green: "bg-run-soft text-run",
    amber: "bg-amber-100 text-amber-800 dark:bg-amber-400/15 dark:text-amber-300",
    rose: "bg-rose-100 text-rose-700 dark:bg-rose-400/15 dark:text-rose-300",
    violet: "bg-violet-100 text-violet-700 dark:bg-violet-400/15 dark:text-violet-300",
  } as const;
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-md px-1.5 py-0.5 text-[11px] font-medium",
        tones[tone],
      )}
    >
      {children}
    </span>
  );
}

/* ---------------- RecommendedBadge ---------------- */
/**
 * 「特别推荐」烫金箔徽标：金属渐变 + 周期扫光 + 星芒（视觉见 index.css .recommend-badge）。
 * 文案由 children 传入，保持组件与 i18n 解耦。
 */
export function RecommendedBadge({ children }: { children: ReactNode }) {
  return (
    <span className="recommend-badge inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-semibold tracking-[0.02em] whitespace-nowrap">
      <Sparkles size={10} strokeWidth={2.5} className="recommend-badge__spark" aria-hidden />
      {children}
    </span>
  );
}

/* ---------------- CollapseToggle ---------------- */
/**
 * 小节折叠开关（macOS 披露三角）：▸ 旋转 90° 成 ▾，唯一动效。宽度随内容，
 * 悬停浮出底色（同 ghost Button 的语汇）。
 *
 * 只负责开关按钮本身，展开体由调用方按 open 条件渲染——不包内容容器，才能兼容
 * grid `contents` 这类对子元素层级敏感的布局。焦点环由全局 :focus-visible 提供。
 *
 * 用它之前先想清楚折叠是不是对的：内容默认收起就等于默认不被看见，
 * 只适合"多数人用不到的高级项"。成组的可选内容用 [[SettingsAccordion]]。
 */
export function CollapseToggle({
  open,
  onToggle,
  trailing,
  className,
  children,
}: {
  open: boolean;
  onToggle: () => void;
  /** 行尾附加内容（如分组项数），恒为 faint 等宽数字，不随悬停提亮。 */
  trailing?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-expanded={open}
      onClick={onToggle}
      className={cn(
        "group -mx-1.5 flex select-none items-center gap-1 rounded-md px-1.5 py-0.5 text-left font-medium text-[13px] text-ink transition-colors hover:bg-hover",
        className,
      )}
    >
      <ChevronRight
        size={14}
        strokeWidth={2.5}
        aria-hidden
        className={cn(
          "shrink-0 text-faint transition-transform duration-150 ease-out motion-reduce:transition-none",
          open && "rotate-90",
        )}
      />
      {children}
      {trailing != null ? (
        <span className="ml-auto shrink-0 font-normal text-faint tabular-nums">{trailing}</span>
      ) : null}
    </button>
  );
}

/* ---------------- SettingsAccordion（可折叠设置段） ---------------- */
/**
 * 一组可折叠的设置段，每段是一张独立卡片：左侧图标 + 标题，右侧状态摘要与展开箭头，
 * 展开体在分隔线下方铺开，高度带过渡动画。
 *
 * 卡片式而不是"一堆行挤在同一张卡里"：每段有自己的边界，扫视时一眼数得清有几段；
 * 摘要让收起态也读得出这一段配了什么——折叠是收纳，不是隐藏。
 *
 * 底层是 Radix Accordion（shadcn）：展开态、键盘操作与 aria 由它负责，多段可同时展开。
 */
export function SettingsAccordion({
  value,
  onValueChange,
  className,
  children,
}: {
  /** 展开中的段 id 列表（受控）。 */
  value: string[];
  onValueChange: (next: string[]) => void;
  className?: string;
  children: ReactNode;
}) {
  return (
    <Accordion
      type="multiple"
      value={value}
      onValueChange={onValueChange}
      // 一张纸装下所有折叠段，段间靠细线分隔。之前每段各是一张带边框和阴影的
      // 小卡，三段叠起来就是三圈边框——页面发灰的一个来源。
      className={cn(
        "divide-y divide-line overflow-hidden rounded-xl border border-line bg-card shadow-[var(--shadow-card)]",
        className,
      )}
    >
      {children}
    </Accordion>
  );
}

export function SettingsAccordionItem({
  value,
  icon: Icon,
  title,
  summary,
  children,
}: {
  value: string;
  /** 段落图标；同一组段落要么都给要么都不给，混着用会让标题参差。 */
  icon?: LucideIcon;
  title: string;
  /** 收起态的状态摘要（如「3 个备用地址」「未修改」）。 */
  summary?: ReactNode;
  children: ReactNode;
}) {
  return (
    // 边框/圆角/阴影都归容器（SettingsAccordion）管，段自身只负责内容与留白。
    <AccordionItem value={value} className="border-none">
      <AccordionTrigger className="items-center gap-2.5 px-7 py-3.5 hover:bg-hover hover:no-underline">
        {Icon ? (
          <Icon size={15} strokeWidth={2} aria-hidden className="shrink-0 text-faint" />
        ) : null}
        <span className="shrink-0 font-semibold text-[13px] text-ink">{title}</span>
        {summary != null ? (
          <span className="ml-auto min-w-0 truncate text-right text-xs text-faint">{summary}</span>
        ) : null}
      </AccordionTrigger>
      <AccordionContent className="border-line border-t px-7 pt-5 pb-6">
        {children}
      </AccordionContent>
    </AccordionItem>
  );
}

/* ---------------- SearchInput ---------------- */
/**
 * 搜索框。`size="sm"` 供边栏这类窄列使用——页面级搜索的 h-9 放进 208px 的
 * 预设列里会比它筛选的每一行都高，看着像个独立控件而不是列表的表头。
 *
 * 焦点态与 shadcn Input 同语汇（主色描边 + 光环），两者常出现在同一屏。
 */
export function SearchInput({
  className,
  size = "md",
  ...props
}: Omit<InputHTMLAttributes<HTMLInputElement>, "size"> & { size?: "sm" | "md" }) {
  const sm = size === "sm";
  return (
    <div className={cn("relative", className)}>
      <Search
        size={sm ? 14 : 15}
        strokeWidth={2}
        aria-hidden
        className={cn(
          "pointer-events-none absolute top-1/2 -translate-y-1/2 text-faint",
          sm ? "left-2.5" : "left-3",
        )}
      />
      <input
        type="search"
        className={cn(
          "w-full rounded-lg border border-line-strong bg-card pr-3 text-ink outline-none",
          "transition-[border-color,box-shadow] placeholder:text-faint",
          "focus-visible:border-accent focus-visible:ring-[3px] focus-visible:ring-accent/18",
          sm ? "h-8 pl-8 text-[13px]" : "h-9 pl-9 text-[13px]",
        )}
        {...props}
      />
    </div>
  );
}

/* ---------------- Field（输入框 / 下拉） ---------------- */
/** 依赖 React 19 的 ref-as-prop：react-hook-form register 的 ref 经 props
 * 透传到原生 input。若此组件被改造/降级（如包一层不透传 props 的中间层），
 * RHF 预填与聚焦会静默断链——改动前先跑 DataSection.test.tsx 的预填断言。 */
export function Field({ className, ...props }: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      className={cn(
        "h-9 rounded-lg border border-line-strong bg-card px-3 text-[13px] text-ink placeholder:text-faint focus:border-accent focus:outline-none",
        "disabled:cursor-not-allowed disabled:border-line disabled:bg-app disabled:text-faint",
        className,
      )}
      {...props}
    />
  );
}

/* ---------------- ModelCombobox（可搜索模型输入） ---------------- */
/**
 * 自由输入 + 候选过滤的模型名输入框。中转站的模型名经常不在「拉取模型」的结果里，
 * 所以主体必须是普通 Input（随便填），候选列表只是加速——`options` 为空时就是
 * 一个纯输入框，不弹层、不挡打字。过滤是大小写不敏感的子串匹配；↑↓ 选中、
 * Enter 采用、Esc 关闭；点候选走 onMouseDown 抢在 blur 前面，否则弹层先关掉。
 * 曾用原生 datalist：各平台过滤/键盘行为不一致，且样式不可控。
 */
export function ModelCombobox({
  value,
  onChange,
  options,
  placeholder,
  className,
  "aria-label": ariaLabel,
}: {
  value: string;
  onChange: (next: string) => void;
  options: string[];
  placeholder?: string;
  className?: string;
  "aria-label"?: string;
}) {
  const { t } = useTranslation();
  const listId = useId();
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const matches = useMemo(() => {
    const q = value.trim().toLowerCase();
    const hits = q ? options.filter((o) => o.toLowerCase().includes(q)) : options;
    return hits.slice(0, 50);
  }, [options, value]);
  const showList = open && matches.length > 0;

  const pick = (next: string) => {
    onChange(next);
    setOpen(false);
  };

  return (
    <Popover open={showList} onOpenChange={setOpen}>
      <PopoverAnchor asChild>
        <Input
          role="combobox"
          aria-label={ariaLabel}
          aria-expanded={showList}
          aria-controls={showList ? listId : undefined}
          aria-autocomplete="list"
          autoComplete="off"
          className={cn("font-mono", className)}
          placeholder={placeholder}
          value={value}
          onChange={(e) => {
            onChange(e.target.value);
            setActive(0);
            setOpen(true);
          }}
          onFocus={() => setOpen(true)}
          onBlur={() => setOpen(false)}
          onKeyDown={(e) => {
            if (!showList) {
              if (e.key === "ArrowDown" && options.length > 0) setOpen(true);
              return;
            }
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setActive((i) => (i + 1) % matches.length);
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setActive((i) => (i - 1 + matches.length) % matches.length);
            } else if (e.key === "Enter") {
              e.preventDefault();
              const hit = matches[Math.min(active, matches.length - 1)];
              if (hit !== undefined) pick(hit);
            } else if (e.key === "Escape") {
              e.stopPropagation();
              setOpen(false);
            }
          }}
        />
      </PopoverAnchor>
      <PopoverContent
        align="start"
        className="max-h-64 w-(--radix-popover-trigger-width) min-w-[16rem] overflow-y-auto"
        onOpenAutoFocus={(e) => e.preventDefault()}
        onCloseAutoFocus={(e) => e.preventDefault()}
      >
        <div id={listId} role="listbox" aria-label={t("common.modelCandidates")}>
          {matches.map((m, i) => (
            <div
              key={m}
              role="option"
              tabIndex={-1}
              aria-selected={i === active}
              className={cn(
                "cursor-default truncate rounded-sm px-2 py-1.5 font-mono text-[13px]",
                i === active ? "bg-hover text-ink" : "text-ink",
              )}
              onMouseEnter={() => setActive(i)}
              onMouseDown={(e) => {
                e.preventDefault();
                pick(m);
              }}
            >
              {m}
            </div>
          ))}
        </div>
      </PopoverContent>
    </Popover>
  );
}

/* ---------------- Textarea（多行输入） ---------------- */
/** 与 [[Field]] 同一套描边/焦点/禁用语汇，只是可多行、可竖向拉伸。
 * 高度与字体由调用方给（`h-16 font-mono text-xs` 等）——多行输入的行数是内容
 * 决定的，写死在组件里只会让每个调用方都来覆盖它。 */
export function Textarea({ className, ...props }: TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return (
    <textarea
      className={cn(
        "w-full resize-y rounded-lg border border-line-strong bg-card px-3 py-2 text-[13px] text-ink placeholder:text-faint focus:border-accent focus:outline-none",
        "disabled:cursor-not-allowed disabled:border-line disabled:bg-app disabled:text-faint",
        className,
      )}
      {...props}
    />
  );
}

/* ---------------- Modal（居中弹窗：backdrop + ESC/遮罩关闭） ---------------- */
const modalSize = {
  md: "max-w-lg",
  lg: "max-w-2xl",
  xl: "max-w-4xl",
} as const;

/**
 * 已挂载 Modal 的层序号。ESC 监听挂在 window 上，嵌套时两层都会收到同一次
 * 按键——一按全关，用户连外层编辑器里的位置也一并丢了。序号最大的那个才处理。
 *
 * 序号在**渲染期**取（`useState` 惰性初始化只跑一次），因为 React 的渲染是
 * 父先子后，正好等于视觉上的层序。不能改用 effect 里 push 一个栈：effect 是
 * **子先父后**执行的，那样算出来的"栈顶"恰恰是最外层，ESC 会关错弹窗——这个
 * 方向搞反过一次，是 ui.modal.test.tsx 那条嵌套用例抓出来的。
 */
let modalSeq = 0;
const mountedModals = new Set<number>();

/** 把焦点落到面板里第一个 / 最后一个真能聚焦的控件；禁用或隐藏的 focus() 无效，顺位跳过。 */
function focusEdge(panel: HTMLElement | null, edge: "first" | "last") {
  if (!panel) return;
  const candidates = Array.from(panel.querySelectorAll<HTMLElement>("*")).filter(
    (el) => el.tabIndex >= 0 && !el.hasAttribute("data-focus-edge"),
  );
  if (edge === "last") candidates.reverse();
  for (const el of candidates) {
    el.focus();
    if (document.activeElement === el) return;
  }
  panel.focus();
}

/**
 * 焦点哨兵：Tab 越过最后一个控件先落到尾哨兵，借它绕回第一个（Shift+Tab 反之）。
 * 不在 keydown 里算边界：WKWebView 默认 Tab 只走输入框、Option+Tab 才走全部控件，
 * 页面猜不出浏览器下一步落在哪；让浏览器自己走到哨兵，两种模式都对。
 */
function FocusEdge({ onFocus }: { onFocus: FocusEventHandler<HTMLSpanElement> }) {
  // 用 span 而非 button：WebKit 默认 Tab 跳过按钮，显式 tabIndex 的普通元素两种模式都会经过。
  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: onFocus 只做回绕转发，不是交互入口
    <span
      // biome-ignore lint/a11y/noNoninteractiveTabindex: 哨兵本就为 Tab 序列而设
      tabIndex={0}
      data-focus-edge=""
      onFocus={onFocus}
      className="pointer-events-none fixed opacity-0 outline-none"
    />
  );
}

export function Modal({
  title,
  size = "md",
  align = "center",
  onClose,
  children,
  footer,
}: {
  title: string;
  size?: keyof typeof modalSize;
  /** top：顶边固定，内容高度随输入变化时（命令面板过滤）框不上下跳。 */
  align?: "center" | "top";
  onClose: () => void;
  children: ReactNode;
  /** 底部操作区；不传则不渲染分隔条。 */
  footer?: ReactNode;
}) {
  const { t } = useTranslation();
  const overlayRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  // onClose 走 ref：调用方普遍传内联闭包，直接作依赖会让监听器随父组件
  // 每次渲染移除重挂，且重跑的 focus() 会夺走弹窗内输入框的焦点。
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  // 打开弹窗的那个元素，关闭时把焦点还回去。必须在渲染期取：等到 effect 里再读，
  // 弹窗内的 autoFocus 已经先一步把焦点抢走了。useState 的惰性初始化只跑一次，
  // 正好落在"弹窗还没挂上、焦点仍在触发者身上"的那一刻。
  const [opener] = useState(() =>
    typeof document === "undefined" ? null : (document.activeElement as HTMLElement | null),
  );
  // 本实例的层序号，见 mountedModals 的说明。
  const [layer] = useState(() => ++modalSeq);

  useEffect(() => {
    const overlay = overlayRef.current;
    const panel = panelRef.current;
    if (!overlay || !panel) return;
    // 子元素已用 autoFocus 抢到焦点时不夺回（否则把焦点拽回容器，输入框的
    // autoFocus 形同虚设）；只有无人聚焦时才回落到 panel 容器兜底。
    if (!panel.contains(document.activeElement)) panel.focus();
    mountedModals.add(layer);
    // 只有最上层的弹窗处理 ESC 与焦点陷阱，下层留着等它关掉。
    const isTop = () => layer === Math.max(...mountedModals);
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      // 弹窗里展开的 radix Select / Popover 已用这次 ESC 关了自己并 preventDefault，
      // 再关弹窗就是一按全关。
      if (event.defaultPrevented) return;
      if (!isTop()) return;
      onCloseRef.current();
    };
    // 焦点陷阱：背景页（同一棵应用树里、遮罩层外）一拿到焦点就拉回。不用 radix FocusScope：
    // 它只记得自己挂载后见过的焦点，菜单关闭把焦点还给背景触发器时无处可拉（真机复现过）。
    // radix 弹层 portal 在 body 下、不属于应用树，弹窗里展开的 Select / 菜单照常能拿焦点。
    let appRoot: HTMLElement = overlay;
    while (appRoot.parentElement && appRoot.parentElement !== document.body) {
      appRoot = appRoot.parentElement;
    }
    const active = document.activeElement;
    let lastInside = active instanceof HTMLElement && panel.contains(active) ? active : panel;
    const onFocusIn = (event: FocusEvent) => {
      const target = event.target;
      if (!(target instanceof Node)) return;
      if (overlay.contains(target)) {
        if (target instanceof HTMLElement) lastInside = target;
      } else if (isTop() && appRoot.contains(target)) {
        (lastInside.isConnected ? lastInside : panel).focus();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    document.addEventListener("focusin", onFocusIn);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      // 先撤陷阱再还焦：触发者在弹窗外，陷阱还在就会把它拽回来。
      document.removeEventListener("focusin", onFocusIn);
      mountedModals.delete(layer);
      // 触发者可能已经随弹窗里的操作消失（删完那行就没了），还在才还。
      if (opener?.isConnected) opener.focus();
    };
  }, [opener, layer]);

  return (
    <div
      ref={overlayRef}
      className={cn(
        "fixed inset-0 z-50 flex justify-center p-4",
        align === "top" ? "items-start pt-[12vh]" : "items-center",
      )}
    >
      {/* 从面板里 Shift+Tab 过来是回绕到末尾；从面板外进来（如焦点所在控件被删后
          从文档开头重新 Tab）是进入，落到第一个。 */}
      <FocusEdge
        onFocus={(event) =>
          focusEdge(
            panelRef.current,
            panelRef.current?.contains(event.relatedTarget) ? "last" : "first",
          )
        }
      />
      {/* 遮罩只给鼠标用；留在 Tab 序列里会多出一个看不见的焦点落点。 */}
      <button
        type="button"
        tabIndex={-1}
        aria-label={t("ui.closeModal")}
        className="modal-backdrop-in absolute inset-0 cursor-default bg-scrim"
        onClick={onClose}
      />
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        className={cn(
          "modal-panel-in relative flex max-h-[85vh] w-full flex-col rounded-xl border border-line bg-card shadow-[var(--shadow-pop)] outline-none",
          modalSize[size],
        )}
      >
        <div className="flex items-center justify-between border-b border-line px-5 py-3.5">
          <h2 className="text-sm font-semibold text-ink">{title}</h2>
          <IconButton icon={X} aria-label={t("ui.close")} onClick={onClose} />
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto p-5">{children}</div>
        {footer ? (
          <div className="flex items-center justify-end gap-2 border-t border-line px-5 py-3.5">
            {footer}
          </div>
        ) : null}
      </div>
      <FocusEdge onFocus={() => focusEdge(panelRef.current, "first")} />
    </div>
  );
}

/* ---------------- CopyButton（一键复制 + 图标切对勾反馈） ---------------- */
/**
 * 复制文本到剪贴板，复制后图标切成对勾并短暂显示「已复制」，1.5s 复位。
 * 传 label = 药丸带文字（CodeBlock 用）；不传 = 纯图标按钮（错误横幅等紧凑场景用）。
 * MCPane 惯例：复制这类非关键路径，剪贴板不可用时静默不报错。
 */
export function CopyButton({
  value,
  label,
  className,
}: {
  value: string;
  /** 按钮文字；不传则只显示图标。 */
  label?: string;
  className?: string;
}) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const resetTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  // 卸载时清理计时器（弹窗内复制后立刻关闭的场景）。
  useEffect(() => () => clearTimeout(resetTimer.current), []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      clearTimeout(resetTimer.current);
      resetTimer.current = setTimeout(() => setCopied(false), 1500);
    } catch {
      /* 剪贴板不可用（无 https/权限）时静默——复制非关键路径 */
    }
  };

  return (
    <button
      type="button"
      onClick={copy}
      aria-label={label ? undefined : copied ? t("ui.copied") : t("ui.copy")}
      className={cn(
        "inline-flex items-center gap-1 rounded-md border border-line-strong bg-card font-medium text-muted transition-colors hover:bg-hover hover:text-ink",
        label ? "px-2 py-1 text-[11px]" : "h-7 w-7 shrink-0 justify-center",
        className,
      )}
    >
      {copied ? (
        <Check size={12} strokeWidth={2} aria-hidden className="text-run" />
      ) : (
        <Copy size={12} strokeWidth={2} aria-hidden />
      )}
      {label ? (copied ? t("ui.copied") : label) : null}
    </button>
  );
}

/* ---------------- CodeBlock（等宽代码块 + 一键复制） ---------------- */
export function CodeBlock({ code, className }: { code: string; className?: string }) {
  const { t } = useTranslation();
  return (
    <div className={cn("relative rounded-lg border border-line bg-app", className)}>
      <pre className="overflow-x-auto whitespace-pre-wrap break-all p-3 pr-20 font-mono text-xs leading-5 text-ink">
        {code}
      </pre>
      <CopyButton value={code} label={t("ui.copy")} className="absolute top-2 right-2" />
    </div>
  );
}

/* ---------------- JsonView（只读 JSON 语法高亮 + 一键复制） ---------------- */
/**
 * 日志载荷这类只读 JSON 用它，不要拉 CodeMirror：一个 basicSetup 实例带着历史、
 * 快捷键与 lint，为展示几十行文本太重，何况详情弹窗里要开两个。
 * 配色直接引 --cm-*，与配置编辑器同一套语汇，主题切换自动跟随。
 */
const JSON_TOKEN =
  /("(?:\\.|[^"\\])*")|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)|\b(?:true|false|null)\b/g;

function highlightJson(code: string): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  let seq = 0;
  for (const match of code.matchAll(JSON_TOKEN)) {
    const start = match.index;
    if (start > last) out.push(code.slice(last, start));
    const [text, str, num] = match;
    // 后面紧跟冒号的字符串是键名，与字符串值分色——读长 JSON 时全靠这个找字段。
    const isKey = str != null && /^\s*:/.test(code.slice(start + text.length));
    const color = str
      ? isKey
        ? "--cm-property"
        : "--cm-string"
      : num
        ? "--cm-number"
        : "--cm-keyword";
    out.push(
      <span key={`${start}-${seq++}`} style={{ color: `var(${color})` }}>
        {text}
      </span>,
    );
    last = start + text.length;
  }
  if (last < code.length) out.push(code.slice(last));
  return out;
}

export function JsonView({
  code,
  className,
  /** 高度上限（Tailwind max-h-*）；加在滚动体 pre 上，不是外框——加错地方会溢出压住下文。 */
  maxHeightClassName = "max-h-72",
}: {
  code: string;
  className?: string;
  maxHeightClassName?: string;
}) {
  const { t } = useTranslation();
  // 截断过的载荷、纯文本错误原文都进得来：不像 JSON 就原样展示，别硬套配色。
  const nodes = useMemo(() => (/^\s*[[{]/.test(code) ? highlightJson(code) : [code]), [code]);
  return (
    <div className={cn("relative overflow-hidden rounded-lg border border-line bg-app", className)}>
      <pre
        className={cn(
          "overflow-auto whitespace-pre-wrap break-all p-3 pr-20 font-mono text-xs leading-5 text-[var(--cm-punct)]",
          maxHeightClassName,
        )}
      >
        {nodes}
      </pre>
      <CopyButton value={code} label={t("ui.copy")} className="absolute top-2 right-2" />
    </div>
  );
}

/* ---------------- FieldLabel ---------------- */
export function LabeledField({
  label,
  required,
  hint,
  error,
  children,
}: {
  label: string;
  /** 必填标记（红色星号）。 */
  required?: boolean;
  /** 字段下方的辅助说明文案。 */
  hint?: string;
  /** 校验错误（react-hook-form 等表单库的字段级报错）；有错时替代 hint 展示。 */
  error?: string;
  children: ReactNode;
}) {
  return (
    // biome-ignore lint/a11y/noLabelWithoutControl: 控件通过 children 传入并被此 label 包裹
    <label className="flex flex-col gap-1.5">
      <span className="text-xs font-medium text-muted">
        {label}
        {required ? <span className="ml-0.5 text-danger">*</span> : null}
      </span>
      {children}
      {error ? (
        <span className="text-[11px] text-danger">{error}</span>
      ) : hint ? (
        <span className="text-[11px] text-faint">{hint}</span>
      ) : null}
    </label>
  );
}

/* ---------------- FormField（表单字段：标签在上，标签行右挂操作） ---------------- */
/**
 * 表单主体用的字段块：标签在上、控件全宽、提示在下，标签行右侧可以挂一个只服务
 * 于本字段的次要操作（「获取 API Key」「拉取模型」「测速」）。
 *
 * 操作挂在标签行而不是另起一行，是因为它属于这个字段而不属于表单流程——
 * 视线扫到标签时顺手就能看到它，也不会把控件的竖排节奏打断。
 *
 * 标签走 shadcn 的 Label（Radix），htmlFor 与控件 id 关联后点标签即聚焦控件。
 * 轻量场景（设置页里零星一两个字段）仍用 [[LabeledField]]。
 */
export function FormField({
  id,
  label,
  required,
  hint,
  error,
  action,
  children,
}: {
  /** 控件 id；传了才能点标签聚焦，同时作为 hint 的 aria-describedby。 */
  id?: string;
  label: string;
  required?: boolean;
  hint?: ReactNode;
  error?: string;
  /** 标签行右侧的次要操作。 */
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="space-y-2">
      <div className="flex min-h-8 items-center justify-between gap-3">
        <Label htmlFor={id} className="gap-1 text-[13px] text-ink">
          {label}
          {required ? <span className="text-danger">*</span> : null}
        </Label>
        {action}
      </div>
      {children}
      {error ? (
        <p id={id ? `${id}-hint` : undefined} className="text-[11px] text-danger">
          {error}
        </p>
      ) : hint ? (
        <p id={id ? `${id}-hint` : undefined} className="text-[11px] text-faint leading-relaxed">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

/* ---------------- SheetHero（纸面抬头：图标板 + 大标题 + 副标题） ---------------- */
/**
 * 整页表单最上面那一块，负责一眼交代"正在配的是谁"。
 *
 * 它是全页唯一的大字号（21px），下面所有段名、标签、提示都明显更小——层级靠
 * 字号差拉开，而不是靠给抬头压一层灰底。图标板同理：中性板托彩色品牌 logo，
 * 没有 logo 可托时（自定义供应商）换成主色板，免得抬头整块是灰的。
 */
export function SheetHero({
  glyph,
  tone = "brand",
  title,
  subtitle,
  badge,
}: {
  glyph: ReactNode;
  /** brand = 中性板托彩色 logo；accent = 主色板，用于没有品牌图标的自定义项。 */
  tone?: "brand" | "accent";
  title: string;
  /** 副标题样式由调用方定（域名走 mono，说明文字走正文），这里只管位置。 */
  subtitle?: ReactNode;
  badge?: ReactNode;
}) {
  return (
    <div className="flex items-center gap-4 px-7 pt-6 pb-5">
      <span
        aria-hidden
        className={cn(
          "flex h-14 w-14 shrink-0 items-center justify-center rounded-[16px]",
          tone === "accent" ? "bg-accent-soft text-accent" : "bg-slot",
        )}
      >
        {glyph}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <h2 className="truncate font-semibold text-[21px] text-ink leading-tight tracking-[-0.02em]">
            {title}
          </h2>
          {badge}
        </div>
        {subtitle ? <div className="mt-1.5 min-w-0 truncate">{subtitle}</div> : null}
      </div>
    </div>
  );
}

/* ---------------- FormSection（接线段：左轨节点 + 段名 + 字段区） ---------------- */
/**
 * 表单主路径的分段。左侧一列是「轨」——每段一个节点，节点之间用细线连起来，
 * 填好的段亮成主色实心，没填的留空心。
 *
 * 为什么是轨而不是一堆并列的字段：接一个供应商本来就是有序的三步（这是谁 →
 * 接口在哪 → 拿什么调用），后一步在前一步没定之前没有意义。轨把这层依赖画了
 * 出来，顺带把"还差哪一步"变成扫一眼就知道的事，不必再在卡底常驻一行灰字
 * 逐项念必填项。
 *
 * `state` 只描述本段是否已备齐，不做校验——文案与规则留在调用方。
 */
export function FormSection({
  label,
  state,
  last,
  children,
}: {
  /** 段名（「身份」「端点」「凭证」）。 */
  label: string;
  /** done = 本段必填项已齐；todo = 还缺；optional = 本段没有必填项。 */
  state: "done" | "todo" | "optional";
  /** 末段不画向下的连接线，否则轨会拖出一截断头。 */
  last?: boolean;
  children: ReactNode;
}) {
  return (
    // 段名列按最长的段名收紧（中文两字、英文 Endpoint 都放得下）：留太宽的话
    // 段名和它领的字段之间会空出一大条，读起来像两栏而不是一段。
    <div className="grid grid-cols-[76px_minmax(0,1fr)] gap-x-4">
      <div className="relative">
        {/* 连接线：起于节点下沿，止于本段内容底部——段间无间距，故与下一段的节点接得上。 */}
        {last ? null : (
          <span aria-hidden className="absolute top-5 bottom-0 left-[3px] w-px bg-line-strong" />
        )}
        <div className="flex items-center gap-2">
          <span
            aria-hidden
            className={cn(
              "h-[7px] w-[7px] shrink-0 rounded-full transition-colors",
              state === "done"
                ? "bg-accent"
                : state === "todo"
                  ? "border-[1.5px] border-line-strong bg-card"
                  : "bg-line-strong",
            )}
          />
          <span className="truncate font-semibold text-[13px] text-ink tracking-[0.01em]">
            {label}
          </span>
        </div>
      </div>
      {/* 末段不留下边距，纸面以字段自身的留白收口。 */}
      <div className={cn("min-w-0", last ? "pb-1" : "pb-7")}>{children}</div>
    </div>
  );
}

/* ---------------- InlineNote（操作结果的就地反馈） ---------------- */
/**
 * 按钮旁边那一句「已保存 / 清理失败」。MCPane 惯例是不弹 toast、就地内联，
 * 但此前各处都把成功和失败渲染成同一种灰色小字——失败长得和成功一模一样，
 * 扫一眼根本分不出刚才那下成功没有。这里用颜色 + 图标把两者分开。
 *
 * `role="status"` + `aria-live="polite"`：操作结果对读屏用户同样要播报，
 * 否则点完按钮界面"什么也没发生"。
 *
 * 长错误（后端返回的路径/堆栈）用 [[ErrorBanner]]，它带整段复制。
 */
export function InlineNote({
  tone,
  children,
  className,
}: {
  tone: "ok" | "error";
  children: ReactNode;
  className?: string;
}) {
  const Icon = tone === "ok" ? Check : TriangleAlert;
  return (
    <span
      role="status"
      aria-live="polite"
      className={cn(
        "inline-flex min-w-0 items-center gap-1.5 text-xs",
        tone === "ok" ? "text-run" : "text-danger",
        className,
      )}
    >
      <Icon size={13} strokeWidth={2.5} aria-hidden className="shrink-0" />
      <span className="min-w-0 truncate">{children}</span>
    </span>
  );
}

/* ---------------- ErrorBanner ---------------- */
/**
 * 操作失败的内联提示（MCPane 惯例：不弹 toast，就地内联报错）。
 * mutation 的 onError 把错误文案塞进来，可选 onDismiss 手动关闭。
 * message 为空即不渲染，调用方无需自己判空。
 */
export function ErrorBanner({
  message,
  onDismiss,
}: {
  message: string | null | undefined;
  onDismiss?: () => void;
}) {
  const { t } = useTranslation();
  if (!message) return null;
  return (
    <div
      role="alert"
      className="mb-4 flex items-start gap-2.5 rounded-lg border border-danger-line bg-danger-soft p-3"
    >
      <TriangleAlert
        size={16}
        strokeWidth={2}
        aria-hidden
        className="mt-0.5 shrink-0 text-danger"
      />
      <p className="min-w-0 flex-1 whitespace-pre-wrap break-words text-sm text-danger">
        {message}
      </p>
      {/* 一键复制错误全文：Tauri/后端错误往往长且含路径，便于用户带走排查上报。 */}
      <CopyButton value={message} className="mt-0.5" />
      {onDismiss ? (
        <IconButton icon={X} tone="danger" aria-label={t("common.dismiss")} onClick={onDismiss} />
      ) : null}
    </div>
  );
}

/* ---------------- BrandMark（MCPane 应用标记：终端提示符 ❯ + 四格面板） ---------------- */
/**
 * 品牌签名标记。chevron 与三格随 currentColor（跟随所在容器字色），
 * 右上格固定品牌绿 #3fae5a（存活/连接的语义色）。Shell 头部与空态共用同一标记。
 */
export function BrandMark({ size = 24, className }: { size?: number; className?: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      aria-hidden="true"
      className={className}
    >
      <path
        d="M4.5 7.5 9 12l-4.5 4.5"
        stroke="currentColor"
        strokeWidth="2.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <rect x="12.5" y="7" width="4.6" height="4.6" rx="1" fill="currentColor" />
      <rect x="18.4" y="7" width="4.6" height="4.6" rx="1" fill="#3fae5a" />
      <rect x="12.5" y="12.9" width="4.6" height="4.6" rx="1" fill="currentColor" />
      <rect x="18.4" y="12.9" width="4.6" height="4.6" rx="1" fill="currentColor" />
    </svg>
  );
}

/* ---------------- EmptyState（空态：品牌标记 + 邀请文案 + 可选 CTA） ---------------- */
/**
 * 列表空时的统一空态。签名：用 MCPane 自己的终端提示符标记取代通用图标——
 * 提示符即「轮到你了」，让"什么都没有"的时刻成为品牌化的行动邀请（设计上签名只花在此处，其余保持扁平克制）。
 */
export function EmptyState({
  title,
  description,
  action,
  className,
}: {
  title: string;
  description?: string;
  /** 主行动按钮等；不传则只展示标记 + 文案。 */
  action?: ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex flex-col items-center rounded-xl border border-dashed border-line-strong px-5 py-8 text-center",
        className,
      )}
    >
      {/* 与应用图标同一套颜色：墨色提示符 + 一格绿，衬在中性凹槽上（不染主色蓝）。 */}
      <span className="flex h-12 w-12 items-center justify-center rounded-[13px] border border-line bg-slot text-ink">
        <BrandMark size={26} />
      </span>
      <p className="mt-3.5 text-sm font-medium text-ink">{title}</p>
      {description ? <p className="mt-1 max-w-md text-xs text-muted">{description}</p> : null}
      {action ? <div className="mt-4">{action}</div> : null}
    </div>
  );
}

/**
 * 日期时间跟随**界面语言**而不是浏览器 locale。
 *
 * 之前用的是 `Intl.DateTimeFormat(undefined, …)`——它读的是系统 locale，于是
 * 中文界面里会冒出 "Jun 11" 和 "08:41 PM"。语言是用户在这个应用里选的，
 * 时间格式没有理由不跟着走。
 *
 * 放在 lib 而非某个组件内：备份列表与配置历史都要按同一套规则显示时间，各写
 * 一份的结果就是历史面板漏掉了这条规则、继续用 toLocaleString() 读系统 locale。
 *
 * 格式化器按「用途 + 语言」缓存：构造 Intl.DateTimeFormat 不便宜，而列表
 * 每行都要用一次。
 */
const DATE_OPTS = {
  time: { month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" },
  /** 保留窗口两端的刻度：只到日，秒级精度在"最早一条是什么时候"上是噪音。 */
  day: { month: "short", day: "numeric" },
  /** 日志表的时刻列：要到秒，不要日期——整屏都是刚发生的事，日期是重复噪音。 */
  clock: { hour: "2-digit", minute: "2-digit", second: "2-digit" },
  /** 额度的更新时刻：每 5 分钟就刷一次，日期和秒都是噪音。 */
  minute: { hour: "2-digit", minute: "2-digit" },
  /** 详情弹窗：完整日期时间，一条记录要能对上会话。 */
  full: {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  },
  /** 图表横轴：紧凑的两位月/日，一根轴上要塞下几十个刻度。 */
  chartDay: { month: "2-digit", day: "2-digit" },
} as const satisfies Record<string, Intl.DateTimeFormatOptions>;

const formatters = new Map<string, Intl.DateTimeFormat>();

export function formatAt(lang: string, kind: keyof typeof DATE_OPTS, seconds: number): string {
  const key = `${kind}:${lang}`;
  let formatter = formatters.get(key);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat(lang.startsWith("en") ? "en-US" : "zh-CN", DATE_OPTS[kind]);
    formatters.set(key, formatter);
  }
  return formatter.format(seconds * 1000);
}

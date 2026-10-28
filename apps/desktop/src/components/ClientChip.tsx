import { BrandGlyph, type BrandName, isBrandName, SurfaceBadge } from "./BrandIcon";
import { IconChip } from "./ui";

/** 品牌图标色块的色调（未安装统一灰）。 */
export const BRAND_TONE: Record<BrandName, "blue" | "amber" | "teal" | "sky" | "slate"> = {
  claude: "amber",
  "claude-desktop": "amber",
  codex: "teal",
  cursor: "slate",
  vscode: "blue",
  cline: "sky",
  devin: "teal",
  gemini: "blue",
  "gemini-cli": "blue",
  // Antigravity 的官方标是纯黑单色，没有可用的品牌色。
  antigravity: "slate",
  grok: "slate",
  opencode: "slate",
  openclaw: "slate",
  hermes: "slate",
  // 国内四家：Trae 与 Qoder 官方 logo 本就是黑白，没有可用的品牌色。
  trae: "slate",
  qoder: "slate",
  "qwen-code": "blue",
  codebuddy: "teal",
  zcode: "slate",
};

const TONE_TEXT = {
  blue: "text-blue-600 dark:text-blue-300",
  amber: "text-amber-600 dark:text-orange-300",
  teal: "text-teal-600 dark:text-teal-300",
  sky: "text-sky-600 dark:text-sky-300",
  slate: "text-ink",
} as const;

/** 裸 logo（不带色块）的品牌字色，与 ClientChip 同一套色调；黑白 logo 落到墨色。 */
export function brandTextClass(id: string): string {
  return isBrandName(id) ? TONE_TEXT[BRAND_TONE[id]] : "text-ink";
}

/**
 * 客户端的品牌图标胶囊：客户端页与概览页共用，同一个客户端在哪儿都长一个样。
 * 形态角标贴在色块外沿：Claude Code 与 Claude Desktop 是同一个 logo，不标就会看漏。
 */
export function ClientChip({
  id,
  title,
  installed = true,
  size = 34,
}: {
  id: string;
  title: string;
  installed?: boolean;
  size?: number;
}) {
  return (
    <span className="relative inline-flex shrink-0">
      <IconChip
        glyph={
          // 自定义客户端没有品牌 logo，用首字母顶上，别留一个空色块。
          isBrandName(id) ? (
            <BrandGlyph name={id} size={Math.round(size * 0.53)} muted={!installed} />
          ) : (
            <span className="font-semibold text-[13px]">{title.slice(0, 1).toUpperCase()}</span>
          )
        }
        tone={installed && isBrandName(id) ? BRAND_TONE[id] : "slate"}
        size={size}
      />
      <SurfaceBadge app={id} size={Math.round(size * 0.41)} className="border-card bg-hover" />
    </span>
  );
}

const compactFormatter = new Intl.NumberFormat(undefined, {
  notation: "compact",
  maximumFractionDigits: 1,
});

/** token 数量紧凑显示（12.3K / 1.2M）。 */
export function formatTokens(n: number): string {
  return compactFormatter.format(n);
}

/**
 * 时长格式化：最多两级单位，避免「86400s」式的原始秒数。
 * 概览页运行时长等场景使用；输入为秒（可含小数，向下取整）。
 *
 * 收 `t`：单位不是靠拼接能跨语言的东西——中文写「3 天 4 小时」，英文写
 * "3d 4h"，连要不要空格都不一样。四条整句 key 各管一档。
 */
export function formatDuration(
  totalSecs: number,
  t: (key: string, options?: Record<string, unknown>) => string,
): string {
  const s = Math.max(0, Math.floor(totalSecs));
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const mins = Math.floor((s % 3600) / 60);
  const secs = s % 60;
  if (days > 0) return t("duration.dh", { days, hours });
  if (hours > 0) return t("duration.hm", { hours, mins });
  if (mins > 0) return t("duration.ms", { mins, secs });
  return t("duration.s", { secs });
}

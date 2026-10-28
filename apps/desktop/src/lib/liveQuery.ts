/**
 * 页面级轮询约定。
 *
 * App 按当前页条件渲染，切页即卸载 observer，`refetchInterval` 随之停。
 * 本 helper 把间隔、staleTime、retry 收成一处，避免各页手写漂移。
 */
export const QUERY_STALE_MS = 2_000;

export function livePoll(intervalMs: number) {
  return {
    staleTime: QUERY_STALE_MS,
    refetchInterval: intervalMs,
    retry: false as const,
  };
}

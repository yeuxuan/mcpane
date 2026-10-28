import type { QueryClient } from "@tanstack/react-query";
import type { ClientApp } from "./ipc";
import { queryKeys } from "./queryKeys";

/**
 * 供应商切换/增删后的查询失效集合。从 Providers 页的 `refresh()` 抽出，
 * 命令面板的「切换供应商」复用同一份，避免两处失效逻辑各写一遍后来跑偏。
 */
export function invalidateProviderQueries(queryClient: QueryClient, app: ClientApp) {
  void queryClient.invalidateQueries({ queryKey: queryKeys.providers });
  void queryClient.invalidateQueries({ queryKey: queryKeys.providerCurrent });
  void queryClient.invalidateQueries({ queryKey: queryKeys.providersUnified });
  void queryClient.invalidateQueries({ queryKey: queryKeys.providerLiveIds(app) });
  // 启用/移除会改变数组遮蔽关系，"Hermes 托管"分组要跟着刷新。
  void queryClient.invalidateQueries({ queryKey: queryKeys.hermesExternal });
}

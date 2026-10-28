import type { QueryClient } from "@tanstack/react-query";
import { queryKeys } from "./queryKeys";

/**
 * Server 增删/启停/导入后的查询失效集合：列表、Hub 诊断、各工作区 Catalog（工具数随之变）。
 * 服务器页、添加弹窗、导入面板、命令面板与 cc-switch 迁移共用，免得各写一份后来跑偏。
 */
export function invalidateServerQueries(queryClient: QueryClient) {
  void queryClient.invalidateQueries({ queryKey: queryKeys.servers });
  void queryClient.invalidateQueries({ queryKey: queryKeys.hubDiagnostics });
  void queryClient.invalidateQueries({ queryKey: queryKeys.catalogAll });
}

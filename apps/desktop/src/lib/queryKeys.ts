import type { ClientTypeId } from "./ipc";

/**
 * 集中管理 react-query 的 query key，避免裸字符串散落各处导致失配。
 * 无参数的 key 用只读元组常量；带参数的 key 用函数返回 as const 元组。
 */
export const queryKeys = {
  servers: ["servers"],
  /** 导入面板的预览：按粘贴/选择的原文指纹缓存（同一段文本不重复解析）。 */
  serverImportPreview: (fingerprint: string) => ["server-import-preview", fingerprint] as const,
  /** 导出框的内容：按勾选的 id 集合（已排序拼接）缓存。 */
  serverExport: (ids: string) => ["server-export", ids] as const,
  hubStatus: ["hub-status"],
  hubDiagnostics: ["hub-diagnostics"],
  hubLogs: ["hub-logs"],
  hubProcessState: ["hub-process-state"],
  hubEnabled: ["hub-enabled"],
  proxy: ["proxy"],
  configHistory: ["config-history"],
  providers: ["providers"],
  providerCurrent: ["provider-current"],
  clientsInstalled: ["clients-installed"],
  clientEnv: ["client-env"],
  configDirs: ["config-dirs"],
  providerLiveIds: (app: string) => ["provider-live-ids", app] as const,
  providersUnified: ["providers-unified"],
  claudeDesktopStatus: ["claude-desktop-status"],
  desktopProxyStatus: ["desktop-proxy-status"],
  providerRouting: (app: string) => ["provider-routing", app] as const,
  routerStatus: ["router-status"],
  routerRequests: (app: string) => ["router-requests", app] as const,
  workspaces: ["workspaces"],
  appInfo: ["app-info"],
  clients: ["clients"],
  mcpClients: ["mcp-clients"],
  catalog: (workspaceId: string) => ["catalog", workspaceId] as const,
  /** 前缀失效用：Server 增删/启停影响**所有**工作区的 Catalog，
   * invalidateQueries 按前缀匹配一次清族（勿再手写裸 ["catalog"]）。 */
  catalogAll: ["catalog"] as const,
  toolOverrides: (workspaceId: string) => ["tool-overrides", workspaceId] as const,
  itemOverrides: (kind: string, workspaceId: string) =>
    ["item-overrides", kind, workspaceId] as const,
  toolDiscovery: (workspaceId: string) => ["tool-discovery", workspaceId] as const,
  usageGuidance: (workspaceId: string) => ["usage-guidance", workspaceId] as const,
  guidanceTargets: (workspaceId: string) => ["guidance-targets", workspaceId] as const,
  guidancePreview: (workspaceId: string) => ["guidance-preview", workspaceId] as const,
  autoUpdateCheck: ["auto-update-check"] as const,
  pricingSync: ["pricing-sync"] as const,
  hermesExternal: ["hermes-external"] as const,
  autostart: ["autostart"] as const,
  /** cc-switch 迁移面板的探测结果（向导与设置页共用同一份）。 */
  ccswitchDetect: ["ccswitch-detect"] as const,
  logStats: (hours: number) => ["log-stats", hours] as const,
  logDetail: (id: string) => ["log-detail", id] as const,
  /** 前缀失效用：清理日志表会影响所有时间窗的统计，一次清族。 */
  logStatsAll: ["log-stats"] as const,
  workspaceServers: (workspaceId: string) => ["workspace-servers", workspaceId] as const,
  /** 前缀失效用：成员集变更影响所有工作区的成员查询。 */
  workspaceServersAll: ["workspace-servers"] as const,
  /** 服务器编辑弹窗「所属工作区」区块：一次拉全部自建工作区的成员集。 */
  workspaceMembership: (workspaceIds: string[]) =>
    ["workspace-membership", workspaceIds.join("|")] as const,
  /** 前缀失效用：成员集按工作区 id 组合缓存，改一处要清整族。 */
  workspaceMembershipAll: ["workspace-membership"] as const,
  configSnippet: (clientId: ClientTypeId) => ["config-snippet", clientId] as const,
  /** 访问控制弹窗：某连接的 Server deny-list。 */
  connectionDeniedServers: (connectionId: string) =>
    ["connection-denied-servers", connectionId] as const,
  // Usage 页族：统一 "usage" 前缀，同步完成后按前缀整族失效。
  usageSummary: (hours: number) => ["usage", "summary", hours] as const,
  usageTrend: (hours: number) => ["usage", "trend", hours] as const,
  usageByModel: (hours: number) => ["usage", "by-model", hours] as const,
  usageByProject: (hours: number) => ["usage", "by-project", hours] as const,
  usageByApp: (hours: number) => ["usage", "by-app", hours] as const,
  // 故意不挂 "usage" 前缀：用量同步会按前缀整族失效，额度不该跟着重查（订阅额度要读 CLI 凭据）。
  providerQuota: (providerId: string) => ["provider-quota", providerId] as const,
  /** 前缀失效用：同步/清理后整族重取。 */
  usageAll: ["usage"] as const,
  backups: ["backups"] as const,
  syncSettings: ["sync-settings"] as const,
  dataOverview: ["data-overview"] as const,
} as const;

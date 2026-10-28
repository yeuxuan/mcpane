import { useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { TriangleAlert, X } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { IconButton } from "./ui";

/** 后端 `config-externally-changed` 的载荷（config_watch.rs）。 */
type ExternalChange = { path: string };

/**
 * 客户端配置被 MCPane 之外的程序改动时的提醒条。
 *
 * 两条来源缺一不可：
 * - **挂载时拉取**：窗口关着时（轻量模式 60s 后销毁 WebView）发生的改动，
 *   其 emit 无人接收——而用户去终端改配置恰恰就在窗口关着的时候；
 * - **事件订阅**：窗口开着时即时提醒。
 *
 * 后端已用 history 的 hash_after 滤掉 MCPane 自己的写入并按内容去重，
 * 故此处收到的都是值得打扰用户的真实外部改动，直接展示即可。
 */
export function ExternalChangeBanner() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [paths, setPaths] = useState<string[]>([]);

  // useCallback 不是为了性能：下面的 effect 要订阅事件且只应挂载一次，
  // 而 queryClient 由 useQueryClient 提供、引用稳定。不包起来的话这个函数
  // 每次渲染都是新引用，进依赖数组会让 effect 反复重订阅。
  const invalidateClientCaches = useCallback(() => {
    queryClient.invalidateQueries({ queryKey: queryKeys.clients });
    queryClient.invalidateQueries({ queryKey: queryKeys.mcpClients });
    queryClient.invalidateQueries({ queryKey: queryKeys.hubStatus });
    queryClient.invalidateQueries({ queryKey: queryKeys.providers });
    queryClient.invalidateQueries({ queryKey: queryKeys.providerCurrent });
  }, [queryClient]);

  useEffect(() => {
    const merge = (incoming: string[]) => {
      setPaths((prev) => [...new Set([...prev, ...incoming])]);
      if (incoming.length > 0) {
        invalidateClientCaches();
      }
    };

    // 取走即清空：用户关掉提醒后，重建 WebView 不该再弹同一条。
    ipc
      .configExternalChanges()
      .then(merge)
      .catch(() => {});

    const unlisten = listen<ExternalChange>("config-externally-changed", (event) => {
      merge([event.payload.path]);
    }).catch(() => () => {});
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [invalidateClientCaches]);

  if (paths.length === 0) return null;

  return (
    <div
      role="status"
      className="flex shrink-0 items-start gap-2.5 border-b border-line bg-warn-soft px-4 py-2.5"
    >
      <TriangleAlert aria-hidden className="mt-px h-4 w-4 shrink-0 text-warn" />
      <div className="min-w-0 flex-1">
        <p className="text-xs font-medium text-ink">{t("configWatch.title")}</p>
        <p className="mt-0.5 text-[11px] text-muted">{t("configWatch.desc")}</p>
        <ul className="mt-1 space-y-0.5">
          {paths.map((path) => (
            <li key={path} className="truncate font-mono text-[11px] text-warn" title={path}>
              {path}
            </li>
          ))}
        </ul>
      </div>
      <IconButton
        icon={X}
        onClick={() => setPaths([])}
        aria-label={t("configWatch.dismiss")}
        title={t("configWatch.dismiss")}
      />
    </div>
  );
}

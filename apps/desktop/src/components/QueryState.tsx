/**
 * 查询三态渲染：pending 出加载文案、失败出错误文案、成功交给 children。
 *
 * 抽出来的原因是 `query.isSuccess ? 内容 : 加载中` 这个写法——它把失败也归进
 * 「加载中」分支，而这些查询多数设了 `retry: false`，一次失败就永久定格。
 * 用户看到的不是「读取失败」，而是一句平静且错误的事实陈述：「未检测到客户端」
 * 「本周期零调用」「还没有启用的服务器」。四个页面各错一遍，是同一个模式问题。
 *
 * 不放进 `ui.tsx`：那里是零业务依赖的 UI 原语集合，不该引入 react-query 类型。
 *
 * 三态的呈现由调用方给（各页面的排版差异大：整页 Card、卡片内小段落都有），
 * 本组件只负责把 error 这一态从 loading 里分出来、并把错误归一化成可读文案。
 */
import type { UseQueryResult } from "@tanstack/react-query";
import type { ReactNode } from "react";

import { errorText } from "../lib/utils";

export function QueryState<T>({
  query,
  loading,
  error,
  children,
}: {
  query: Pick<UseQueryResult<T>, "isSuccess" | "isError" | "data" | "error">;
  /** pending 态的呈现。 */
  loading: ReactNode;
  /** 失败态的呈现，入参是已归一化的错误文案。 */
  error: (message: string) => ReactNode;
  /** 成功态：data 此时必定就绪。 */
  children: (data: T) => ReactNode;
}) {
  if (query.isError) return <>{error(errorText(query.error))}</>;
  if (query.isSuccess) return <>{children(query.data as T)}</>;
  return <>{loading}</>;
}

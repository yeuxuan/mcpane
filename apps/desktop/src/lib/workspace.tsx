import { useQuery } from "@tanstack/react-query";
import { createContext, type ReactNode, useContext, useMemo, useState } from "react";
import { ipc, type Workspace } from "./ipc";
import { queryKeys } from "./queryKeys";

/** 默认工作区：聚合全部启用的下游服务器（ADR-004）。 */
export const DEFAULT_WS = "ws_default";

// IPC 载荷类型的正主在 lib/ipc.ts，这里 re-export 以维持既有引用路径。
export type { Workspace };

interface WorkspaceContextValue {
  /** 当前全局选中的工作区 id（顶右切换器与管理页共享）。 */
  selectedId: string;
  setSelectedId: (id: string) => void;
  workspaces: Workspace[];
}

const WorkspaceContext = createContext<WorkspaceContextValue | null>(null);

/**
 * 工作区是笼罩全局的"当前语境"而非一个页面：顶右切换器改的就是这里，
 * 服务器分组 / 工具目录 / 连接绑定都读同一个 selectedId。
 */
export function WorkspaceProvider({ children }: { children: ReactNode }) {
  const [selectedId, setSelectedId] = useState(DEFAULT_WS);
  const workspaces = useQuery({
    queryKey: queryKeys.workspaces,
    queryFn: ipc.workspacesList,
    retry: false,
  });

  // value 必须 memo：字面量对象（含 ?? [] 新数组）会让 Provider 任意重渲染级联全部 consumer。
  const data = workspaces.data;
  const value = useMemo(
    () => ({ selectedId, setSelectedId, workspaces: data ?? [] }),
    [selectedId, data],
  );

  return <WorkspaceContext.Provider value={value}>{children}</WorkspaceContext.Provider>;
}

export function useWorkspace() {
  const value = useContext(WorkspaceContext);
  if (!value) throw new Error("useWorkspace 必须在 WorkspaceProvider 内使用");
  return value;
}

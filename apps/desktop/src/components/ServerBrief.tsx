import { Globe, Terminal } from "lucide-react";
import type { ReactNode } from "react";
import type { ServerDef } from "../lib/ipc";
import { IconChip } from "./ui";

/**
 * 端点摘要：列表只显示有辨识度的那部分，完整值挂 title 悬停可见。
 * - http/sse：主机名（mcp.context7.com），而非整条带路径的 URL；
 * - stdio：命令 basename + 参数（node --xxx），而非 /Applications/…/node 绝对路径。
 */
export function endpointSummary(
  server: Pick<ServerDef, "transport" | "url" | "command" | "args">,
): { text: string; full: string } {
  if (server.transport !== "stdio") {
    const full = server.url ?? "";
    try {
      return { text: new URL(full).host, full };
    } catch {
      return { text: full, full };
    }
  }
  const full = `${server.command ?? ""} ${server.args.join(" ")}`.trim();
  const base = server.command?.split("/").pop() ?? "";
  const text = [base, ...server.args].filter(Boolean).join(" ");
  return { text: text || full, full };
}

/** 传输方式图标：远程蓝地球、本地青终端（服务器页、导入导出列表同一画法）。 */
export function ServerTransportChip({
  transport,
  size = 30,
}: {
  transport: ServerDef["transport"];
  size?: number;
}) {
  const isRemote = transport !== "stdio";
  return (
    <IconChip icon={isRemote ? Globe : Terminal} tone={isRemote ? "blue" : "teal"} size={size} />
  );
}

/** 导入预览 / 导出勾选列表里的一行身份：图标 + 名称（可跟徽标）+ 端点摘要。 */
export function ServerBrief({
  server,
  badges,
}: {
  server: Pick<ServerDef, "name" | "transport" | "url" | "command" | "args">;
  badges?: ReactNode;
}) {
  const endpoint = endpointSummary(server);
  return (
    <div className="flex min-w-0 flex-1 items-center gap-3">
      <ServerTransportChip transport={server.transport} size={26} />
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="truncate font-semibold text-[13px] text-ink">{server.name}</span>
          {badges}
        </div>
        {endpoint.text ? (
          <p className="truncate font-mono text-xs text-faint" title={endpoint.full || undefined}>
            {endpoint.text}
          </p>
        ) : null}
      </div>
    </div>
  );
}

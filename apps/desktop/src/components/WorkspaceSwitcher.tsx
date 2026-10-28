import { Boxes, Check, ChevronsUpDown, Settings2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "../lib/utils";
import { DEFAULT_WS, useWorkspace } from "../lib/workspace";

/**
 * 顶右工作区切换器：当前语境常驻可见，一键切换，末尾进入管理页。
 * 参考 MCP Router 的 `Local ▾`——把工作区从导航抽屉提升为全局上下文。
 */
export function WorkspaceSwitcher({ onManage }: { onManage: () => void }) {
  const { t } = useTranslation();
  const { selectedId, setSelectedId, workspaces } = useWorkspace();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onPointer = (event: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(event.target as Node)) setOpen(false);
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const items = [
    { id: DEFAULT_WS, name: t("workspace.default") },
    ...workspaces.filter((workspace) => workspace.id !== DEFAULT_WS),
  ];
  const currentName =
    items.find((workspace) => workspace.id === selectedId)?.name ?? t("workspace.default");

  return (
    <div ref={rootRef} className="relative">
      <button
        data-tour="workspace-switcher"
        type="button"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={t("workspace.switcher")}
        onClick={() => setOpen((value) => !value)}
        className="flex h-8 items-center gap-2 rounded-lg border border-line-strong bg-card pr-2 pl-2.5 text-[13px] font-medium text-ink transition-colors hover:bg-hover"
      >
        <Boxes size={15} strokeWidth={2} aria-hidden className="text-accent" />
        <span className="max-w-[168px] truncate">{currentName}</span>
        <ChevronsUpDown size={14} strokeWidth={2} aria-hidden className="text-faint" />
      </button>

      {open ? (
        <div
          role="menu"
          className="absolute right-0 z-50 mt-1.5 w-60 rounded-xl border border-line bg-card p-1.5 shadow-[var(--shadow-pop)]"
        >
          <p className="px-2.5 py-1.5 text-[11px] font-semibold tracking-wide text-faint uppercase">
            {t("workspace.switcher")}
          </p>
          <div className="max-h-64 space-y-0.5 overflow-y-auto">
            {items.map((workspace) => {
              const active = workspace.id === selectedId;
              return (
                <button
                  key={workspace.id}
                  type="button"
                  role="menuitemradio"
                  aria-checked={active}
                  onClick={() => {
                    setSelectedId(workspace.id);
                    setOpen(false);
                  }}
                  className={cn(
                    "flex w-full items-center justify-between gap-2 rounded-lg px-2.5 py-1.5 text-[13px] transition-colors",
                    active ? "bg-accent-soft text-ink" : "text-muted hover:bg-hover hover:text-ink",
                  )}
                >
                  <span className="truncate">{workspace.name}</span>
                  {active ? (
                    <Check
                      size={15}
                      strokeWidth={2.25}
                      aria-hidden
                      className="shrink-0 text-accent"
                    />
                  ) : null}
                </button>
              );
            })}
          </div>
          <div className="my-1 h-px bg-line" />
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              setOpen(false);
              onManage();
            }}
            className="flex w-full items-center gap-2 rounded-lg px-2.5 py-1.5 text-[13px] text-muted transition-colors hover:bg-hover hover:text-ink"
          >
            <Settings2 size={15} strokeWidth={2} aria-hidden className="text-faint" />
            {t("workspace.manage")}
          </button>
        </div>
      ) : null}
    </div>
  );
}

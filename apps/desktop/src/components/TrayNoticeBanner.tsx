import { listen } from "@tauri-apps/api/event";
import { TriangleAlert, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { IconButton } from "./ui";

/** 后端 `tray-action-failed` 的载荷（main.rs switch_provider_from_tray）。 */
type TrayNotice = { message: string };

/**
 * 托盘操作失败/警告的提醒条。托盘本身没有任何反馈通道——切换 Provider
 * 失败时用户只能靠「勾没变过来」察觉。窗口开着时经此横幅可见；
 * 窗口关着时 emit 无接收方，后端 eprintln 兜底（可接受：托盘操作
 * 通常发生在用户正对着屏幕时）。
 */
export function TrayNoticeBanner() {
  const { t } = useTranslation();
  const [messages, setMessages] = useState<string[]>([]);

  useEffect(() => {
    const unlisten = listen<TrayNotice>("tray-action-failed", (event) => {
      setMessages((prev) =>
        prev.includes(event.payload.message) ? prev : [...prev, event.payload.message],
      );
    }).catch(() => () => {});
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  if (messages.length === 0) return null;

  return (
    <div
      role="status"
      className="flex shrink-0 items-start gap-2.5 border-b border-line bg-warn-soft px-4 py-2.5"
    >
      <TriangleAlert aria-hidden className="mt-px h-4 w-4 shrink-0 text-warn" />
      <div className="min-w-0 flex-1">
        <p className="text-xs font-medium text-ink">{t("trayNotice.title")}</p>
        <ul className="mt-1 space-y-0.5">
          {messages.map((message) => (
            <li key={message} className="text-[11px] text-warn">
              {message}
            </li>
          ))}
        </ul>
      </div>
      <IconButton
        icon={X}
        onClick={() => setMessages([])}
        aria-label={t("trayNotice.dismiss")}
        title={t("trayNotice.dismiss")}
      />
    </div>
  );
}

import { listen } from "@tauri-apps/api/event";
import { ArrowDownToLine, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc, type UpdateStatus } from "../lib/ipc";
import { Button, IconButton } from "./ui";

/**
 * 启动静默检查发现新版本时的提醒条（事件由 main.rs setup 延迟发出）。
 * 下载安装与重启就地完成，不强迫用户先找到设置页；「忽略」只静默本次会话，
 * 下次启动仍会提醒——更新提醒的打扰频率以启动为界，够克制也不会永久错过。
 */
export function UpdateBanner() {
  const { t } = useTranslation();
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  const [phase, setPhase] = useState<"idle" | "installing" | "installed" | "failed">("idle");

  useEffect(() => {
    const unlisten = listen<UpdateStatus>("mcpane://update-available", (event) => {
      setStatus(event.payload);
    }).catch(() => () => {});
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  if (!status?.available) return null;

  const install = async () => {
    setPhase("installing");
    try {
      await ipc.updateInstall();
      setPhase("installed");
    } catch {
      setPhase("failed");
    }
  };

  return (
    <div
      role="status"
      className="flex shrink-0 items-center gap-2.5 border-b border-line bg-accent-soft px-4 py-2"
    >
      <ArrowDownToLine aria-hidden className="h-4 w-4 shrink-0 text-accent" />
      <p className="min-w-0 flex-1 truncate text-xs text-ink">
        {phase === "failed"
          ? t("update.installFailed")
          : t("update.bannerText", { version: status.version ?? "" })}
      </p>
      {phase === "installed" ? (
        <Button size="sm" variant="primary" onClick={() => ipc.updateRestart()}>
          {t("update.restart")}
        </Button>
      ) : (
        <Button size="sm" variant="primary" loading={phase === "installing"} onClick={install}>
          {t("update.install")}
        </Button>
      )}
      <IconButton
        icon={X}
        onClick={() => setStatus(null)}
        aria-label={t("update.dismiss")}
        title={t("update.dismiss")}
      />
    </div>
  );
}

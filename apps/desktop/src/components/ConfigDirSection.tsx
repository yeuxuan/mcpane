/**
 * 客户端配置目录覆盖（对齐 cc-switch 的 `*_override_dir`）。
 *
 * 默认位置写死时，把配置放在别处的人（多账号隔离、便携盘、公司规范目录）
 * 连"装没装"都探测不出来，更别说切换供应商——这不是锦上添花，是那类用户
 * 完全用不了。留空即回到默认位置。
 *
 * Claude Desktop 不在列：它的四个文件跨 normal/threep 两个目录，整体覆盖没有
 * 意义；给了却不生效比不给更糟（同 Hermes 协议下拉那次的教训）。
 */
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { FolderCog } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { CLIENTS } from "../lib/clients";
import { type ClientApp, ipc } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { BrandGlyph } from "./BrandIcon";
import { Card, Field, IconChip } from "./ui";

/** 支持覆盖的客户端（Desktop 除外，见模块注释）。 */
const OVERRIDABLE = CLIENTS.filter((meta) => meta.app !== "claude-desktop");

export function ConfigDirSection() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const dirs = useQuery({
    queryKey: queryKeys.configDirs,
    queryFn: ipc.providerConfigDirsGet,
    retry: false,
  });
  // 输入框是受控的本地草稿：失焦才提交，免得每敲一个字符就校验目录存在性。
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [error, setError] = useState<Record<string, string>>({});
  useEffect(() => {
    if (dirs.data) setDraft(dirs.data);
  }, [dirs.data]);

  const save = useMutation({
    mutationFn: ({ app, dir }: { app: ClientApp; dir: string }) =>
      ipc.providerConfigDirSet(app, dir),
    onSuccess: (_data, { app }) => {
      setError(({ [app]: _removed, ...rest }) => rest);
      void queryClient.invalidateQueries({ queryKey: queryKeys.configDirs });
      // 探测的痕迹判定会看这个目录，改完要重新检测，否则面板还显示旧结论。
      void queryClient.invalidateQueries({ queryKey: queryKeys.clientEnv });
      void queryClient.invalidateQueries({ queryKey: queryKeys.clientsInstalled });
    },
    onError: (err, { app }) => setError((e) => ({ ...e, [app]: errorText(err) })),
  });

  return (
    <Card className="mb-5 p-5">
      <div className="flex items-center gap-3">
        <IconChip icon={FolderCog} tone="slate" />
        <div className="flex-1">
          <p className="font-medium text-ink text-sm">{t("settings.configDir.title")}</p>
          <p className="text-muted text-xs">{t("settings.configDir.desc")}</p>
        </div>
      </div>

      <div className="mt-3 space-y-2">
        {OVERRIDABLE.map((meta) => (
          <div key={meta.app} className="flex flex-wrap items-center gap-3">
            <span
              aria-hidden
              className="flex h-7 w-7 shrink-0 items-center justify-center"
              style={{ color: meta.color }}
            >
              <BrandGlyph name={meta.app} size={17} />
            </span>
            <span className="w-28 shrink-0 text-[13px] text-ink">{meta.label}</span>
            <div className="min-w-0 flex-1">
              <Field
                className="w-full font-mono text-xs"
                placeholder={t("settings.configDir.placeholder")}
                aria-label={t("settings.configDir.fieldLabel", { client: meta.label })}
                value={draft[meta.app] ?? ""}
                onChange={(e) => setDraft((d) => ({ ...d, [meta.app]: e.target.value }))}
                onBlur={(e) => {
                  const next = e.target.value.trim();
                  if (next === (dirs.data?.[meta.app] ?? "")) return;
                  save.mutate({ app: meta.app, dir: next });
                }}
              />
              {error[meta.app] ? (
                <p className="mt-0.5 text-[11px] text-danger">{error[meta.app]}</p>
              ) : null}
            </div>
          </div>
        ))}
      </div>
    </Card>
  );
}

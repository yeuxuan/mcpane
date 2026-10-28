import { ArrowRight, ExternalLink } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc } from "../lib/ipc";
import {
  applyPreset,
  presetInputsSatisfied,
  RUNTIME_LABEL_KEY,
  SERVER_PRESETS,
  type ServerPreset,
} from "../lib/serverPresets";
import { cn } from "../lib/utils";
import { Button, Field, LabeledField, Tag } from "./ui";

export interface PresetFill {
  name: string;
  transport: "stdio" | "http" | "sse";
  command: string;
  args: string[];
  url: string;
  bearer: string;
  env: Record<string, string>;
  /** 远程服务器预填的请求头（仅「按地址查找」会给）。 */
  headers?: Record<string, string>;
}

/**
 * 官方 MCP Server 预设选择器。
 *
 * 只负责**填表**：选完把值交给上层塞进既有的手动表单，用户仍在那里审阅、
 * 测试连通、再保存。不另开一条创建路径——那会多出一套要各自维护的校验与
 * 错误处理，且用户在保存前看不到自己到底装了什么。
 */
export function ServerPresetPicker({ onPick }: { onPick: (fill: PresetFill) => void }) {
  const { t } = useTranslation();
  const [openId, setOpenId] = useState<string | null>(null);
  /** 展开项的输入值，按 preset.inputs 同序。 */
  const [values, setValues] = useState<string[]>([]);

  const select = (preset: ServerPreset) => {
    // 无参数的预设点一下就走，不必多一次「确认」。
    if (!preset.inputs?.length) {
      onPick(applyPreset(preset, []));
      return;
    }
    setOpenId(preset.id);
    setValues(preset.inputs.map((input) => input.defaultValue ?? ""));
  };

  return (
    <div className="space-y-2">
      <p className="text-xs text-muted">{t("serverPreset.intro")}</p>
      {SERVER_PRESETS.map((preset) => {
        const open = openId === preset.id;
        return (
          <div
            key={preset.id}
            className={cn(
              "rounded-lg border transition-colors",
              open ? "border-accent bg-card" : "border-line hover:border-line-strong",
            )}
          >
            <div className="flex items-center gap-3 p-3">
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <p className="truncate text-[13px] font-medium text-ink">{preset.name}</p>
                  <Tag>{t(RUNTIME_LABEL_KEY[preset.runtime])}</Tag>
                </div>
                <p className="mt-0.5 truncate text-xs text-muted">{t(preset.descriptionKey)}</p>
              </div>
              <button
                type="button"
                onClick={() => ipc.openExternal(preset.docsUrl).catch(() => {})}
                aria-label={t("serverPreset.docs", { name: preset.name })}
                title={t("serverPreset.docs", { name: preset.name })}
                className="shrink-0 rounded-md p-1.5 text-muted transition-colors hover:bg-hover hover:text-ink"
              >
                <ExternalLink aria-hidden className="h-3.5 w-3.5" />
              </button>
              {/* 每个按钮都得有各自的无障碍名：8 个都叫「使用」的话，
                  读屏用户在按钮列表里根本分不清点的是哪一个。 */}
              <Button
                size="sm"
                icon={ArrowRight}
                aria-label={t("serverPreset.useNamed", { name: preset.name })}
                onClick={() => select(preset)}
              >
                {t("serverPreset.use")}
              </Button>
            </div>

            {open && preset.inputs ? (
              <div className="space-y-3 border-t border-line px-3 pb-3 pt-3">
                {preset.inputs.map((input, index) => (
                  <LabeledField
                    key={input.labelKey}
                    label={t(input.labelKey)}
                    required={!input.optional}
                    hint={input.hintKey ? t(input.hintKey) : undefined}
                  >
                    <Field
                      value={values[index] ?? ""}
                      onChange={(event) => {
                        const next = [...values];
                        next[index] = event.target.value;
                        setValues(next);
                      }}
                      placeholder={
                        input.placeholderKey ? t(input.placeholderKey) : (input.placeholder ?? "")
                      }
                      spellCheck={false}
                      // Token 类输入不该在别人路过时被看光。
                      type={input.kind === "bearer" ? "password" : "text"}
                      className="w-full font-mono"
                    />
                  </LabeledField>
                ))}
                <Button
                  variant="primary"
                  size="sm"
                  aria-label={t("serverPreset.fillNamed", { name: preset.name })}
                  disabled={!presetInputsSatisfied(preset, values)}
                  onClick={() => onPick(applyPreset(preset, values))}
                >
                  {t("serverPreset.fill")}
                </Button>
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}

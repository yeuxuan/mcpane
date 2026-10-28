import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { DeclaredAnnotations } from "../lib/ipc";
import { Button, JsonView, Modal, Switch } from "./ui";

/** MCP 规范给各 hint 写明的默认值——首次声明时的起手式，不是我们的臆断。 */
const SPEC_DEFAULTS: DeclaredAnnotations = {
  readOnlyHint: false,
  destructiveHint: true,
  idempotentHint: false,
  openWorldHint: true,
};

const HINTS = [
  "readOnlyHint",
  "destructiveHint",
  "idempotentHint",
  "openWorldHint",
] as const satisfies readonly (keyof DeclaredAnnotations)[];

/**
 * 四条说明写成静态 `t("...")` 而不是 `t(\`annotations.hint.${hint}\`)`：
 * i18n.test 的"引用的 key 都已定义"守卫只认双引号字面量，拼出来的 key
 * 它一个都扫不到——打错字就只能等界面上赫然显示 key 本身时才发现。
 */
function useHintDescriptions(): Record<keyof DeclaredAnnotations, string> {
  const { t } = useTranslation();
  return {
    readOnlyHint: t("annotations.hint.readOnlyHint"),
    destructiveHint: t("annotations.hint.destructiveHint"),
    idempotentHint: t("annotations.hint.idempotentHint"),
    openWorldHint: t("annotations.hint.openWorldHint"),
  };
}

/**
 * 为下游工具声明 ToolAnnotations。
 *
 * 只在下游**没申报**注解时才有编辑意义：下游自己说了的，这里只读展示原文，
 * 不给改口（改口就是伪造下游，ADR-002 Gate 的红线；补空位才是这个口子的
 * 全部授权范围）。
 */
export function ToolAnnotationsModal({
  toolName,
  downstream,
  declared,
  saving,
  onSave,
  onClose,
}: {
  toolName: string;
  /** 下游申报的注解原文；null = 没申报。 */
  downstream: Record<string, unknown> | null;
  /** 用户已有的声明；null = 未声明。 */
  declared: DeclaredAnnotations | null;
  saving: boolean;
  onSave: (next: DeclaredAnnotations | null) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const hintDescriptions = useHintDescriptions();
  const [draft, setDraft] = useState<DeclaredAnnotations>(declared ?? SPEC_DEFAULTS);

  return (
    <Modal
      title={t("annotations.title", { tool: toolName })}
      onClose={onClose}
      footer={
        downstream ? (
          <Button variant="ghost" onClick={onClose}>
            {t("common.dismiss")}
          </Button>
        ) : (
          <>
            {/* 撤回只在真有声明时给：没声明过的行摆一个「撤回」按钮，
                按下去什么也不会发生，纯属让人以为自己漏看了什么。 */}
            {declared ? (
              <Button variant="danger" disabled={saving} onClick={() => onSave(null)}>
                {t("annotations.clear")}
              </Button>
            ) : null}
            <Button variant="ghost" onClick={onClose}>
              {t("ui.cancel")}
            </Button>
            <Button loading={saving} onClick={() => onSave(draft)}>
              {t("annotations.save")}
            </Button>
          </>
        )
      }
    >
      {downstream ? (
        <>
          <p className="text-sm text-muted">{t("annotations.downstreamOwns")}</p>
          <JsonView className="mt-3" code={JSON.stringify(downstream, null, 2)} />
        </>
      ) : (
        <>
          <p className="text-sm text-muted">{t("annotations.why")}</p>
          {/* 四变体探针实测：Codex 只认 readOnlyHint，补 title 之类不管用。
              据此告诉用户「勾一项就够」，省得他对着四个开关犯难——但同一句话
              里必须写明代价，否则就成了诱导乱勾。 */}
          <p className="mt-1.5 text-xs text-muted">{t("annotations.minimal")}</p>
          <p className="mt-1.5 text-xs text-faint">{t("annotations.attribution")}</p>
          <div className="mt-4 space-y-3">
            {HINTS.map((hint) => (
              // 用 div 不用 label：Switch 渲染的是 button[role=switch] 而非
              // input，label 关联不上它；可及名由 Switch 自己的 aria-label 提供
              // （与工作区页的开关同一写法）。
              <div key={hint} className="flex items-start justify-between gap-4">
                <span className="min-w-0">
                  <span className="block font-mono text-[13px] text-ink">{hint}</span>
                  <span className="mt-0.5 block text-xs text-faint">{hintDescriptions[hint]}</span>
                </span>
                <Switch
                  checked={draft[hint]}
                  label={hint}
                  onChange={(next) => setDraft((prev) => ({ ...prev, [hint]: next }))}
                />
              </div>
            ))}
          </div>
        </>
      )}
    </Modal>
  );
}

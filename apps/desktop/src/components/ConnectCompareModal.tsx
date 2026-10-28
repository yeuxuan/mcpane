import { CheckCircle2, UnfoldVertical } from "lucide-react";
import type { ReactNode } from "react";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { buildCompareRows, type CompareRow, foldCompareRows } from "../lib/compare";
import type { ConnectOutcome, ConnectPreview, CredentialMode } from "../lib/ipc";
import { cn } from "../lib/utils";
import { Button, CodeBlock, IconChip, Modal, Tag } from "./ui";

/** 单栏一行：变更行按侧着色，另一侧缺行时渲染空占位。 */
function CompareLine({
  cell,
  side,
}: {
  cell?: { text: string; changed: boolean };
  side: "left" | "right";
}) {
  if (!cell) return <div className="min-h-[18px] bg-app/60" />;
  return (
    <div
      className={cn(
        "min-h-[18px] whitespace-pre-wrap break-all px-2",
        cell.changed && (side === "left" ? "bg-danger-soft text-danger" : "bg-run-soft text-run"),
      )}
    >
      {cell.text || " "}
    </div>
  );
}

/** 凭据方式单选卡：标题 + 一句后果说明 + 该方式的状态细节。 */
function ModeCard({
  selected,
  disabled,
  title,
  badge,
  description,
  detail,
  onSelect,
}: {
  selected: boolean;
  disabled: boolean;
  title: string;
  badge?: ReactNode;
  description: string;
  detail?: ReactNode;
  onSelect: () => void;
}) {
  return (
    <label
      className={cn(
        "relative flex-1 cursor-pointer rounded-lg border p-3 transition-colors",
        selected ? "border-accent bg-accent-soft" : "border-line bg-card hover:border-line-strong",
        disabled && "cursor-default opacity-60",
      )}
    >
      <input
        type="radio"
        name="credential-mode"
        checked={selected}
        disabled={disabled}
        onChange={onSelect}
        className="absolute inset-0 cursor-pointer appearance-none rounded-lg disabled:cursor-default"
      />
      <span className="flex items-center gap-2">
        <span
          aria-hidden
          className={cn(
            "flex h-4 w-4 shrink-0 items-center justify-center rounded-full border",
            selected ? "border-accent" : "border-line-strong",
          )}
        >
          {selected ? <span className="h-2 w-2 rounded-full bg-accent" /> : null}
        </span>
        <span className="text-[13px] font-semibold text-ink">{title}</span>
        {badge}
      </span>
      <span className="mt-1 block pl-6 text-xs leading-5 text-muted">{description}</span>
      {detail ? <span className="mt-1.5 block pl-6">{detail}</span> : null}
    </label>
  );
}

/** 收尾指引行：shell 命令渲染为可复制代码块，其余按普通说明文案。 */
function NextStepLine({ line }: { line: string }) {
  if (/^(export |setx )/.test(line)) return <CodeBlock code={line} />;
  return <p className="text-[13px] leading-5 text-muted">{line}</p>;
}

/** 双栏行渲染（左右各一格，右侧带分隔线）。 */
function CompareRowView({ row }: { row: CompareRow }) {
  return (
    <div className="grid grid-cols-2">
      <CompareLine cell={row.left} side="left" />
      <div className="border-l border-line">
        <CompareLine cell={row.right} side="right" />
      </div>
    </div>
  );
}

/**
 * 折叠式对比视图：只展开变更块 ± 上下文，未变更区域收成「N 行未变更」条，
 * 长配置文件的变更打开即见，无需滚动寻找。
 */
function CompareDiff({ rows }: { rows: CompareRow[] }) {
  const { t } = useTranslation();
  const segments = useMemo(() => foldCompareRows(rows), [rows]);
  const [expanded, setExpanded] = useState<ReadonlySet<number>>(new Set());

  return (
    <>
      {segments.map((segment) =>
        segment.kind === "fold" && !expanded.has(segment.start) ? (
          <button
            key={`fold-${segment.start}`}
            type="button"
            onClick={() => setExpanded(new Set([...expanded, segment.start]))}
            className="flex w-full items-center justify-center gap-1.5 border-y border-line border-dashed bg-app py-1 text-[11px] text-faint transition-colors hover:bg-hover hover:text-ink"
          >
            <UnfoldVertical size={12} strokeWidth={2} aria-hidden />
            {t("connectCompare.expandUnchanged", { count: segment.rows.length })}
          </button>
        ) : (
          segment.rows.map((row, offset) => (
            // biome-ignore lint/suspicious/noArrayIndexKey: diff 行静态只读、不增删重排，全局行号（start+offset）即身份。
            <CompareRowView key={segment.start + offset} row={row} />
          ))
        ),
      )}
    </>
  );
}

/**
 * 一键接入对比弹窗：顶部选凭据方式（env 默认 / inline 直写，两种都零手动
 * 步骤，ADR-016），中部左右对比「当前配置 / 写入后」，确认写入后就地切换
 * 为结果视图。
 */
export function ConnectCompareModal({
  title,
  preview,
  outcome,
  error,
  applying,
  reloadingPreview,
  envSupported,
  onApply,
  onSwitchMode,
  onClose,
}: {
  title: string;
  preview: ConnectPreview;
  /** 写入成功后的结果；非空时展示结果视图。 */
  outcome: ConnectOutcome | null;
  error: string | null;
  applying: boolean;
  /** 切换凭据模式后重新拉取预览中。 */
  reloadingPreview: boolean;
  /** false = 该客户端不支持环境变量展开（Cline），只有直写模式。 */
  envSupported: boolean;
  onApply: () => void;
  onSwitchMode: (mode: CredentialMode) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const rows = useMemo(
    () => buildCompareRows(preview.beforeText, preview.afterText),
    [preview.beforeText, preview.afterText],
  );
  const inline = preview.credentialMode === "inline";
  const noop = rows.every((row) => !row.left?.changed && !row.right?.changed);

  if (outcome) {
    return (
      <Modal
        title={t("connectCompare.doneTitle", { title })}
        onClose={onClose}
        footer={
          <Button variant="primary" onClick={onClose}>
            {t("connectCompare.done")}
          </Button>
        }
      >
        <div className="flex items-start gap-3">
          <IconChip icon={CheckCircle2} tone="green" />
          <div className="min-w-0">
            <p className="text-sm font-semibold text-ink">{t("connectCompare.written")}</p>
            <code className="mt-0.5 block break-all font-mono text-xs text-muted">
              {outcome.targetPath}
            </code>
          </div>
        </div>

        <div className="mt-3 flex flex-wrap items-center gap-x-2 gap-y-1.5 text-xs text-faint">
          {/* no-op（配置本就最新）不产生快照，snapshotId 为空串 */}
          {outcome.snapshotId ? (
            <Tag>{t("connectCompare.snapshot", { id: outcome.snapshotId })}</Tag>
          ) : null}
          {outcome.tokenPrefix ? (
            <Tag>{t("connectCompare.token", { prefix: outcome.tokenPrefix })}</Tag>
          ) : null}
          <span>{t("connectCompare.rollbackHint")}</span>
        </div>

        <h3 className="mt-5 text-xs font-semibold text-muted">{t("connectCompare.nextSteps")}</h3>
        <div className="mt-2 space-y-2">
          {outcome.nextSteps.map((line) => (
            <NextStepLine key={line} line={line} />
          ))}
        </div>
      </Modal>
    );
  }

  return (
    <Modal
      title={t("connectCompare.previewTitle", { title })}
      size="xl"
      onClose={onClose}
      footer={
        <>
          {error ? <p className="mr-auto text-xs text-danger">{error}</p> : null}
          <Button onClick={onClose}>{t("connectCompare.cancel")}</Button>
          <Button
            variant="primary"
            disabled={applying || reloadingPreview || noop}
            onClick={onApply}
          >
            {applying ? t("connectCompare.applying") : t("connectCompare.apply")}
          </Button>
        </>
      }
    >
      {envSupported ? (
        <>
          <p className="mb-2 text-xs text-muted">{t("connectCompare.modeIntro")}</p>
          <div
            role="radiogroup"
            aria-label={t("connectCompare.modeGroup")}
            className="flex flex-col gap-2 sm:flex-row"
          >
            <ModeCard
              selected={!inline}
              disabled={reloadingPreview}
              title={t("connectCompare.mode.env.title")}
              badge={<Tag>{t("connectCompare.mode.env.badge")}</Tag>}
              description={t("connectCompare.mode.env.desc")}
              detail={
                !inline && preview.envVar ? (
                  <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
                    <code className="font-mono text-ink">{preview.envVar}</code>
                    <Tag tone="green">{t("connectCompare.mode.env.autoSet")}</Tag>
                  </span>
                ) : undefined
              }
              onSelect={() => {
                if (inline) onSwitchMode("env");
              }}
            />
            <ModeCard
              selected={inline}
              disabled={reloadingPreview}
              title={t("connectCompare.mode.inline.title")}
              badge={<Tag>{t("connectCompare.mode.inline.badge")}</Tag>}
              description={t("connectCompare.mode.inline.desc")}
              onSelect={() => {
                if (!inline) onSwitchMode("inline");
              }}
            />
          </div>
        </>
      ) : (
        <p className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
          <Tag>{t("connectCompare.inlineOnly.badge")}</Tag>
          {t("connectCompare.inlineOnly.desc", { title })}
        </p>
      )}

      <p className="mt-4 mb-2 flex flex-wrap items-center gap-1.5 text-xs text-muted">
        {t("connectCompare.target")}
        <code className="font-mono text-ink">{preview.targetPath}</code>
        <span>{t("connectCompare.targetHint")}</span>
      </p>

      <div className="relative overflow-hidden rounded-lg border border-line">
        <div className="grid grid-cols-2 border-b border-line bg-app text-xs font-medium">
          <p className="px-3 py-2 text-muted">{t("connectCompare.beforeLabel")}</p>
          <p className="border-l border-line px-3 py-2 text-muted">
            {t("connectCompare.afterLabel")}
          </p>
        </div>
        <div className="max-h-[42vh] overflow-y-auto font-mono text-[11px] leading-[18px] text-ink">
          {noop ? (
            <p className="p-4 text-center text-xs text-muted">{t("connectCompare.noChanges")}</p>
          ) : (
            <CompareDiff key={`${preview.credentialMode}:${preview.afterText}`} rows={rows} />
          )}
        </div>
        {reloadingPreview ? (
          <div className="absolute inset-0 flex items-center justify-center bg-card/70 text-xs font-medium text-muted">
            {t("connectCompare.refreshing")}
          </div>
        ) : null}
      </div>
    </Modal>
  );
}

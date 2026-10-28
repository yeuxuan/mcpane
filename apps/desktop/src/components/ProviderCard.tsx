import type { DraggableAttributes, DraggableSyntheticListeners } from "@dnd-kit/core";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import {
  Activity,
  Check,
  Copy,
  Gauge,
  GripVertical,
  Loader2,
  Pencil,
  Play,
  Trash2,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { type ClientApp, ipc, type ProviderProfile } from "../lib/ipc";
import { useProviderQuota } from "../lib/quota";
import { cn } from "../lib/utils";
import { ProviderIcon } from "./ProviderIcon";
import { QuotaSummary } from "./QuotaSummary";
import { Button, Card, IconButton, Tag } from "./ui";

interface ProviderCardProps {
  /** 导览锚点。只有列表首张卡会拿到，用来指认"卡片上能做什么"。 */
  "data-tour"?: string;
  provider: ProviderProfile;
  app: ClientApp;
  isCurrent: boolean;
  isSwitching: boolean;
  switchPending: boolean;
  isTesting: boolean;
  warnings: string[];
  testResult?: string;
  /** 统一供应商的影子条目：编辑路由到统一供应商弹窗，复制/删除入口隐藏。 */
  unifiedManaged?: boolean;
  onSwitch: () => void;
  /** additive 客户端（OpenCode）：配置里多个供应商并存，没有"当前生效项"。 */
  isAdditive?: boolean;
  /** additive：该供应商此刻是否写在客户端配置里。 */
  isEnabled?: boolean;
  /** additive：从配置移除（保留列表记录）。 */
  onRemoveFromLive?: () => void;
  onTest: () => void;
  onEdit: () => void;
  onDuplicate: () => void;
  onDelete: () => void;
  dragHandle?: {
    attributes: DraggableAttributes;
    listeners: DraggableSyntheticListeners;
    isDragging: boolean;
  };
}

/** 中转站卡片外包一层 useSortable，把拖拽手柄传给通用卡片。 */
export function SortableRelayCard(props: Omit<ProviderCardProps, "dragHandle">) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: props.provider.id,
  });
  return (
    <div
      ref={setNodeRef}
      style={{ transform: CSS.Transform.toString(transform), transition }}
      className={cn(isDragging && "relative z-10")}
    >
      <ProviderCard {...props} dragHandle={{ attributes, listeners, isDragging }} />
    </div>
  );
}

export function ProviderCard({
  "data-tour": dataTour,
  provider,
  app,
  isCurrent,
  isSwitching,
  switchPending,
  isTesting,
  warnings,
  testResult,
  unifiedManaged,
  onSwitch,
  isAdditive,
  isEnabled,
  onRemoveFromLive,
  onTest,
  onEdit,
  onDuplicate,
  onDelete,
  dragHandle,
}: ProviderCardProps) {
  const { t } = useTranslation();
  const isOfficial = provider.category === "official";
  // additive 客户端没有"当前生效"，高亮跟随"是否已写进配置"。
  const highlighted = isAdditive ? Boolean(isEnabled) : isCurrent;
  // 只有生效项自动刷新额度，其余卡片等用户点「用量」按钮。
  const quota = useProviderQuota(provider, { auto: isCurrent });
  const quotaButton = quota.kind ? (
    <IconButton
      icon={quota.query.isFetching ? Loader2 : Gauge}
      title={t("providers.quota")}
      aria-label={t("providers.quota")}
      disabled={quota.query.isFetching}
      className={quota.query.isFetching ? "[&>svg]:animate-spin" : undefined}
      onClick={quota.refresh}
    />
  ) : null;

  return (
    <Card
      data-tour={dataTour}
      interactive
      className={cn(
        "group relative overflow-hidden p-4 transition-all duration-200",
        highlighted && "border-accent/60",
        // 拖拽中轻微放大 + 高阴影，给"拎起来"的触感（对齐 cc-switch scale-105）
        dragHandle?.isDragging && "scale-[1.02] border-accent shadow-[var(--shadow-pop)]",
      )}
    >
      {/* 当前生效项高亮：L→R 淡出的主色渐变叠层（对齐同类工具的视觉语言，opacity 淡入切换） */}
      <div
        aria-hidden
        className={cn(
          "pointer-events-none absolute inset-0 bg-gradient-to-r from-accent/10 to-transparent transition-opacity duration-500",
          highlighted ? "opacity-100" : "opacity-0",
        )}
      />
      <div className="relative flex items-center gap-3">
        {dragHandle ? (
          <button
            type="button"
            aria-label={t("providers.dragHandle")}
            className={cn(
              "-ml-1.5 shrink-0 cursor-grab p-1 text-faint transition-colors hover:text-muted active:cursor-grabbing",
              dragHandle.isDragging && "cursor-grabbing",
            )}
            {...dragHandle.attributes}
            {...dragHandle.listeners}
          >
            <GripVertical size={15} aria-hidden />
          </button>
        ) : null}

        <span
          className="flex h-9 w-9 shrink-0 items-center justify-center rounded-[9px] border border-line bg-app transition-transform duration-300 group-hover:scale-105"
          aria-hidden
        >
          <ProviderIcon
            name={provider.name}
            icon={
              isOfficial
                ? app === "claude" || app === "claude-desktop"
                  ? "anthropic"
                  : app === "gemini"
                    ? "gemini"
                    : "openai"
                : provider.icon
            }
            size={20}
          />
        </span>

        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-semibold text-ink">{provider.name}</span>
            {/* 生效态要有一眼能扫到的徽标：渐变叠层太淡，按钮文字又在最右边。 */}
            {highlighted ? <Tag tone="green">{t("providers.inUseBadge")}</Tag> : null}
            {unifiedManaged ? <Tag tone="violet">{t("providers.unified.badge")}</Tag> : null}
            {isOfficial ? (
              <Tag>{t("providers.category.official")}</Tag>
            ) : provider.apiKeyRef ? (
              <Tag tone="green">{t("providers.keyStored")}</Tag>
            ) : (
              <Tag tone="amber">{t("providers.keyMissing")}</Tag>
            )}
          </div>
          {isOfficial ? (
            <p className="mt-1 text-xs text-muted">{t(`providers.officialDesc.${app}`)}</p>
          ) : (
            // 第二行对齐 cc-switch：备注优先展示，点击打开官网（缺省退到 base_url）。
            <button
              type="button"
              title={provider.websiteUrl ?? provider.baseUrl ?? undefined}
              onClick={() => {
                const url = provider.websiteUrl ?? provider.baseUrl;
                if (url) void ipc.openExternal(url);
              }}
              className={cn(
                // 网址是次要信息：平时灰，悬停才亮主色——整列蓝线会把交互色用成装饰。
                "mt-1 block max-w-full truncate text-left text-xs text-muted hover:text-accent hover:underline",
                !provider.notes && "font-mono",
              )}
            >
              {provider.notes ??
                `${provider.baseUrl ?? ""}${provider.model ? ` · ${provider.model}` : ""}`}
            </button>
          )}
          {warnings.map((warning) => (
            <p key={warning} className="mt-1 text-xs text-warn">
              {warning}
            </p>
          ))}
          {testResult ? <p className="mt-1 text-xs text-muted">{testResult}</p> : null}
          {quota.kind ? <QuotaSummary query={quota.query} className="mt-1" /> : null}
        </div>

        <div
          className={cn(
            "flex shrink-0 items-center gap-1.5 transition-opacity duration-200",
            // 对齐 cc-switch：操作区 hover/键盘聚焦时显现，当前项常显主按钮状态
            !highlighted &&
              "pointer-events-none opacity-0 group-focus-within:pointer-events-auto group-focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100",
          )}
        >
          {isOfficial ? (
            // 官方订阅同样可编辑（配置片段）、可删除——它是落库的普通记录，
            // 删了不会在下次启动复活，需要时再从「添加供应商」里加回来。
            <>
              {quotaButton}
              <IconButton
                icon={Pencil}
                title={t("providers.edit", { name: provider.name })}
                aria-label={t("providers.edit", { name: provider.name })}
                onClick={onEdit}
              />
              <IconButton
                icon={Trash2}
                tone="danger"
                title={
                  isCurrent
                    ? t("providers.deleteInUseHint")
                    : t("providers.delete", { name: provider.name })
                }
                aria-label={t("providers.delete", { name: provider.name })}
                disabled={isCurrent}
                className={isCurrent ? "cursor-not-allowed opacity-40" : undefined}
                onClick={onDelete}
              />
            </>
          ) : null}
          {!isOfficial ? (
            <>
              {quotaButton}
              <IconButton
                icon={isTesting ? Loader2 : Activity}
                title={t("providers.test")}
                aria-label={t("providers.test")}
                disabled={isTesting}
                className={isTesting ? "[&>svg]:animate-spin" : undefined}
                onClick={onTest}
              />
              <IconButton
                icon={Pencil}
                title={t("providers.edit", { name: provider.name })}
                aria-label={t("providers.edit", { name: provider.name })}
                onClick={onEdit}
              />
              {!unifiedManaged ? (
                <>
                  <IconButton
                    icon={Copy}
                    title={t("providers.duplicate", { name: provider.name })}
                    aria-label={t("providers.duplicate", { name: provider.name })}
                    onClick={onDuplicate}
                  />
                  <IconButton
                    icon={Trash2}
                    tone="danger"
                    title={
                      isCurrent
                        ? t("providers.deleteInUseHint")
                        : t("providers.delete", { name: provider.name })
                    }
                    aria-label={t("providers.delete", { name: provider.name })}
                    disabled={isCurrent}
                    className={isCurrent ? "cursor-not-allowed opacity-40" : undefined}
                    onClick={onDelete}
                  />
                </>
              ) : null}
            </>
          ) : null}
          {isAdditive ? (
            // additive：主按钮是开关——已启用时点它就是"从配置移除"，
            // 不像切换式客户端那样只能靠切到别家来替换。
            <Button
              size="sm"
              variant={isEnabled ? "secondary" : "primary"}
              icon={isEnabled ? Check : Play}
              disabled={switchPending}
              className="min-w-[5.25rem] justify-center"
              title={isEnabled ? t("providers.additive.removeHint") : undefined}
              onClick={isEnabled ? onRemoveFromLive : onSwitch}
            >
              {isSwitching
                ? t("providers.switching")
                : isEnabled
                  ? t("providers.additive.enabled")
                  : t("providers.additive.enable")}
            </Button>
          ) : (
            <Button
              size="sm"
              variant={isCurrent ? "secondary" : "primary"}
              icon={isCurrent ? Check : Play}
              disabled={isCurrent || switchPending}
              className="min-w-[5.25rem] justify-center"
              onClick={onSwitch}
            >
              {isSwitching
                ? t("providers.switching")
                : isCurrent
                  ? t("providers.inUse")
                  : t("providers.switch")}
            </Button>
          )}
        </div>
      </div>
    </Card>
  );
}

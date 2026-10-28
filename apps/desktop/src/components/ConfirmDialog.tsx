import { AlertTriangle } from "lucide-react";
import { useTranslation } from "react-i18next";
import { Button, Modal } from "./ui";

/**
 * 危险操作确认弹窗（对齐 cc-switch 的 ConfirmDialog，套用本项目 Modal/Button）。
 * 用于删除等不可逆操作，替代直接执行。
 */
export function ConfirmDialog({
  title,
  message,
  confirmText,
  cancelText,
  onConfirm,
  onCancel,
}: {
  title: string;
  message: string;
  /** 确认按钮文案；缺省用「删除」语义由调用方传入。 */
  confirmText: string;
  /** 取消按钮文案；缺省「取消」。放弃改动这类场景传「继续编辑」更清楚。 */
  cancelText?: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  return (
    <Modal
      title={title}
      onClose={onCancel}
      footer={
        <>
          <Button size="sm" onClick={onCancel}>
            {cancelText ?? t("confirm.cancel")}
          </Button>
          <Button size="sm" variant="danger" onClick={onConfirm}>
            {confirmText}
          </Button>
        </>
      }
    >
      <div className="flex items-start gap-3">
        <span className="mt-0.5 flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-danger-soft text-danger">
          <AlertTriangle size={16} aria-hidden />
        </span>
        <p className="text-sm leading-relaxed break-words whitespace-pre-line text-muted">
          {message}
        </p>
      </div>
    </Modal>
  );
}

/**
 * 未保存改动守卫：大表单（接入服务器 / 供应商编辑）点遮罩 / ESC / 返回时，
 * 如果用户已经改过内容，先弹一道 ConfirmDialog，别让一次误触把半张表单丢掉。
 *
 * 脏检测走容器级 `onChangeCapture` / `onInputCapture`——React 的 change 事件冒泡，
 * 一处就能接住所有 input/textarea/select 的输入。按钮驱动的状态变化（加一行请求头、
 * 选一个预设）不算数：那类改动少、且用户通常记得，为它逐字段接线不值得。
 */
import { useCallback, useRef, useState } from "react";

export interface UnsavedGuard {
  /** 表单里接 `onChangeCapture` / `onInputCapture`：用户改过内容就置真。 */
  markDirty: () => void;
  /** 确认框是否打开。 */
  confirmOpen: boolean;
  /** 所有关闭入口（X / 遮罩 / ESC / 返回按钮）都走这个：脏则弹确认，否则直接关。 */
  requestClose: () => void;
  /** 确认框「放弃改动」。 */
  confirmDiscard: () => void;
  /** 确认框「继续编辑」。 */
  cancelDiscard: () => void;
}

export function useUnsavedGuard(onClose: () => void): UnsavedGuard {
  // onClose 常是内联闭包，走 ref 以免每次渲染换掉下面几个回调。
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const dirtyRef = useRef(false);
  const [confirmOpen, setConfirmOpen] = useState(false);

  const markDirty = useCallback(() => {
    dirtyRef.current = true;
  }, []);
  const requestClose = useCallback(() => {
    if (dirtyRef.current) setConfirmOpen(true);
    else onCloseRef.current();
  }, []);
  const confirmDiscard = useCallback(() => {
    setConfirmOpen(false);
    onCloseRef.current();
  }, []);
  const cancelDiscard = useCallback(() => setConfirmOpen(false), []);

  return { markDirty, confirmOpen, requestClose, confirmDiscard, cancelDiscard };
}

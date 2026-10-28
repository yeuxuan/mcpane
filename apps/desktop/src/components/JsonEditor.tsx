import { lazy, Suspense } from "react";
import { useTranslation } from "react-i18next";
import type { JsonEditorProps } from "./JsonEditorView";

/**
 * 配置编辑器的按需加载边界。
 *
 * 实现在 [[JsonEditorView]]，它静态引入 codemirror / lezer 一族（压缩后仍有数百 KB），
 * 而这四处调用都藏在弹窗里——多数会话一次都不会打开。边界放在这一层而不是各调用点：
 * 调用方照常 `import { JsonEditor }`，也就没人会不小心把静态引用加回来。
 */
const JsonEditorView = lazy(() =>
  import("./JsonEditorView").then((m) => ({ default: m.JsonEditorView })),
);

/** 与 JsonEditorView 的同名默认值一致：占位框先撑到编辑器的高度，加载完不跳版。 */
const DEFAULT_MIN_HEIGHT = 144;

export function JsonEditor(props: JsonEditorProps) {
  const { t } = useTranslation();
  return (
    <Suspense
      fallback={
        <div
          className="flex items-center justify-center rounded-lg bg-slot text-sm text-muted"
          style={{ minHeight: props.minHeight ?? DEFAULT_MIN_HEIGHT }}
        >
          {t("common.loading")}
        </div>
      }
    >
      <JsonEditorView {...props} />
    </Suspense>
  );
}

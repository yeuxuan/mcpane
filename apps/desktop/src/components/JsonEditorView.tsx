import { json } from "@codemirror/lang-json";
import { HighlightStyle, StreamLanguage, syntaxHighlighting } from "@codemirror/language";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { type Diagnostic, linter } from "@codemirror/lint";
import { EditorSelection, EditorState, StateEffect, StateField } from "@codemirror/state";
import { placeholder as cmPlaceholder, Decoration, type DecorationSet } from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { basicSetup, EditorView } from "codemirror";
import { Wand2 } from "lucide-react";
import { useEffect, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { parse as parseToml, stringify as stringifyToml } from "smol-toml";
import { cn } from "../lib/utils";

type EditorLanguage = "json" | "toml";

export interface JsonEditorProps {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  /** 语法与校验模式：Claude=json（默认），Codex=toml（对齐 config.toml 原生格式）。 */
  language?: EditorLanguage;
  /** 编辑器最小高度（px），内容超出后内部滚动。 */
  minHeight?: number;
  /** 编辑器最大高度（px）：live 配置可能很长，超出后编辑器内部滚动而非撑爆弹窗。 */
  maxHeight?: number;
  /** 外部校验失败时描红边框（与内置 lint 波浪线互补）。 */
  hasError?: boolean;
  disabled?: boolean;
  /** 无障碍名称：CodeMirror 的 contenteditable 是 role=textbox，必须有 accessible name。
   * 缺省用 jsonEditor.ariaLabel 的本地化文案。 */
  ariaLabel?: string;
}

/** 按语言解析文本；非法时抛错（linter/格式化共用）。 */
function parseByLanguage(text: string, language: EditorLanguage): unknown {
  return language === "toml" ? parseToml(text) : JSON.parse(text);
}

/**
 * 新旧文本的最小差异区间（公共前后缀收缩）。
 * 外部写回（快捷开关等）据此只替换变化区间而非整份文档——
 * 整文档替换会掀翻视口与光标，改动落在视野外时用户看不到任何变化。
 */
export function minimalDiff(
  oldText: string,
  newText: string,
): { from: number; to: number; insert: string } | null {
  if (oldText === newText) return null;
  let start = 0;
  const minLen = Math.min(oldText.length, newText.length);
  while (start < minLen && oldText[start] === newText[start]) start++;
  let endOld = oldText.length;
  let endNew = newText.length;
  while (endOld > start && endNew > start && oldText[endOld - 1] === newText[endNew - 1]) {
    endOld--;
    endNew--;
  }
  return { from: start, to: endOld, insert: newText.slice(start, endNew) };
}

/** 外部写回的落点闪烁：置为区间显示高亮，置 null 清除。 */
const setExternalFlash = StateEffect.define<{ from: number; to: number } | null>({
  map: (value, mapping) =>
    value ? { from: mapping.mapPos(value.from), to: mapping.mapPos(value.to) } : value,
});

const externalFlashLine = Decoration.line({ class: "cm-external-flash" });

/** 高亮按行渲染（改动可能是纯删除——零宽区间也要有落点可见）。 */
const externalFlashField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    let next = deco.map(tr.changes);
    for (const effect of tr.effects) {
      if (effect.is(setExternalFlash)) {
        if (!effect.value) return Decoration.none;
        const doc = tr.state.doc;
        const first = doc.lineAt(Math.min(effect.value.from, doc.length)).number;
        const last = doc.lineAt(Math.min(effect.value.to, doc.length)).number;
        const lines = [];
        for (let n = first; n <= last; n++) {
          lines.push(externalFlashLine.range(doc.line(n).from));
        }
        next = Decoration.set(lines);
      }
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

/**
 * 语法高亮：必须自定义，不能吃 basicSetup 自带的 defaultHighlightStyle——
 * 后者的颜色是为浅底硬编码的（string `#a11`、number `#219`），在暗底上对比度
 * 只有 1.3~2.0，字符串和数字近乎隐形。
 *
 * 色值走 CSS 变量（见 index.css 的 --cm-*），一份定义自动跟随明暗，
 * 无需为暗色再建一套 HighlightStyle。basicSetup 的默认样式声明为
 * `fallback: true`，本样式存在时即被让位。
 */
const syntaxColors = HighlightStyle.define([
  { tag: [tags.propertyName, tags.definition(tags.propertyName)], color: "var(--cm-property)" },
  { tag: [tags.string, tags.special(tags.string)], color: "var(--cm-string)" },
  { tag: [tags.number, tags.integer, tags.float], color: "var(--cm-number)" },
  { tag: [tags.bool, tags.atom, tags.null, tags.keyword], color: "var(--cm-keyword)" },
  { tag: [tags.comment, tags.lineComment, tags.blockComment], color: "var(--cm-comment)" },
  { tag: [tags.punctuation, tags.separator, tags.bracket], color: "var(--cm-punct)" },
  { tag: tags.invalid, color: "var(--color-danger)" },
]);

/**
 * CodeMirror 6 配置编辑器（对齐 cc-switch JsonEditor.tsx）：
 * 语法高亮 + 行号 + 实时 lint + 一键格式化，支持 JSON 与 TOML 两种模式。
 * 主题与语法色全走 MCPane 设计系统 CSS 变量（随明暗切换），不引入 oneDark。
 *
 * 调用方一律用 [[JsonEditor]]（同目录 JsonEditor.tsx）——这份实现连同 codemirror
 * 一族只在真正打开编辑器时才下载，静态引入它等于把那几百 KB 拽回首屏 chunk。
 */
export function JsonEditorView({
  value,
  onChange,
  placeholder = "",
  language = "json",
  minHeight = 144,
  maxHeight = 480,
  hasError = false,
  disabled = false,
  ariaLabel,
}: JsonEditorProps) {
  const { t } = useTranslation();
  const resolvedAriaLabel = ariaLabel ?? t("jsonEditor.ariaLabel");
  const hostRef = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  // onChange/value 走 ref，避免 updateListener 闭包捕获陈旧回调，也避免因其变化重建编辑器。
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;
  const valueRef = useRef(value);
  valueRef.current = value;

  const configLinter = useMemo(
    () =>
      linter((view) => {
        const diagnostics: Diagnostic[] = [];
        const doc = view.state.doc.toString();
        if (!doc.trim()) return diagnostics;
        try {
          const parsed = parseByLanguage(doc, language);
          if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
            diagnostics.push({
              from: 0,
              to: doc.length,
              severity: "error",
              message: t("providers.modal.jsonMustBeObject"),
            });
          }
        } catch (e) {
          diagnostics.push({
            from: 0,
            to: doc.length,
            severity: "error",
            message:
              e instanceof Error && e.message
                ? e.message
                : t(
                    language === "toml"
                      ? "providers.modal.tomlInvalid"
                      : "providers.modal.jsonInvalid",
                  ),
          });
        }
        return diagnostics;
      }),
    [t, language],
  );

  useEffect(() => {
    if (!hostRef.current) return;

    const theme = EditorView.theme({
      "&": {
        minHeight: `${minHeight}px`,
        maxHeight: `${maxHeight}px`,
        // 与输入框同一个凹槽色：编辑器也是"白纸上可以写字的地方"，用窗口底色
        // 会比输入框浅一档，同一张纸上出现两种深浅的槽。
        backgroundColor: "var(--color-slot)",
        borderRadius: "0.5rem",
        fontSize: "12px",
      },
      "&.cm-focused": { outline: "none" },
      ".cm-scroller": {
        fontFamily: "var(--font-mono)",
        lineHeight: "1.6",
        overflow: "auto",
        borderRadius: "0.5rem",
      },
      // 快捷开关等外部写回的落点行：短暂高亮，让改动即刻可见。
      ".cm-external-flash": {
        backgroundColor: "color-mix(in srgb, var(--color-accent) 14%, transparent)",
      },
      // outline: none 同时压掉全局 :focus-visible 描边（聚焦态由外层 focus-within 边框表达）。
      ".cm-content": { padding: "8px 0", caretColor: "var(--color-ink)", outline: "none" },
      ".cm-line": { padding: "0 12px" },
      ".cm-gutters": {
        backgroundColor: "transparent",
        borderRight: "1px solid var(--color-line)",
        color: "var(--color-faint)",
        borderRadius: "0.5rem 0 0 0.5rem",
      },
      ".cm-activeLine": {
        backgroundColor: "color-mix(in srgb, var(--color-accent) 5%, transparent)",
      },
      ".cm-activeLineGutter": {
        backgroundColor: "color-mix(in srgb, var(--color-accent) 5%, transparent)",
      },
      ".cm-selectionBackground, .cm-content ::selection": {
        backgroundColor: "color-mix(in srgb, var(--color-accent) 18%, transparent) !important",
      },
      ".cm-selectionMatch": {
        backgroundColor: "color-mix(in srgb, var(--color-accent) 12%, transparent)",
      },
      ".cm-cursor": { borderLeftColor: "var(--color-ink)" },
      /* muted 而不是 faint：faint(#6e6e78) 落在编辑器凹槽底上只有 4.16:1，不达 AA
       * （在旧的 app 底色上是 4.30，同样不达——只是这一页此前没被 axe 扫过）。 */
      ".cm-placeholder": { color: "var(--color-muted)" },
      ".cm-tooltip": {
        backgroundColor: "var(--color-card)",
        border: "1px solid var(--color-line-strong)",
        borderRadius: "8px",
        boxShadow: "var(--shadow-pop)",
        color: "var(--color-ink)",
      },
    });

    const state = EditorState.create({
      doc: valueRef.current,
      extensions: [
        basicSetup,
        syntaxHighlighting(syntaxColors),
        language === "toml" ? StreamLanguage.define(toml) : json(),
        cmPlaceholder(placeholder),
        // 显式 tabindex：只读时 contenteditable=false，内容区不再可聚焦，超宽的长行
        // 用键盘滚不到（axe scrollable-region-focusable）；可编辑时与浏览器默认一致。
        EditorView.contentAttributes.of({ "aria-label": resolvedAriaLabel, tabindex: "0" }),
        theme,
        configLinter,
        externalFlashField,
        EditorState.readOnly.of(disabled),
        EditorView.editable.of(!disabled),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) onChangeRef.current(update.state.doc.toString());
        }),
      ],
    });

    const view = new EditorView({ state, parent: hostRef.current });
    viewRef.current = view;
    return () => {
      view.destroy();
      viewRef.current = null;
    };
    // value 的外部同步由下一个 effect 增量处理，不重建编辑器。
  }, [minHeight, maxHeight, placeholder, disabled, configLinter, language, resolvedAriaLabel]);

  // 外部 value 变化（如异步基线加载、快捷开关写回）时同步进编辑器：
  // 只替换最小差异区间（保住视口/光标/折叠），并把落点滚进视野短暂高亮——
  // live 配置很长时改动常落在视野外，整文档替换会让用户以为「点了没反应」。
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    const view = viewRef.current;
    if (!view) return;
    const oldText = view.state.doc.toString();
    const diff = minimalDiff(oldText, value);
    if (!diff) return;
    // 整文档替换（初始基线加载、语言切换重建等）不做落点提示——那不是「一处改动」。
    const isFullReplace = diff.from === 0 && diff.to === oldText.length;
    const insertEnd = diff.from + diff.insert.length;
    view.dispatch({
      changes: diff,
      ...(isFullReplace
        ? {}
        : {
            effects: [
              setExternalFlash.of({ from: diff.from, to: insertEnd }),
              EditorView.scrollIntoView(EditorSelection.range(diff.from, insertEnd), {
                y: "center",
              }),
            ],
          }),
    });
    if (!isFullReplace) {
      if (flashTimer.current) clearTimeout(flashTimer.current);
      flashTimer.current = setTimeout(() => {
        // 编辑器可能已随弹窗关闭销毁；view 引用变化说明已重建，都不再补发清除。
        if (viewRef.current === view) {
          view.dispatch({ effects: setExternalFlash.of(null) });
        }
      }, 1200);
    }
  }, [value]);
  useEffect(
    () => () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    },
    [],
  );

  const canFormat = useMemo(() => {
    if (!value.trim()) return false;
    try {
      parseByLanguage(value, language);
      return true;
    } catch {
      return false;
    }
  }, [value, language]);

  const handleFormat = () => {
    try {
      onChange(
        language === "toml"
          ? `${stringifyToml(parseToml(value)).trim()}\n`
          : JSON.stringify(JSON.parse(value), null, 2),
      );
    } catch {
      // lint 已实时提示错误，这里静默即可（canFormat 兜底，正常到不了这里）。
    }
  };

  return (
    <div>
      <div
        ref={hostRef}
        className={cn(
          "overflow-hidden rounded-lg border bg-slot transition-colors focus-within:border-accent",
          hasError ? "border-danger" : "border-transparent",
          disabled && "pointer-events-none opacity-60",
        )}
      />
      <button
        type="button"
        onClick={handleFormat}
        disabled={!canFormat || disabled}
        className={cn(
          "mt-1.5 inline-flex items-center gap-1 text-[11px] font-medium transition-colors",
          canFormat && !disabled
            ? "cursor-pointer text-muted hover:text-accent"
            : "cursor-not-allowed text-faint",
        )}
      >
        <Wand2 size={12} />
        {t("providers.modal.formatJson")}
      </button>
    </div>
  );
}

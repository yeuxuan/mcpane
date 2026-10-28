import { useMutation, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Eye, EyeOff, SlidersHorizontal, TriangleAlert } from "lucide-react";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc, type UnifiedProvider } from "../lib/ipc";
import { UNIFIED_PRESETS, type UnifiedPreset } from "../lib/providerPresets";
import { queryKeys } from "../lib/queryKeys";
import { useUnsavedGuard } from "../lib/useUnsavedGuard";
import { cn, errorText } from "../lib/utils";
import { ClientGlyph } from "./BrandIcon";
import { ConfirmDialog } from "./ConfirmDialog";
import { hasBuiltinIcon, inferProviderIcon, ProviderIcon } from "./ProviderIcon";
import { Input } from "./shadcn/input";
import { Button, Card, FormField, FormSection, SheetHero, Switch } from "./ui";

/**
 * 统一供应商面板（对齐 cc-switch UniversalProviderForm面板）：
 * - 一份 base_url + API Key，按 app 开关同时应用到 Claude Code 与 Codex；
 * - 每 app 独立模型配置（开关开启时显示）；
 * - 保存即同步影子条目（cc-switch 的 upsert+sync 合并为一步）。
 */
export function UnifiedProviderEditor({
  unified,
  nextSortOrder = 0,
  onClose,
}: {
  /** 传入表示编辑；缺省表示新建。 */
  unified?: UnifiedProvider;
  nextSortOrder?: number;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const guard = useUnsavedGuard(onClose);
  // ESC 走脏检测守卫；有弹层开着时归那一层（放弃确认框自己会处理）。
  const { requestClose } = guard;
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (document.querySelector('[role="dialog"][aria-modal="true"]')) return;
      requestClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [requestClose]);
  const isAdding = !unified;
  const [presetId, setPresetId] = useState<string>(UNIFIED_PRESETS[0]?.id ?? "custom-gateway");
  const [name, setName] = useState(unified?.name ?? UNIFIED_PRESETS[0]?.name ?? "");
  const [notes, setNotes] = useState(unified?.notes ?? "");
  const [websiteUrl, setWebsiteUrl] = useState(
    unified?.websiteUrl ?? UNIFIED_PRESETS[0]?.websiteUrl ?? "",
  );
  const [baseUrl, setBaseUrl] = useState(unified?.baseUrl ?? "");
  const [apiKey, setApiKey] = useState("");
  const [showKey, setShowKey] = useState(false);
  const [claudeEnabled, setClaudeEnabled] = useState(unified?.apps.claude ?? true);
  const [codexEnabled, setCodexEnabled] = useState(unified?.apps.codex ?? true);
  const first = UNIFIED_PRESETS[0];
  const [model, setModel] = useState(unified?.model ?? first?.claudeModel ?? "");
  const [modelHaiku, setModelHaiku] = useState(
    unified?.modelHaiku ?? first?.claudeModelHaiku ?? "",
  );
  const [modelSonnet, setModelSonnet] = useState(
    unified?.modelSonnet ?? first?.claudeModelSonnet ?? "",
  );
  const [modelOpus, setModelOpus] = useState(unified?.modelOpus ?? first?.claudeModelOpus ?? "");
  const [modelFable, setModelFable] = useState(
    unified?.modelFable ?? first?.claudeModelFable ?? "",
  );
  const [codexModel, setCodexModel] = useState(unified?.codexModel ?? first?.codexModel ?? "");
  const [error, setError] = useState<string | null>(null);
  // 新建模式下记录已创建的记录：若"创建成功但存 Key 失败"，重试走更新路径。
  const [created, setCreated] = useState<UnifiedProvider | null>(null);

  const applyPreset = (preset: UnifiedPreset) => {
    setPresetId(preset.id);
    setName(preset.name);
    setWebsiteUrl(preset.websiteUrl ?? "");
    setModel(preset.claudeModel ?? "");
    setModelHaiku(preset.claudeModelHaiku ?? "");
    setModelSonnet(preset.claudeModelSonnet ?? "");
    setModelOpus(preset.claudeModelOpus ?? "");
    setCodexModel(preset.codexModel ?? "");
  };

  const save = useMutation({
    mutationFn: async () => {
      const trimmed = (value: string) => value.trim() || null;
      const existing = unified ?? created;
      const payload: UnifiedProvider = {
        schemaVersion: 2,
        id: existing?.id ?? crypto.randomUUID().replaceAll("-", ""),
        name,
        baseUrl: baseUrl.trim(),
        apps: { claude: claudeEnabled, codex: codexEnabled },
        model: trimmed(model),
        modelHaiku: trimmed(modelHaiku),
        modelSonnet: trimmed(modelSonnet),
        modelOpus: trimmed(modelOpus),
        modelFable: trimmed(modelFable),
        codexModel: trimmed(codexModel),
        apiKeyRef: existing?.apiKeyRef ?? null,
        notes: trimmed(notes),
        websiteUrl: trimmed(websiteUrl),
        sortOrder: existing?.sortOrder ?? nextSortOrder,
      };
      const saved = await ipc.providerUnifiedUpsert(payload);
      if (isAdding) setCreated(saved);
      if (apiKey.trim()) {
        // 后端会同时把 Key 副本下发到已同步的影子条目。
        await ipc.providerUnifiedSetApiKey(saved.id, apiKey.trim());
      }
    },
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: queryKeys.providersUnified });
      void queryClient.invalidateQueries({ queryKey: queryKeys.providers });
      onClose();
    },
    onError: (e) => {
      setError(errorText(e));
      void queryClient.invalidateQueries({ queryKey: queryKeys.providersUnified });
      void queryClient.invalidateQueries({ queryKey: queryKeys.providers });
    },
  });

  // 同 ProviderEditor：认得出品牌才用中性板托 logo，认不出走主色板 + 首字母。
  const resolvedIcon = inferProviderIcon(name);
  const hasBrandGlyph = Boolean(resolvedIcon && hasBuiltinIcon(resolvedIcon));

  const valid =
    name.trim().length > 0 && baseUrl.trim().length > 0 && (claudeEnabled || codexEnabled);
  // 与供应商编辑器同一条规则：保存置灰时把原因摆在必填项下面，别让用户挨个猜。
  const blocker = (() => {
    const missing = [
      !name.trim() && t("providers.modal.name"),
      !baseUrl.trim() && t("providers.modal.baseUrl"),
    ].filter(Boolean) as string[];
    if (missing.length > 0) {
      return t("providers.editor.blockMissing", {
        fields: missing.join(t("providers.editor.fieldJoin")),
      });
    }
    if (!claudeEnabled && !codexEnabled) return t("providers.unified.blockNoApp");
    return null;
  })();

  return (
    // 容器级脏检测：change/input 事件冒泡，一处接住所有字段输入。
    <div
      className="editor-panel-in"
      onChangeCapture={guard.markDirty}
      onInputCapture={guard.markDirty}
    >
      {/* 与供应商编辑器同一套返回栏：同一个页面上两种交互（一边面板一边面板）
          比任一种单独用都糟——用户得在两套心智模型之间来回切。 */}
      <div className="sticky top-0 z-10 -mx-8 mb-5 border-line border-b bg-app/90 px-8 py-3 backdrop-blur">
        <div className="flex flex-wrap items-center gap-3">
          <Button size="sm" variant="ghost" icon={ArrowLeft} onClick={guard.requestClose}>
            {t("providers.editor.back")}
          </Button>
          {/* 标题写身份、副标题写场景，两行不重复同一句话。 */}
          <div className="min-w-0 flex-1">
            <p className="truncate font-semibold text-[15px] text-ink">
              {name.trim() ||
                t(isAdding ? "providers.unified.addTitle" : "providers.unified.editTitle")}
            </p>
            <p className="truncate text-[11px] text-faint">
              {`${t("providers.unified.tab")} · ${t(
                isAdding ? "providers.modal.addTitle" : "providers.modal.editTitle",
              )}`}
            </p>
          </div>
          {error ? <p className="text-xs text-danger">{error}</p> : null}
          <Button
            size="sm"
            variant="primary"
            disabled={!valid || save.isPending}
            aria-describedby={blocker ? "unified-blocker" : undefined}
            onClick={() => save.mutate()}
          >
            {save.isPending ? t("providers.unified.saving") : t("providers.unified.saveAndSync")}
          </Button>
        </div>
      </div>

      {/* 与供应商编辑器同宽同居中：两个面板在同一个页签里来回切，宽度都不一样会很跳。 */}
      <div className="mx-auto w-full max-w-[760px] space-y-5">
        {isAdding ? (
          <div>
            <p className="mb-2 px-1 font-semibold text-[11px] text-faint tracking-[0.06em] uppercase">
              {t("providers.modal.preset")}
            </p>
            <div className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-2">
              {UNIFIED_PRESETS.map((preset) => (
                <button
                  key={preset.id}
                  type="button"
                  aria-pressed={presetId === preset.id}
                  onClick={() => applyPreset(preset)}
                  className={cn(
                    "inline-flex w-full items-center justify-start gap-2 rounded-lg border px-3 py-2 text-[13px] font-medium transition-colors",
                    presetId === preset.id
                      ? "border-accent bg-accent-solid text-on-accent-solid"
                      : // 未选中态给白底：站在窗口灰底上时 bg-app 与背景同色，按钮看着像没渲染。
                        "border-line bg-card text-muted hover:border-line-strong hover:text-ink",
                  )}
                  title={t(preset.descriptionKey)}
                >
                  <ProviderIcon
                    name={preset.name}
                    icon={preset.icon}
                    size={16}
                    plate={presetId === preset.id}
                  />
                  <span className="truncate">{preset.name}</span>
                </button>
              ))}
            </div>
            <p className="mt-1.5 text-[11px] text-faint">{t("providers.unified.presetHint")}</p>
          </div>
        ) : null}

        {/* 接入纸：抬头 + 三段接线 + 缺口条，与供应商编辑器同一套结构。 */}
        <Card className="overflow-hidden">
          <SheetHero
            glyph={
              hasBrandGlyph ? (
                <ProviderIcon name={name} size={28} />
              ) : name.trim() ? (
                <ProviderIcon name={name} size={30} flat />
              ) : (
                <SlidersHorizontal size={24} aria-hidden />
              )
            }
            tone={hasBrandGlyph ? "brand" : "accent"}
            title={name.trim() || t("providers.editor.untitled")}
            subtitle={
              <p className="truncate font-mono text-xs text-faint">
                {baseUrl.trim() || t("providers.editor.noBaseUrl")}
              </p>
            }
          />

          <div className="@container border-line border-t px-7 pt-6 pb-6">
            <FormSection
              label={t("providers.editor.section.identity")}
              state={name.trim() ? "done" : "todo"}
            >
              <div className="space-y-4">
                <FormField id="unified-name" label={t("providers.modal.name")} required>
                  <Input id="unified-name" value={name} onChange={(e) => setName(e.target.value)} />
                </FormField>
                <div className="grid gap-x-5 gap-y-4 @[420px]:grid-cols-2">
                  <FormField id="unified-notes" label={t("providers.modal.notes")}>
                    <Input
                      id="unified-notes"
                      placeholder={t("providers.modal.notesPlaceholder")}
                      value={notes}
                      onChange={(e) => setNotes(e.target.value)}
                    />
                  </FormField>
                  <FormField id="unified-website" label={t("providers.modal.websiteUrl")}>
                    <Input
                      id="unified-website"
                      className="font-mono"
                      placeholder="https://example.com"
                      value={websiteUrl}
                      onChange={(e) => setWebsiteUrl(e.target.value)}
                    />
                  </FormField>
                </div>
              </div>
            </FormSection>

            <FormSection
              label={t("providers.editor.section.endpoint")}
              state={baseUrl.trim() ? "done" : "todo"}
            >
              <FormField
                id="unified-base-url"
                label={t("providers.modal.baseUrl")}
                required
                hint={t("providers.unified.baseUrlHint")}
              >
                <Input
                  id="unified-base-url"
                  className="font-mono"
                  placeholder="https://gateway.example.com"
                  value={baseUrl}
                  onChange={(e) => setBaseUrl(e.target.value)}
                />
              </FormField>
            </FormSection>

            <FormSection
              label={t("providers.editor.section.credential")}
              state={apiKey.trim() || unified?.apiKeyRef ? "done" : "optional"}
              last
            >
              <FormField
                id="unified-api-key"
                label={t("providers.modal.apiKey")}
                hint={t("providers.modal.apiKeyHint")}
              >
                <div className="relative">
                  <Input
                    id="unified-api-key"
                    type={showKey ? "text" : "password"}
                    autoComplete="off"
                    className="w-full pr-10"
                    placeholder={unified?.apiKeyRef ? t("providers.modal.apiKeyKeep") : ""}
                    value={apiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                  />
                  {apiKey ? (
                    <button
                      type="button"
                      onClick={() => setShowKey((v) => !v)}
                      aria-label={
                        showKey ? t("providers.modal.hideKey") : t("providers.modal.showKey")
                      }
                      className="absolute inset-y-0 right-0 flex items-center pr-3 text-faint transition-colors hover:text-ink"
                    >
                      {showKey ? <EyeOff size={15} aria-hidden /> : <Eye size={15} aria-hidden />}
                    </button>
                  ) : null}
                </div>
              </FormField>
            </FormSection>
          </div>

          {blocker ? (
            <div
              id="unified-blocker"
              className="flex items-center gap-2 border-line border-t bg-warn-soft px-7 py-3"
            >
              <TriangleAlert size={13} strokeWidth={2} aria-hidden className="shrink-0 text-warn" />
              <p className="text-[11px] text-warn">{blocker}</p>
            </div>
          ) : null}
        </Card>

        {/* 应用开关 + 每 app 模型配置（对齐 cc-switch：开关开启才显示模型区） */}
        <div>
          <p className="px-1 pt-2 pb-1 font-semibold text-[11px] text-faint tracking-[0.06em] uppercase">
            {t("providers.unified.apps")}
          </p>
          {/* 两个客户端合成一张纸、中间一条线：各自成卡时就是灰底上两块白，
              与上面的接入纸叠成三段式，页面又碎又灰。 */}
          <div className="divide-y divide-line overflow-hidden rounded-xl border border-line bg-card shadow-[var(--shadow-card)]">
            <div>
              <div className="flex items-center justify-between px-7 py-3.5">
                <span className="inline-flex items-center gap-2.5 font-semibold text-[13px] text-ink">
                  <ClientGlyph app="claude" size={16} />
                  Claude Code
                </span>
                <Switch checked={claudeEnabled} onChange={setClaudeEnabled} label="Claude Code" />
              </div>
              {claudeEnabled ? (
                <div className="@container border-line border-t px-7 pt-5 pb-6">
                  <div className="grid gap-x-5 gap-y-4 @[420px]:grid-cols-2">
                    {(
                      [
                        [t("providers.modal.fallbackModel"), model, setModel],
                        ["Haiku", modelHaiku, setModelHaiku],
                        ["Sonnet", modelSonnet, setModelSonnet],
                        ["Opus", modelOpus, setModelOpus],
                        ["Fable", modelFable, setModelFable],
                      ] as const
                    ).map(([label, value, set]) => (
                      <FormField key={label} label={label}>
                        <Input
                          className="w-full font-mono"
                          value={value}
                          onChange={(e) => set(e.target.value)}
                        />
                      </FormField>
                    ))}
                  </div>
                </div>
              ) : null}
            </div>
            <div>
              <div className="flex items-center justify-between px-7 py-3.5">
                <span className="inline-flex items-center gap-2.5 font-semibold text-[13px] text-ink">
                  <ClientGlyph app="codex" size={16} />
                  Codex
                </span>
                <Switch checked={codexEnabled} onChange={setCodexEnabled} label="Codex" />
              </div>
              {codexEnabled ? (
                <div className="border-line border-t px-7 pt-5 pb-6">
                  <FormField
                    label={t("providers.unified.codexModel")}
                    hint={t("providers.unified.codexBaseUrlNote")}
                  >
                    <Input
                      className="w-full font-mono"
                      value={codexModel}
                      onChange={(e) => setCodexModel(e.target.value)}
                    />
                  </FormField>
                </div>
              ) : null}
            </div>
          </div>
        </div>
      </div>

      {guard.confirmOpen ? (
        <ConfirmDialog
          title={t("common.discardTitle")}
          message={t("common.discardMessage")}
          confirmText={t("common.discardConfirm")}
          cancelText={t("common.keepEditing")}
          onConfirm={guard.confirmDiscard}
          onCancel={guard.cancelDiscard}
        />
      ) : null}
    </div>
  );
}

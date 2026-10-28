import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc, type ModelPrices } from "../lib/ipc";
import { formatPrice, parsePrice, scalePrices } from "../lib/priceOverride";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { Button, ErrorBanner, Field, LabeledField, Modal } from "./ui";

type Draft = Record<keyof ModelPrices, string>;

const FIELDS: { key: keyof ModelPrices; labelKey: string }[] = [
  { key: "input", labelKey: "usage.price.input" },
  { key: "output", labelKey: "usage.price.output" },
  { key: "cacheRead", labelKey: "usage.price.cacheRead" },
  { key: "cacheCreation", labelKey: "usage.price.cacheCreation" },
];

const toDraft = (prices: ModelPrices | null | undefined): Draft => ({
  input: prices ? formatPrice(prices.input) : "",
  output: prices ? formatPrice(prices.output) : "",
  cacheRead: prices ? formatPrice(prices.cacheRead) : "0",
  cacheCreation: prices ? formatPrice(prices.cacheCreation) : "0",
});

function PriceForm({
  modelId,
  official,
  custom,
  onClose,
}: {
  modelId: string;
  official: ModelPrices | null;
  custom: ModelPrices | null;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [draft, setDraft] = useState<Draft>(() => toDraft(custom ?? official));
  const [multiplier, setMultiplier] = useState("");
  const [error, setError] = useState<string | null>(null);

  const input = parsePrice(draft.input);
  const output = parsePrice(draft.output);
  const cacheRead = parsePrice(draft.cacheRead);
  const cacheCreation = parsePrice(draft.cacheCreation);
  const prices =
    input !== null && output !== null && cacheRead !== null && cacheCreation !== null
      ? { input, output, cacheRead, cacheCreation }
      : null;
  const multiplierValue = parsePrice(multiplier);
  const scaled =
    official && multiplierValue !== null ? scalePrices(official, multiplierValue) : null;

  const done = () => {
    queryClient.invalidateQueries({ queryKey: queryKeys.usageAll });
    onClose();
  };
  const save = useMutation({
    mutationFn: async () => {
      if (prices) await ipc.usagePriceOverrideSet(modelId, prices);
    },
    onSuccess: done,
    onError: (e) => setError(errorText(e)),
  });
  const clear = useMutation({
    mutationFn: () => ipc.usagePriceOverrideClear(modelId),
    onSuccess: done,
    onError: (e) => setError(errorText(e)),
  });
  const busy = save.isPending || clear.isPending;

  return (
    <Modal
      title={t("usage.price.title", { model: modelId })}
      onClose={onClose}
      footer={
        <>
          {custom ? (
            <Button
              size="sm"
              className="mr-auto"
              loading={clear.isPending}
              disabled={busy}
              onClick={() => clear.mutate()}
            >
              {t("usage.price.reset")}
            </Button>
          ) : null}
          <Button size="sm" disabled={busy} onClick={onClose}>
            {t("confirm.cancel")}
          </Button>
          <Button
            size="sm"
            variant="primary"
            loading={save.isPending}
            disabled={!prices || busy}
            onClick={() => save.mutate()}
          >
            {t("usage.price.save")}
          </Button>
        </>
      }
    >
      <ErrorBanner message={error} onDismiss={() => setError(null)} />
      <p className="mb-3 text-xs text-muted">{t("usage.price.scope")}</p>
      {!official ? <p className="mb-3 text-xs text-faint">{t("usage.price.noOfficial")}</p> : null}

      {official ? (
        <div className="mb-4 flex items-end gap-2">
          <LabeledField label={t("usage.price.multiplier")} hint={t("usage.price.multiplierHint")}>
            <Field
              inputMode="decimal"
              value={multiplier}
              onChange={(e) => setMultiplier(e.target.value)}
              placeholder="0.6"
              className="w-28 font-mono"
            />
          </LabeledField>
          <Button
            size="sm"
            disabled={!scaled}
            onClick={() => scaled && setDraft(toDraft(scaled))}
            className="mb-[22px]"
          >
            {t("usage.price.apply")}
          </Button>
        </div>
      ) : null}

      <div className="grid grid-cols-2 gap-3">
        {FIELDS.map(({ key, labelKey }) => {
          const bad = draft[key] !== "" && parsePrice(draft[key]) === null;
          return (
            <LabeledField
              key={key}
              label={t(labelKey)}
              error={bad ? t("usage.price.invalid") : undefined}
              hint={
                official
                  ? t("usage.price.official", { price: formatPrice(official[key]) })
                  : undefined
              }
            >
              <Field
                inputMode="decimal"
                value={draft[key]}
                onChange={(e) => setDraft({ ...draft, [key]: e.target.value })}
                className="font-mono"
                aria-label={t(labelKey)}
              />
            </LabeledField>
          );
        })}
      </div>
      <p className="mt-3 text-[11px] text-faint">{t("usage.price.unit")}</p>
    </Modal>
  );
}

/** 单个模型的价格覆盖。按模型生效、不区分供应商（ADR-026）；对历史用量同样生效。 */
export function PriceOverrideModal({ modelId, onClose }: { modelId: string; onClose: () => void }) {
  const { t } = useTranslation();
  const info = useQuery({
    queryKey: ["usage", "price-info", modelId],
    queryFn: () => ipc.usagePriceInfo(modelId),
    retry: false,
  });
  if (info.data) {
    return (
      <PriceForm
        modelId={modelId}
        official={info.data.official}
        custom={info.data.custom}
        onClose={onClose}
      />
    );
  }
  return (
    <Modal title={t("usage.price.title", { model: modelId })} onClose={onClose}>
      {info.isError ? (
        <ErrorBanner message={t("usage.loadError", { error: errorText(info.error) })} />
      ) : (
        <p className="py-3 text-xs text-faint">{t("usage.loading")}</p>
      )}
    </Modal>
  );
}

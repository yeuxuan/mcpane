import { ArrowRight, ExternalLink, Search } from "lucide-react";
import { useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { type DiscoverCandidate, type DiscoverNotice, type DiscoverResult, ipc } from "../lib/ipc";
import {
  candidateToFill,
  DISCOVER_RUNTIME_LABEL_KEY,
  installSummary,
  optionalFieldNames,
} from "../lib/serverDiscover";
import { cn } from "../lib/utils";
import type { PresetFill } from "./ServerPresetPicker";
import { Button, ErrorBanner, SearchInput, Tag } from "./ui";

const NOTICE_KEY: Record<DiscoverNotice, string> = {
  repositoryNotListed: "serverDiscover.notice.repositoryNotListed",
  directCandidate: "serverDiscover.notice.directCandidate",
  partialFailure: "serverDiscover.notice.partialFailure",
};

function CandidateCard({
  candidate,
  onPick,
}: {
  candidate: DiscoverCandidate;
  onPick: (fill: PresetFill) => void;
}) {
  const { t } = useTranslation();
  const fill = candidateToFill(candidate);
  const { install } = candidate;
  const optional = optionalFieldNames(install);
  const label = candidate.title || candidate.name;
  const repo = candidate.repositoryUrl;
  return (
    <div className="rounded-lg border border-line p-3 transition-colors hover:border-line-strong">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-1.5">
            <p className="truncate text-[13px] font-medium text-ink">{label}</p>
            {candidate.exact ? <Tag tone="green">{t("serverDiscover.exact")}</Tag> : null}
            {candidate.source === "direct" ? (
              <Tag tone="amber">{t("serverDiscover.unregistered")}</Tag>
            ) : null}
            {install.kind === "stdio" ? (
              <Tag>{t(DISCOVER_RUNTIME_LABEL_KEY[install.runtime])}</Tag>
            ) : null}
            {install.kind === "remote" ? <Tag>{t("serverPreset.runtime.none")}</Tag> : null}
            {install.kind === "unsupported" ? <Tag>{t("serverDiscover.unsupported")}</Tag> : null}
          </div>
          {candidate.namespace ? (
            <p className="mt-0.5 truncate font-mono text-[11px] text-faint">
              {candidate.namespace}
            </p>
          ) : null}
          {candidate.description ? (
            <p className="mt-1 line-clamp-2 text-xs text-muted">{candidate.description}</p>
          ) : null}
          <p
            className={cn(
              "mt-1.5 break-all rounded-md bg-app px-2 py-1 font-mono text-[11px]",
              install.kind === "unsupported" ? "text-muted" : "text-ink",
            )}
          >
            {installSummary(install)}
          </p>
          {candidate.todo.map((line) => (
            <p key={line} className="mt-1 text-xs text-warn">
              {line}
            </p>
          ))}
          {optional.length > 0 ? (
            <p className="mt-1 text-xs text-muted">
              {t("serverDiscover.optionalEnv", { names: optional.join(", ") })}
            </p>
          ) : null}
        </div>
        {repo ? (
          <button
            type="button"
            onClick={() => ipc.openExternal(repo).catch(() => {})}
            aria-label={t("serverDiscover.openRepo", { name: label })}
            title={repo}
            className="shrink-0 rounded-md p-1.5 text-muted transition-colors hover:bg-hover hover:text-ink"
          >
            <ExternalLink aria-hidden className="h-3.5 w-3.5" />
          </button>
        ) : null}
        {fill ? (
          <Button
            size="sm"
            icon={ArrowRight}
            // 同一服务器常有远程与本地两种启动方式，名字相同，得靠命令区分。
            aria-label={t("serverDiscover.useNamed", { name: label, how: installSummary(install) })}
            onClick={() => onPick(fill)}
          >
            {t("serverPreset.use")}
          </Button>
        ) : null}
      </div>
    </div>
  );
}

/**
 * 按地址查找：粘贴 GitHub 仓库 / npm / PyPI 包名 / 远程服务器地址，查官方 MCP Registry。
 *
 * 和预设一样只**填表**，不保存：命令来自公共 Registry（第三方登记），要让用户在表单里
 * 看清并按「测试连通」之后自己决定（ADR-025）。
 */
export function ServerDiscoverPanel({ onPick }: { onPick: (fill: PresetFill) => void }) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<DiscoverResult | null>(null);
  /** 只认最后一次查询的响应：Registry 一次要等十几秒，用户很可能中途改了输入。 */
  const seq = useRef(0);

  const search = async () => {
    const text = query.trim();
    if (!text) return;
    const mine = ++seq.current;
    setBusy(true);
    setError(null);
    try {
      const next = await ipc.serverDiscover(text);
      if (mine !== seq.current) return;
      setResult(next);
    } catch (e) {
      if (mine !== seq.current) return;
      setResult(null);
      setError(String(e));
    } finally {
      if (mine === seq.current) setBusy(false);
    }
  };

  const exact = result?.candidates.filter((c) => c.exact) ?? [];
  const rest = result?.candidates.filter((c) => !c.exact) ?? [];
  const nothing = result && result.candidates.length === 0 && result.notices.length === 0;

  return (
    <section
      aria-label={t("serverDiscover.title")}
      className="mb-5 space-y-2 border-b border-line pb-5"
    >
      <div>
        <h3 className="text-[13px] font-medium text-ink">{t("serverDiscover.title")}</h3>
        <p className="text-xs text-muted">{t("serverDiscover.intro")}</p>
      </div>
      <form
        className="flex items-center gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          void search();
        }}
      >
        <SearchInput
          className="min-w-0 flex-1"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder={t("serverDiscover.placeholder")}
          aria-label={t("serverDiscover.title")}
          spellCheck={false}
          autoComplete="off"
        />
        {/* 查询期间不禁用：禁用提交按钮会连带让输入框回车失效，用户没法在等待中改查询。 */}
        <Button type="submit" icon={Search} disabled={!query.trim()}>
          {t("serverDiscover.search")}
        </Button>
      </form>
      {busy ? <p className="text-xs text-muted">{t("serverDiscover.searching")}</p> : null}
      <ErrorBanner message={error} onDismiss={() => setError(null)} />
      {result ? (
        <div className="space-y-2">
          {result.notices.map((notice) => (
            <p key={notice} className="text-xs text-warn">
              {t(NOTICE_KEY[notice])}
            </p>
          ))}
          {nothing ? <p className="text-xs text-muted">{t("serverDiscover.empty")}</p> : null}
          {exact.map((c) => (
            <CandidateCard key={c.id} candidate={c} onPick={onPick} />
          ))}
          {rest.length > 0 ? (
            <>
              <p className="pt-1 text-xs text-muted">{t("serverDiscover.similar")}</p>
              {rest.map((c) => (
                <CandidateCard key={c.id} candidate={c} onPick={onPick} />
              ))}
            </>
          ) : null}
          {result.candidates.length > 0 ? (
            <p className="text-xs text-faint">{t("serverDiscover.trust")}</p>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}

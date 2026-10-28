/**
 * 本地环境（对齐 cc-switch 设置页的「本地环境检查」）：一屏看清各个客户端
 * 装没装、装在哪、什么版本、跑不跑得起来。
 *
 * 与 cc-switch 的差异：**不代跑安装/升级命令**。cc-switch 为一键升级写了整套
 * 包管理器锚定（nvm/fnm/volta/bun/homebrew 各有各的 bin 布局，GUI 进程还缺
 * PATH），命令跑错地方比不跑更糟；这里给一行随时可粘进终端的官方命令 +
 * 一键复制，把执行留在用户自己的 shell 里。
 *
 * 跑不起来的那几行是这个面板的重点，也是全卡唯一的色块：只报"坏了"是把诊断
 * 当结论，所以把后端带上来的失败原因、原始报错和对症的下一步一并摊开。
 */
import { useQuery, useQueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { ExternalLink, RefreshCw, TerminalSquare, TriangleAlert } from "lucide-react";
import { useTranslation } from "react-i18next";
import { PROVIDER_APPS } from "../lib/appVisibility";
import { CLIENT_META } from "../lib/clients";
import { type ClientApp, type ClientEnv, type ClientEnvProblem, ipc } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { BrandGlyph, SurfaceBadge } from "./BrandIcon";
import { Button, Card, CopyButton, IconButton, IconChip, StatusBadge } from "./ui";

export function LocalEnvSection() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  // 跑 --version 有成本，缓存住；用户点「重新检测」时才强制刷新。
  const env = useQuery({
    queryKey: queryKeys.clientEnv,
    queryFn: ipc.clientEnvStatus,
    retry: false,
    staleTime: 5 * 60 * 1000,
  });
  const byApp = new Map((env.data ?? []).map((item) => [item.app, item]));
  const counts = countByState(PROVIDER_APPS, byApp);

  const recheck = () => {
    void queryClient.invalidateQueries({ queryKey: queryKeys.clientEnv });
    // 供应商页的页签标灰用的是轻量探测，一起刷新，两处结论不打架。
    void queryClient.invalidateQueries({ queryKey: queryKeys.clientsInstalled });
  };

  return (
    <Card className="mb-5 p-5">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <IconChip icon={TerminalSquare} tone={counts.broken > 0 ? "rose" : "blue"} />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium text-ink">{t("settings.localEnv.title")}</p>
          <p className="text-xs text-muted">{t("settings.localEnv.desc")}</p>
        </div>
        {/* 结论前置：八行等权重平铺时，"我该看哪一行"得靠逐行扫，摘要一眼给出答案。 */}
        {env.data?.length ? (
          <div className="flex items-center gap-3">
            <Tally
              dot="bg-run-dot"
              label={t("settings.localEnv.summaryReady", { count: counts.ready })}
            />
            {counts.broken > 0 ? (
              <Tally
                dot="bg-danger"
                label={t("settings.localEnv.summaryBroken", { count: counts.broken })}
              />
            ) : null}
            <Tally
              dot="bg-faint"
              label={t("settings.localEnv.summaryMissing", { count: counts.missing })}
            />
          </div>
        ) : null}
        <Button
          size="sm"
          variant="secondary"
          icon={RefreshCw}
          disabled={env.isFetching}
          onClick={recheck}
        >
          {env.isFetching ? t("settings.localEnv.checking") : t("settings.localEnv.recheck")}
        </Button>
      </div>

      {/* 顺序恒为 PROVIDER_APPS，不把出问题的挑到前面：八行很短，位置稳定比
          "智能置顶"更好找，Claude Code 与 Claude Desktop 相邻也是刻意的。 */}
      <div className="mt-3 divide-y divide-line">
        {PROVIDER_APPS.map((app) => (
          <ClientEnvRow key={app} app={app} env={byApp.get(app)} loading={env.isLoading} />
        ))}
      </div>

      {env.isError ? (
        <p className="mt-3 text-xs text-danger">
          {t("settings.localEnv.error", { error: errorText(env.error) })}
        </p>
      ) : null}
    </Card>
  );
}

/** 摘要里的一段计数：色点沿用行内 StatusBadge 的圆点语言，扫视时对得上。 */
function Tally({ dot, label }: { dot: string; label: string }) {
  return (
    <span className="inline-flex items-center gap-1.5 text-[11px] text-muted">
      <span className={`h-1.5 w-1.5 rounded-full ${dot}`} />
      {label}
    </span>
  );
}

function countByState(apps: readonly ClientApp[], byApp: Map<ClientApp, ClientEnv>) {
  return apps.reduce(
    (acc, app) => {
      // 后端漏报某个 app 时按"未检测到"计——那正是它那一行会显示的东西，
      // 跳过不计会让摘要的总数和列表行数对不上。
      const item = byApp.get(app);
      if (!item?.installed) acc.missing += 1;
      else if (item.runnable) acc.ready += 1;
      else acc.broken += 1;
      return acc;
    },
    { ready: 0, broken: 0, missing: 0 },
  );
}

function ClientEnvRow({
  app,
  env,
  loading,
}: {
  app: ClientApp;
  env?: ClientEnv;
  loading: boolean;
}) {
  const { t } = useTranslation();
  const meta = CLIENT_META[app];
  const info = meta.install;
  const installed = env?.installed ?? false;
  const broken = installed && env?.runnable === false;
  // 装了却跑不起来单独成一态：这类半损坏只报"已安装"会把人引向错误方向。
  const badge = loading
    ? { tone: "pending" as const, text: t("settings.localEnv.checking") }
    : !installed
      ? { tone: "stop" as const, text: t("settings.localEnv.missing") }
      : broken
        ? { tone: "danger" as const, text: problemLabel(env?.problem, t) }
        : { tone: "run" as const, text: t("settings.localEnv.ready") };

  return (
    <div className="py-2.5">
      <div className="flex items-center gap-3">
        <span
          aria-hidden
          className="relative flex h-7 w-7 shrink-0 items-center justify-center"
          style={{ color: installed ? meta.color : "currentColor" }}
        >
          <BrandGlyph name={app} size={17} />
          {/* Claude Code 与 Claude Desktop 在这张表里上下相邻、logo 相同。 */}
          <SurfaceBadge app={app} className="right-0 bottom-0 border-card bg-hover" />
        </span>
        <div className="flex min-w-0 flex-1 flex-wrap items-center gap-2">
          <span className="text-[13px] text-ink">{meta.label}</span>
          {env?.version ? (
            <span className="font-mono text-[11px] text-muted">v{env.version}</span>
          ) : null}
          <StatusBadge tone={badge.tone}>{badge.text}</StatusBadge>
        </div>
        {/* 官网降为图标：它对已装好的客户端没什么用，不该是整行最重的元素。 */}
        <IconButton
          icon={ExternalLink}
          aria-label={`${meta.label} — ${t("settings.localEnv.site")}`}
          title={t("settings.localEnv.site")}
          onClick={() => void ipc.openExternal(info.url)}
        />
      </div>

      {/* 次级信息统一缩进到名称那一列（图标 28px + gap 12px）。 */}
      <div className="ml-10">
        {/* 路径是排障的关键信息：同名 CLI 装了两份时，"用的是哪一份"只有它能回答。 */}
        {env?.path ? (
          <p className="truncate font-mono text-[11px] text-faint" title={env.path}>
            {env.path}
          </p>
        ) : null}

        {broken && env?.problem ? (
          <ProblemPanel
            problem={env.problem}
            label={meta.label}
            path={env.path}
            command={info.command}
          />
        ) : null}

        {/* 装了好几份：升级的常常不是实际被调用的那份，用户会困惑"更新了怎么没变"。
            不猜哪份对——上面那行 path 就是会被用到的，这里把其余的摆出来。 */}
        {env?.conflicts?.length ? (
          <p className="mt-1 text-[11px] text-warn">
            {t("settings.localEnv.conflicts", { count: env.conflicts.length })}
            {env.conflicts.map((extra) => (
              <span key={extra} className="ml-1 block truncate font-mono text-faint" title={extra}>
                {extra}
              </span>
            ))}
          </p>
        ) : null}

        {!installed && !loading && info.command ? (
          <p className="mt-1 flex flex-wrap items-center gap-2">
            <code className="rounded border border-line bg-app px-1.5 py-0.5 font-mono text-[11px] text-muted">
              {info.command}
            </code>
            <CopyButton value={info.command} label={t("ui.copy")} />
          </p>
        ) : null}
      </div>
    </div>
  );
}

/**
 * 跑不起来时的诊断块：说清哪坏了、附上它自己的报错、给出下一步。
 * 全卡唯一的色块，不折叠——折叠是留给"多数人用不到"的东西的，这里恰好相反。
 */
function ProblemPanel({
  problem,
  label,
  path,
  command,
}: {
  problem: ClientEnvProblem;
  label: string;
  path?: string | null;
  command?: string;
}) {
  const { t } = useTranslation();
  const { headline, detail } = describeProblem(problem, t);
  // 带走的诊断要能独立看懂：贴进 issue 或直接丢给 AI 都不用再补充上下文。
  const clipboard = [`${label} — ${problemLabel(problem, t)}`, path, headline, detail]
    .filter(Boolean)
    .join("\n");

  return (
    <div className="mt-2 rounded-lg border border-danger-line bg-danger-soft p-2.5">
      <div className="flex items-start gap-2">
        <TriangleAlert
          size={13}
          strokeWidth={2.5}
          aria-hidden
          className="mt-0.5 shrink-0 text-danger"
        />
        <p className="min-w-0 text-[11px] leading-relaxed text-danger">{headline}</p>
      </div>
      {detail ? (
        // 无边框：红底上再套一圈灰边就成了框中框，白底本身已经把报错原文分出层次。
        <pre className="mt-2 max-h-28 overflow-auto whitespace-pre-wrap break-all rounded bg-card px-2 py-1.5 font-mono text-[11px] text-muted">
          {detail}
        </pre>
      ) : null}
      <div className="mt-2 flex flex-wrap items-center gap-2">
        <CopyButton value={clipboard} label={t("settings.localEnv.copyDiagnostics")} />
        {command ? <CopyButton value={command} label={t("settings.localEnv.reinstall")} /> : null}
      </div>
    </div>
  );
}

/** 状态徽标上的短词。三种失败的补救动作不同，不能共用一句「跑不起来」。 */
function problemLabel(problem: ClientEnvProblem | null | undefined, t: TFunction): string {
  switch (problem?.kind) {
    case "timeout":
      return t("settings.localEnv.brokenTimeout");
    case "exit":
      return t("settings.localEnv.brokenExit");
    default:
      // 后端说了跑不起来却没给原因时，"启动失败"是最不会把人带偏的说法。
      return t("settings.localEnv.brokenSpawn");
  }
}

/** 失败原因 → 一句人话 + 它自己的报错原文。 */
function describeProblem(
  problem: ClientEnvProblem,
  t: TFunction,
): { headline: string; detail: string } {
  switch (problem.kind) {
    case "spawn":
      return { headline: t("settings.localEnv.brokenSpawnDesc"), detail: problem.detail };
    case "timeout":
      return {
        headline: t("settings.localEnv.brokenTimeoutDesc", { seconds: problem.seconds }),
        detail: "",
      };
    case "exit":
      return {
        headline:
          problem.code === null
            ? t("settings.localEnv.brokenExitNoCodeDesc")
            : t("settings.localEnv.brokenExitDesc", { code: problem.code }),
        detail: problem.stderr,
      };
  }
  // 后端加了新 kind 而这里还没跟上：宁可把原始结构摊开，也不让面板白屏。
  return { headline: "", detail: JSON.stringify(problem) };
}

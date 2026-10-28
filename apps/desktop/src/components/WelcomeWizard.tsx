/**
 * 首启向导：三步把人配起来，然后再讲界面。
 *
 * 与聚光灯导览（components/OnboardingTour.tsx）分工明确——那边指认控件、不做
 * 任何有副作用的事；这里相反，只做两件带结果的事：把 cc-switch 的配置搬过来，
 * 以及问一句"你先想干什么"，据此决定落到哪一页、接着讲哪一套导览。
 *
 * 不复用 Modal：它的遮罩点一下就关，标题栏还带个 X——首启这一屏被误触关掉，
 * 用户就再也遇不到迁移入口了（重跑得自己去设置里找）。这里只留显式的「跳过」
 * 与 ESC 两条出口。
 */
import { useQuery } from "@tanstack/react-query";
import { ArrowRight, Download, Server, Wand2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { CcSwitchImportPanel } from "./CcSwitchImport";
import type { PageKey } from "./Shell";
import { ConvergenceDiagram } from "./TourHero";
import { Button, IconChip } from "./ui";

type Step = "intro" | "migrate" | "route";

export function WelcomeWizard({
  onNavigate,
  onDone,
}: {
  onNavigate: (page: PageKey) => void;
  /** `markOverviewSeen` 为真 = 用户选了直奔任务，总览导览不再接着弹。 */
  onDone: (markOverviewSeen: boolean) => void;
}) {
  const { t } = useTranslation();
  const [step, setStep] = useState<Step>("intro");
  const [imported, setImported] = useState(false);

  /**
   * 没装过 cc-switch 就不该有这一步。
   *
   * 留着它，多数新用户三步里有一步是「没找到 cc-switch 的配置」加一行看不懂的
   * 路径——既没得做，还平白让人以为自己少装了什么。查询与面板共用同一个 key，
   * 缓存命中，不会多跑一次（那要复制整个库）。
   */
  const detection = useQuery({
    queryKey: queryKeys.ccswitchDetect,
    queryFn: ipc.ccswitchDetect,
    staleTime: Number.POSITIVE_INFINITY,
    retry: false,
  });
  // 只有"确实查过、确实没有"才省掉这一步。
  //
  // 查失败不算没有——库被 cc-switch 自己锁着就会失败，此时省掉这一步，一个正
  // 装着 cc-switch 的用户会以为 MCPane 压根不支持迁移。那一步照常出现，由面板
  // 说明失败原因。还没查完时也先按"有"排版：用户读完开场那几行通常已经查完，
  // 真没有也只是步数从 3 变 2，而反过来（先说 2 步又冒出第三步）更突兀。
  const hasCcSwitch = !detection.isFetched || detection.isError || Boolean(detection.data?.found);
  const steps: Step[] = hasCcSwitch ? ["intro", "migrate", "route"] : ["intro", "route"];
  const index = Math.max(0, steps.indexOf(step));

  const doneRef = useRef(onDone);
  doneRef.current = onDone;
  // ESC 随时退出，与导览同惯例：被首启这一屏困住比没有向导更糟。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") doneRef.current(true);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const goTask = (page: PageKey) => {
    onNavigate(page);
    onDone(true);
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-4"
      role="dialog"
      aria-modal="true"
      aria-label={t("wizard.label")}
    >
      <div className="modal-backdrop-in absolute inset-0 bg-scrim" aria-hidden />
      <div className="modal-panel-in relative flex max-h-[88vh] w-full max-w-xl flex-col overflow-y-auto rounded-xl border border-line bg-card p-6 shadow-[var(--shadow-pop)]">
        <div className="mb-4 flex items-baseline justify-between gap-3">
          {/* 读屏会把「1 / 3」念成「1 斜杠 3」，所以整块另给一句人话。 */}
          <p
            role="status"
            aria-label={t("wizard.progress", { current: index + 1, total: steps.length })}
            className="font-mono text-[11px] tracking-tight text-faint tabular-nums"
          >
            <span className="text-accent">{index + 1}</span>
            <span className="mx-0.5">/</span>
            {steps.length}
          </p>
          <button
            type="button"
            onClick={() => onDone(true)}
            className="-mr-1 rounded px-1 py-0.5 text-[11px] text-faint transition-colors hover:text-ink"
          >
            {t("wizard.skip")}
          </button>
        </div>

        {step === "intro" ? (
          <IntroStep onNext={() => setStep(hasCcSwitch ? "migrate" : "route")} />
        ) : null}
        {step === "migrate" ? (
          <MigrateStep
            imported={imported}
            onImported={() => setImported(true)}
            onNext={() => setStep("route")}
          />
        ) : null}
        {step === "route" ? (
          <RouteStep
            onPick={goTask}
            onOverview={() => {
              onNavigate("dashboard");
              onDone(false);
            }}
          />
        ) : null}
      </div>
    </div>
  );
}

function IntroStep({ onNext }: { onNext: () => void }) {
  const { t } = useTranslation();
  return (
    <>
      <ConvergenceDiagram />
      <h2 className="mt-5 text-[19px] font-semibold leading-tight tracking-tight text-ink">
        {t("wizard.intro.title")}
      </h2>
      <p className="mt-2 text-[13px] leading-relaxed text-muted">{t("wizard.intro.desc")}</p>
      <div className="mt-6 flex justify-end">
        <Button variant="primary" size="sm" onClick={onNext}>
          {t("wizard.next")}
        </Button>
      </div>
    </>
  );
}

/**
 * 迁移这一步只认 cc-switch：它是同类工具里唯一在本机留下可读配置的，
 * 且供应商与 Key 都在里面——搬过来就省掉了一条条手抄 base_url。
 * 面板本身与设置页那张卡是同一个（components/CcSwitchImport.tsx）。
 */
function MigrateStep({
  imported,
  onImported,
  onNext,
}: {
  imported: boolean;
  onImported: () => void;
  onNext: () => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <div className="flex items-center gap-3">
        <IconChip icon={Download} tone="blue" />
        <div>
          <h2 className="text-[17px] font-semibold tracking-tight text-ink">
            {t("wizard.migrate.title")}
          </h2>
          <p className="text-[13px] text-muted">{t("wizard.migrate.desc")}</p>
        </div>
      </div>
      <div className="mt-5">
        <CcSwitchImportPanel onImported={onImported} />
      </div>
      {/*
        搬之前，这一屏的主行动是卡片里那颗「导入」。底下再放一颗同样醒目的
        「下一步」，人多半会点右下角最显眼的那个，一路点过去就把迁移错过了——
        而向导只走一次。所以未搬时它退成次要按钮，并把话说明白：不搬也能走。
      */}
      <div className="mt-6 flex justify-end">
        <Button variant={imported ? "primary" : undefined} size="sm" onClick={onNext}>
          {imported ? t("wizard.next") : t("wizard.migrate.later")}
        </Button>
      </div>
    </>
  );
}

/**
 * 分流这一步。两条路对应两类用户：来换供应商的（多数），和来统一管 MCP 的。
 * 选完直接落到那一页，那一页自己的导览随即开讲——不再从六站的总览讲起。
 */
function RouteStep({
  onPick,
  onOverview,
}: {
  onPick: (page: PageKey) => void;
  onOverview: () => void;
}) {
  const { t } = useTranslation();
  return (
    <>
      <h2 className="text-[17px] font-semibold tracking-tight text-ink">
        {t("wizard.route.title")}
      </h2>
      <p className="mt-1.5 text-[13px] text-muted">{t("wizard.route.desc")}</p>
      <div className="mt-5 grid gap-3 sm:grid-cols-2">
        <RouteCard
          icon={Wand2}
          title={t("wizard.route.providers.title")}
          desc={t("wizard.route.providers.desc")}
          onClick={() => onPick("providers")}
        />
        <RouteCard
          icon={Server}
          title={t("wizard.route.servers.title")}
          desc={t("wizard.route.servers.desc")}
          onClick={() => onPick("mcp-servers")}
        />
      </div>
      <div className="mt-5 flex justify-center border-t border-line pt-4">
        <button
          type="button"
          onClick={onOverview}
          className="rounded px-1 py-0.5 text-[12px] text-muted transition-colors hover:text-ink"
        >
          {t("wizard.route.overview")}
        </button>
      </div>
    </>
  );
}

function RouteCard({
  icon: Icon,
  title,
  desc,
  onClick,
}: {
  icon: typeof Wand2;
  title: string;
  desc: string;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="group flex flex-col items-start rounded-xl border border-line bg-card p-4 text-left transition-colors hover:border-accent hover:bg-hover"
    >
      <IconChip icon={Icon} tone="blue" />
      <span className="mt-3 flex items-center gap-1 text-[14px] font-medium text-ink">
        {title}
        <ArrowRight
          size={14}
          strokeWidth={2}
          aria-hidden
          className="text-faint transition-transform group-hover:translate-x-0.5"
        />
      </span>
      <span className="mt-1 text-[12px] leading-relaxed text-muted">{desc}</span>
    </button>
  );
}

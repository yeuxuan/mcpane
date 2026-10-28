import { useQuery } from "@tanstack/react-query";
import { Zap } from "lucide-react";
import type { ReactNode } from "react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { HUB_MCP_URL } from "../lib/constants";
import { type ClientTypeId, ipc } from "../lib/ipc";
import { detectPlatform, PLATFORM_LABELS, type Platform, tokenEnvCommand } from "../lib/platform";
import { queryKeys } from "../lib/queryKeys";
import { cn } from "../lib/utils";
import { CodeBlock, Modal, Tag } from "./ui";

/** 平台切换器：默认选中当前系统，可查看其它平台的命令写法。 */
function PlatformSwitch({
  value,
  onChange,
}: {
  value: Platform;
  onChange: (platform: Platform) => void;
}) {
  return (
    <div className="inline-flex shrink-0 rounded-lg border border-line bg-app p-0.5">
      {(Object.keys(PLATFORM_LABELS) as Platform[]).map((platform) => (
        <button
          key={platform}
          type="button"
          aria-pressed={value === platform}
          onClick={() => onChange(platform)}
          className={cn(
            "rounded-md px-2 py-1 text-[11px] font-medium transition-colors",
            value === platform ? "bg-card text-ink shadow-sm" : "text-muted hover:text-ink",
          )}
        >
          {PLATFORM_LABELS[platform]}
        </button>
      ))}
    </div>
  );
}

/**
 * Claude Code 官方 CLI 接入命令（手动备用路径）：--scope user 写入用户级配置。
 * 单引号让 ${TOKEN_ENV} 原样落盘、由 Claude Code 运行时展开；
 * bash/zsh/PowerShell 中单引号均为字面量，命令跨平台一致。
 */
function claudeCliCommand(tokenEnv: string, comment: string): string {
  return [
    `# ${comment}`,
    `claude mcp add --scope user --transport http mcpane ${HUB_MCP_URL} --header 'Authorization: Bearer \${${tokenEnv}}'`,
  ].join("\n");
}

/** 分节：标题 + 说明 + 可选代码块 / 辅助提示 / 高亮要点。 */
function Section({
  index,
  title,
  children,
  code,
  hint,
  callout,
}: {
  index: number;
  title: string;
  children: ReactNode;
  code?: string;
  /** 代码块下方的辅助说明（小字弱化）。 */
  hint?: string;
  /** 高亮要点（如凭据自动化说明）。 */
  callout?: string;
}) {
  return (
    <section>
      <h3 className="flex items-center gap-2.5 text-sm font-semibold text-ink">
        <span className="flex h-5 w-5 shrink-0 items-center justify-center rounded-full bg-accent-soft text-[11px] font-semibold text-accent">
          {index}
        </span>
        {title}
      </h3>
      <div className="mt-1.5 space-y-2 pl-[30px]">
        <p className="text-[13px] leading-5 text-muted">{children}</p>
        {code ? <CodeBlock code={code} /> : null}
        {hint ? <p className="text-xs leading-5 text-faint">{hint}</p> : null}
        {callout ? (
          <div className="flex items-start gap-2 rounded-lg bg-accent-soft px-3 py-2.5">
            <Zap size={14} strokeWidth={2} aria-hidden className="mt-0.5 shrink-0 text-accent" />
            <p className="text-xs leading-5 text-ink">{callout}</p>
          </div>
        ) : null}
      </div>
    </section>
  );
}

/**
 * 使用说明（How To Use）：一键接入为主叙事（两种凭据方式都零手动步骤，
 * ADR-016）；手动配置降级为末尾备用节（自动注入失败时的自救路径）。
 */
export function HowToUseModal({
  clientId,
  title,
  configPath,
  tokenEnv,
  connected,
  custom,
  onClose,
}: {
  clientId: ClientTypeId;
  title: string;
  configPath: string;
  /** null 表示该客户端不支持环境变量展开（Cline，接入固定直写模式）。 */
  tokenEnv: string | null;
  connected: boolean;
  /** 用户手动添加的客户端：没有一键接入，只有"发 Token + 自己贴片段"。 */
  custom?: boolean;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [platform, setPlatform] = useState<Platform>(detectPlatform);
  // Claude Code 手动备用走官方 CLI（无需手改配置文件），不再展示合并片段。
  const useCli = clientId === "claude" && tokenEnv !== null;
  const snippet = useQuery({
    queryKey: queryKeys.configSnippet(clientId),
    queryFn: () => ipc.clientConfigSnippet(clientId),
    staleTime: Infinity,
    retry: false,
    enabled: !useCli,
  });

  return (
    <Modal title={t("howTo.title", { title })} size="lg" onClose={onClose}>
      <p className="mb-5 flex flex-wrap items-center gap-2 text-xs text-muted">
        {/* 自定义客户端的配置文件在哪，只有用户自己知道：不报接入状态，
            也不摆一个空的路径出来。 */}
        {configPath ? (
          <>
            {connected ? (
              <Tag tone="green">{t("howTo.connected")}</Tag>
            ) : (
              <Tag tone="amber">{t("howTo.notConnected")}</Tag>
            )}
            {t("howTo.configFile")}
            <code className="font-mono text-ink">{configPath}</code>
          </>
        ) : (
          t("howTo.customHint")
        )}
      </p>

      {/* 自定义客户端：MCPane 既不写它的配置文件、也没有「接入」按钮，
          下面那套"一键接入 / 变成已接入"的步骤对它每一条都是错的。 */}
      {custom ? (
        <div className="space-y-6">
          <Section index={1} title={t("howTo.custom.step1.title")}>
            {t("howTo.custom.step1.desc")}
          </Section>
          <Section index={2} title={t("howTo.custom.step2.title")}>
            {t("howTo.custom.step2.desc")}
          </Section>
          <div className="space-y-3">
            <p className="text-[13px] leading-5 text-muted">
              {t("howTo.custom.snippetDesc")}
              {snippet.isError ? (
                <span className="text-danger">{t("howTo.manual.snippetError")}</span>
              ) : null}
            </p>
            {snippet.isSuccess ? <CodeBlock code={snippet.data} /> : null}
          </div>
          <Section index={3} title={t("howTo.custom.step3.title")}>
            {t("howTo.custom.step3.desc", { title })}
          </Section>
        </div>
      ) : (
        <>
          <div className="space-y-6">
            <Section
              index={1}
              title={t("howTo.step1.title")}
              callout={
                tokenEnv
                  ? t("howTo.step1.callout.env", { tokenEnv })
                  : t("howTo.step1.callout.inline", { title })
              }
            >
              {t("howTo.step1.desc")}
            </Section>

            <Section index={2} title={t("howTo.step2.title")}>
              {t("howTo.step2.desc")}
            </Section>

            <Section
              index={3}
              title={t("howTo.step3.title")}
              code={useCli ? `# ${t("howTo.step3.codeComment")}\nclaude mcp list` : undefined}
            >
              {t("howTo.step3.desc", { title })}
              {useCli ? ` ${t("howTo.step3.cliHint")}` : null}
            </Section>
          </div>

          <div className="mt-8 border-t border-line pt-5">
            <div className="mb-3 flex flex-wrap items-center justify-between gap-3">
              <div>
                <h3 className="text-sm font-semibold text-ink">{t("howTo.manual.title")}</h3>
                <p className="mt-0.5 text-xs text-faint">{t("howTo.manual.desc")}</p>
              </div>
              {tokenEnv ? <PlatformSwitch value={platform} onChange={setPlatform} /> : null}
            </div>

            <div className="space-y-3">
              {tokenEnv ? (
                <>
                  <p className="text-[13px] leading-5 text-muted">
                    {t("howTo.manual.envIntro")}{" "}
                    <code className="font-mono text-ink">{tokenEnv}</code>
                  </p>
                  <CodeBlock code={tokenEnvCommand(platform, tokenEnv, t)} />
                  <p className="text-xs leading-5 text-faint">
                    {t("howTo.manual.envHint", { platform: PLATFORM_LABELS[platform] })}
                  </p>
                </>
              ) : (
                <p className="text-[13px] leading-5 text-muted">
                  {t("howTo.manual.inlineDesc", { title })}
                </p>
              )}

              {useCli && tokenEnv ? (
                <>
                  <p className="text-[13px] leading-5 text-muted">{t("howTo.manual.cliDesc")}</p>
                  <CodeBlock code={claudeCliCommand(tokenEnv, t("howTo.manual.cliComment"))} />
                </>
              ) : (
                <>
                  <p className="text-[13px] leading-5 text-muted">
                    {t("howTo.manual.snippetDesc")}
                    {snippet.isError ? (
                      <span className="text-danger">{t("howTo.manual.snippetError")}</span>
                    ) : null}
                  </p>
                  {snippet.isSuccess ? <CodeBlock code={snippet.data} /> : null}
                </>
              )}
            </div>
          </div>
        </>
      )}
    </Modal>
  );
}

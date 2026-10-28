import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Network, Radar, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { ipc, type ProxyConfig } from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { cn, errorText } from "../lib/utils";
import { Button, Card, Field, IconChip, SegmentedControl } from "./ui";

type Mode = ProxyConfig["mode"];

const MODES: Mode[] = ["system", "direct", "manual"];

/**
 * 全局出网代理设置（ADR-017）。
 *
 * 只管 MCPane 自己出网（连通性测试 / 额度查询 / HTTP 下游 MCP Server），
 * 不改客户端配置——Claude Code / Codex 走不走代理由它们自己决定。
 *
 * 「扫描」直接给出实测可用的地址：本机开着的端口未必是代理（8080 多半是
 * 本地 dev server），只探 TCP 通不通会把它们一并列出来，用户点了才发现不通。
 */
export function ProxySettings() {
  const { t } = useTranslation();
  const client = useQueryClient();

  const proxy = useQuery({ queryKey: queryKeys.proxy, queryFn: ipc.proxyGet });
  const [mode, setMode] = useState<Mode>("system");
  const [url, setUrl] = useState("");
  /** 保存/测试的即时反馈。null = 无反馈。 */
  const [notice, setNotice] = useState<{ ok: boolean; text: string } | null>(null);

  // useCallback 不是为了性能，是为了让下面的 effect 能如实列出依赖又不多跑：
  // setState 恒定 ⇒ 这个引用恒定 ⇒ effect 仍只在 proxy.data 变化时触发。
  // 写成普通函数的话每次渲染都是新引用，effect 每渲染必跑，会把用户正在输入的内容冲掉。
  const applyToForm = useCallback((config: ProxyConfig) => {
    setMode(config.mode);
    setUrl(config.mode === "manual" ? config.url : "");
  }, []);

  // 服务端数据到达后灌进本地表单。
  useEffect(() => {
    if (!proxy.data) return;
    applyToForm(proxy.data);
  }, [proxy.data, applyToForm]);

  const save = useMutation({
    mutationFn: (config: ProxyConfig) => ipc.proxySet(config),
    onSuccess: () => {
      setNotice({ ok: true, text: t("settings.proxy.saved") });
      client.invalidateQueries({ queryKey: queryKeys.proxy });
    },
    onError: (error: unknown) => {
      setNotice({ ok: false, text: errorText(error) });
      // 保存失败时把表单拉回服务端的真实值：否则按钮仍高亮在没存进去的那个
      // 模式上，界面显示的与实际生效的对不上。
      //
      // 必须直接回滚，不能靠 invalidateQueries 触发上面的 effect —— react-query
      // 的结构共享会在数据未变时保留旧引用，effect 根本不会重跑。
      if (proxy.data) applyToForm(proxy.data);
    },
  });

  const scan = useMutation({
    mutationFn: () => ipc.proxyScan(),
    onSuccess: (found) => {
      // 实测最快的那个直接填上：结果已按延迟排序，且个个都验证过。
      const best = found[0];
      if (!best) {
        setNotice({ ok: false, text: t("settings.proxy.scanEmpty") });
        return;
      }
      setMode("manual");
      setUrl(best.url);
      setNotice({ ok: true, text: t("settings.proxy.scanFound", { count: found.length }) });
    },
    onError: (error) => {
      setNotice({ ok: false, text: t("settings.proxy.scanFailed", { error: errorText(error) }) });
    },
  });

  const test = useMutation({
    mutationFn: () => ipc.proxyTest(url),
    onSuccess: (result) => {
      setNotice(
        result.ok
          ? { ok: true, text: t("settings.proxy.testOk", { ms: result.latencyMs }) }
          : { ok: false, text: t("settings.proxy.testFailed", { error: result.error ?? "" }) },
      );
    },
    onError: (error) => {
      setNotice({ ok: false, text: t("settings.proxy.testFailed", { error: errorText(error) }) });
    },
  });

  const busy = save.isPending || scan.isPending || test.isPending;
  const trimmed = url.trim();
  // 表单相对已保存值是否有改动：没改动就没什么可存的，保存按钮置灰给出信号
  // （对齐 cc-switch GlobalProxySettings 的 dirty 判定，避免无意义的重复落库）。
  const dirty = proxy.data?.mode !== "manual" || proxy.data.url !== trimmed;

  const handleModeChange = (next: Mode) => {
    setMode(next);
    setNotice(null);
    // 非 manual 无需地址，直接落库；manual 要等用户填完按保存。
    if (next !== "manual") save.mutate({ mode: next });
  };

  return (
    <Card className="mb-5 p-5">
      <div className="flex items-center gap-3">
        <IconChip icon={Network} tone="slate" />
        <div className="flex-1">
          <p className="text-sm font-medium text-ink">{t("settings.proxy.title")}</p>
          <p className="text-xs text-muted">{t("settings.proxy.desc")}</p>
        </div>
        {/* 必须传 label：本页另有一处「跟随系统」（外观），两个同名项对读屏用户
            无法区分，靠卡片标题区分只对视力用户有效。 */}
        <SegmentedControl
          label={t("settings.proxy.title")}
          value={mode}
          options={MODES.map((item) => ({ value: item, label: t(`settings.proxy.mode.${item}`) }))}
          onChange={handleModeChange}
          disabled={busy}
        />
      </div>

      {mode === "manual" ? (
        <div className="mt-4 flex items-center gap-2">
          <Field
            value={url}
            onChange={(event) => {
              setUrl(event.target.value);
              setNotice(null);
            }}
            placeholder="http://127.0.0.1:7897"
            aria-label={t("settings.proxy.urlLabel")}
            spellCheck={false}
            className="flex-1 font-mono"
          />
          <Button
            variant="ghost"
            icon={Radar}
            loading={scan.isPending}
            onClick={() => scan.mutate()}
            disabled={busy}
            title={t("settings.proxy.scanHint")}
          >
            {t("settings.proxy.scan")}
          </Button>
          <Button
            variant="ghost"
            loading={test.isPending}
            onClick={() => test.mutate()}
            disabled={busy || !trimmed}
          >
            {t("settings.proxy.test")}
          </Button>
          <Button
            variant="primary"
            loading={save.isPending}
            onClick={() => save.mutate({ mode: "manual", url: trimmed })}
            disabled={busy || !trimmed || !dirty}
          >
            {t("settings.proxy.save")}
          </Button>
        </div>
      ) : null}

      {notice ? (
        <p
          role="status"
          className={cn(
            "mt-3 flex items-center gap-1.5 text-xs",
            notice.ok ? "text-run" : "text-danger",
          )}
        >
          {notice.ok ? (
            <Check aria-hidden className="h-3.5 w-3.5 shrink-0" />
          ) : (
            <TriangleAlert aria-hidden className="h-3.5 w-3.5 shrink-0" />
          )}
          <span className="min-w-0 break-all">{notice.text}</span>
        </p>
      ) : null}
    </Card>
  );
}

import { zodResolver } from "@hookform/resolvers/zod";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Archive, CloudUpload, Database, Download, RotateCcw, Trash2, Upload } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useForm } from "react-hook-form";
import { useTranslation } from "react-i18next";
import { z } from "zod";
import { formatAt } from "../lib/datetime";
import {
  type DataOverview,
  ipc,
  type RemoteInfo,
  type RetentionView,
  type SyncSettingsInput,
} from "../lib/ipc";
import { queryKeys } from "../lib/queryKeys";
import { errorText } from "../lib/utils";
import { ConfirmDialog } from "./ConfirmDialog";
import {
  Button,
  Card,
  EmptyState,
  ErrorBanner,
  Field,
  IconButton,
  IconChip,
  InlineNote,
  LabeledField,
  SegmentedControl,
  Switch,
} from "./ui";

function fmtSize(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

// ---------- 共享：操作结果 ----------

type ActionNote = { tone: "ok" | "error"; text: string } | null;

/**
 * 按钮旁那句「已保存 / 清理失败」的状态。
 *
 * 成功 4 秒后自净：久留的「备份完成」会给后面那次失败的操作背书——用户看到
 * 绿字还在，以为刚点的那下也成了。失败则留着，等用户处理完再被下一次覆盖。
 */
function useActionNote() {
  const [note, setNote] = useState<ActionNote>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const show = useCallback((tone: "ok" | "error", text: string) => {
    setNote({ tone, text });
    clearTimeout(timer.current);
    if (tone === "ok") timer.current = setTimeout(() => setNote(null), 4000);
  }, []);
  return [note, show] as const;
}

// ---------- 本机数据（保留窗口 + 就地清理） ----------

/**
 * 保留窗口条：一条时间轴，左端是最早一条记录、右端是现在，颜色分界处就是
 * 保留期的起点——灰的那截是下次清理会没掉的数据。
 *
 * 它画的是**时间比例**而不是数量比例（记录在时间上不均匀分布，按数量画会
 * 谎报"删掉的是这么长一段时间"）。所以两端各压一个日期刻度：不标日期它会被
 * 读成进度条。具体条数另用文字给出，两种口径各说各的、都不冒充对方。
 */
function RetentionBar({ view, now }: { view: RetentionView; now: number }) {
  const { t, i18n } = useTranslation();
  const oldest = view.oldestTs;

  if (view.rows === 0 || oldest === null) {
    return (
      <div className="mt-2.5">
        <div aria-hidden className="h-1 w-full rounded-full bg-slot" />
        <p className="mt-2 text-[11px] text-faint">{t("settings.data.noRecords")}</p>
      </div>
    );
  }

  const cutoff = now - view.retainDays * 86400;
  const span = now - oldest;
  // 最早一条已在保留期内 → 整条都是"保留中"，无灰段。
  const expired = span <= 0 ? 0 : Math.min(1, Math.max(0, (cutoff - oldest) / span));

  return (
    <div className="mt-2.5">
      {/* 4px 高、保留段用半透明主色：满格的实心蓝会被读成"进度 100%"，而这里
          没有任何进度可言——它是一根时间刻度线，得像刻度线那样克制。 */}
      <div aria-hidden className="relative h-1 w-full overflow-hidden rounded-full bg-accent/45">
        <span
          className="absolute inset-y-0 left-0 bg-line-strong"
          style={{ width: `${expired * 100}%` }}
        />
      </div>
      <div className="mt-2 flex items-center justify-between text-[11px] text-faint tabular-nums">
        <span>{formatAt(i18n.language, "day", oldest)}</span>
        <span>{t("settings.data.now")}</span>
      </div>
    </div>
  );
}

function RetentionRow({
  label,
  view,
  loading,
  note,
  onPrune,
  now,
}: {
  label: string;
  view: RetentionView;
  loading: boolean;
  note: ActionNote;
  onPrune: () => void;
  now: number;
}) {
  const { t } = useTranslation();
  return (
    <div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <span className="font-medium text-[13px] text-ink">{label}</span>
        <span className="text-faint text-xs tabular-nums">
          {t("settings.data.rows", { count: view.rows })} ·{" "}
          {t("settings.data.retain", { days: view.retainDays })}
        </span>
        <span className="ml-auto flex min-w-0 items-center gap-3">
          {note ? <InlineNote tone={note.tone}>{note.text}</InlineNote> : null}
          {/* 按钮文案带上条数：点下去会发生什么，写在按钮上而不是留给用户猜。 */}
          <Button
            size="sm"
            loading={loading}
            disabled={view.prunable === 0}
            onClick={onPrune}
            icon={Trash2}
          >
            {view.prunable > 0
              ? t("settings.data.prune", { count: view.prunable })
              : t("settings.data.pruneNone")}
          </Button>
        </span>
      </div>
      <RetentionBar view={view} now={now} />
    </div>
  );
}

/**
 * 本机数据：日志与用量两张按时间增长的表，各自的保留窗口与就地清理。
 *
 * 此前这里是两张几乎一模一样的卡片，各写一句「清理 N 天前的…」加一个按钮——
 * 用户既看不出攒了多少，也看不出点下去会掉多少，更不知道系统其实每次启动都
 * 自己清一遍。现在把"有多少 / 最早到什么时候 / 会清掉多少"摆出来，天数也
 * 由后端回传（与启动自动清理同一个常量），界面不再有解释不了的魔数。
 */
export function RetentionSection() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [logNote, showLogNote] = useActionNote();
  const [usageNote, showUsageNote] = useActionNote();

  const overview = useQuery({
    queryKey: queryKeys.dataOverview,
    queryFn: ipc.dataOverview,
    retry: false,
  });
  const data: DataOverview | undefined = overview.data;
  // 保留窗口条的右端是"现在"，取本次查询回来的时刻——与条上那批数据同一时间基准，
  // 用当前墙上时间会让久开不动的页面把分界线越推越右，和后端算出的 prunable 对不上。
  const now = Math.floor((overview.dataUpdatedAt || Date.now()) / 1000);

  const prune = useMutation({
    mutationFn: ({ kind, days }: { kind: "logs" | "usage"; days: number }) =>
      kind === "logs" ? ipc.logsPrune(days) : ipc.usagePrune(days),
    onSuccess: (deleted, { kind }) => {
      (kind === "logs" ? showLogNote : showUsageNote)(
        "ok",
        t("settings.data.pruned", { count: deleted }),
      );
      void queryClient.invalidateQueries({ queryKey: queryKeys.dataOverview });
      // 日志/用量页看的是同一批行，清完要跟着刷新，否则那边还列着已删的记录。
      void queryClient.invalidateQueries({ queryKey: queryKeys.logStatsAll });
      void queryClient.invalidateQueries({ queryKey: queryKeys.usageAll });
    },
    onError: (error, { kind }) =>
      (kind === "logs" ? showLogNote : showUsageNote)(
        "error",
        t("common.actionFailed", { error: errorText(error) }),
      ),
  });
  const pruning = (kind: "logs" | "usage") => prune.isPending && prune.variables?.kind === kind;

  return (
    <Card className="mb-5 p-5">
      <div className="flex items-center gap-3">
        <IconChip icon={Database} tone="slate" />
        <div className="min-w-0 flex-1">
          <p className="font-medium text-ink text-sm">{t("settings.data.title")}</p>
          <p className="text-muted text-xs">{t("settings.data.desc")}</p>
        </div>
        {data ? (
          <span className="shrink-0 text-right">
            <span className="block font-semibold text-[15px] text-ink tabular-nums">
              {fmtSize(data.dbBytes)}
            </span>
            <span className="block text-[11px] text-faint">{t("settings.data.dbLabel")}</span>
          </span>
        ) : null}
      </div>

      {overview.isError ? (
        // 读不到就说读不到，不摆一排 0 冒充"干净"。
        <p className="mt-4 border-line border-t pt-4 text-danger text-xs">
          {t("settings.data.overviewFailed", { error: errorText(overview.error) })}
        </p>
      ) : data ? (
        // 数据到了才画：条数与保留期都来自后端，拿不到就一个数都不写——
        // 用 0 兜底会在加载的那一瞬间闪出「0 条 · 保留最近 0 天」这种假话。
        <div className="mt-4 space-y-6 border-line border-t pt-4">
          <RetentionRow
            label={t("settings.data.logs")}
            view={data.logs}
            loading={pruning("logs")}
            note={logNote}
            now={now}
            onPrune={() => prune.mutate({ kind: "logs", days: data.logs.retainDays })}
          />
          <RetentionRow
            label={t("settings.data.usage")}
            view={data.usage}
            loading={pruning("usage")}
            note={usageNote}
            now={now}
            onPrune={() => prune.mutate({ kind: "usage", days: data.usage.retainDays })}
          />
        </div>
      ) : null}
    </Card>
  );
}

// ---------- 本地备份 ----------

export function BackupSection() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const [note, showNote] = useActionNote();
  const [confirmRestore, setConfirmRestore] = useState<string | null>(null);
  /* 删除比恢复更需要拦一道：恢复挑错了还能再恢复另一份，删掉的备份文件找不回来。
   * 此前只有恢复有确认框，保护级别正好装反了。 */
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const backups = useQuery({
    queryKey: queryKeys.backups,
    queryFn: ipc.backupList,
    retry: false,
  });
  const entries = backups.data ?? [];

  const create = useMutation({
    mutationFn: ipc.backupCreate,
    onSuccess: () => {
      showNote("ok", t("settings.backup.created"));
      queryClient.invalidateQueries({ queryKey: queryKeys.backups });
    },
    onError: (error) => showNote("error", t("common.actionFailed", { error: errorText(error) })),
  });
  const restore = useMutation({
    mutationFn: ipc.backupRestore,
    onSuccess: () => {
      showNote("ok", t("settings.backup.restored"));
      // 业务数据整体变更：全量失效。
      queryClient.invalidateQueries();
    },
    onError: (error) => showNote("error", t("common.actionFailed", { error: errorText(error) })),
  });
  const remove = useMutation({
    mutationFn: ipc.backupDelete,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: queryKeys.backups }),
    onError: (error) => showNote("error", t("common.actionFailed", { error: errorText(error) })),
  });

  return (
    <Card className="mb-5 p-5">
      <div className="flex items-center gap-3">
        <IconChip icon={Archive} tone="slate" />
        <div className="min-w-0 flex-1">
          <p className="font-medium text-ink text-sm">{t("settings.backup.title")}</p>
          <p className="text-muted text-xs">{t("settings.backup.desc")}</p>
        </div>
        {note ? <InlineNote tone={note.tone}>{note.text}</InlineNote> : null}
        <Button loading={create.isPending} onClick={() => create.mutate()}>
          {t("settings.backup.createNow")}
        </Button>
      </div>

      <div className="mt-4 border-line border-t pt-4">
        {entries.length > 0 ? (
          // 分隔线而不是一行一个描边盒：十份备份摞起来就是十圈边框，页面会发灰。
          <div className="divide-y divide-line">
            {entries.map((entry, index) => (
              <div key={entry.fileName} className="flex items-center gap-3 py-2 text-xs">
                <span className="text-ink tabular-nums">
                  {formatAt(i18n.language, "time", entry.createdAt)}
                </span>
                {/* 最新一份要一眼认得出——恢复时找的通常就是它。 */}
                {index === 0 ? (
                  <span className="text-[11px] text-accent">{t("settings.backup.latest")}</span>
                ) : null}
                {/* 文件名（mcpane-backup-<unix 秒>.db）是这份备份在磁盘上的身份，
                    要留着——但它读出来只是一串数字，故退到末位、走 faint 等宽。 */}
                <span className="min-w-0 flex-1 truncate text-right font-mono text-faint">
                  {entry.fileName}
                </span>
                <span className="shrink-0 text-faint tabular-nums">{fmtSize(entry.sizeBytes)}</span>
                <IconButton
                  icon={RotateCcw}
                  aria-label={t("settings.backup.restoreOne", {
                    time: formatAt(i18n.language, "time", entry.createdAt),
                  })}
                  title={t("settings.backup.restore")}
                  onClick={() => setConfirmRestore(entry.fileName)}
                />
                <IconButton
                  icon={Trash2}
                  tone="danger"
                  aria-label={t("settings.backup.deleteOne", {
                    time: formatAt(i18n.language, "time", entry.createdAt),
                  })}
                  title={t("settings.backup.delete")}
                  onClick={() => setConfirmDelete(entry.fileName)}
                />
              </div>
            ))}
          </div>
        ) : (
          <EmptyState
            title={t("settings.backup.emptyTitle")}
            description={t("settings.backup.emptyDesc")}
          />
        )}
        {/* 定时备份是常驻行为（main.rs 每 6h 复查、超 24h 补一份、留最近 10 份），
            此前完全没告诉用户——他们会以为不手动点就一份都没有。 */}
        <p className="mt-3 text-[11px] text-faint">{t("settings.backup.autoNote")}</p>
      </div>

      {confirmRestore ? (
        <ConfirmDialog
          title={t("settings.backup.restoreTitle")}
          message={t("settings.backup.restoreConfirm")}
          confirmText={t("settings.backup.restore")}
          onConfirm={() => {
            restore.mutate(confirmRestore);
            setConfirmRestore(null);
          }}
          onCancel={() => setConfirmRestore(null)}
        />
      ) : null}

      {confirmDelete ? (
        <ConfirmDialog
          title={t("settings.backup.deleteTitle")}
          message={t("settings.backup.deleteConfirm")}
          confirmText={t("settings.backup.delete")}
          onConfirm={() => {
            remove.mutate(confirmDelete);
            setConfirmDelete(null);
          }}
          onCancel={() => setConfirmDelete(null)}
        />
      ) : null}
    </Card>
  );
}

// ---------- 云同步 ----------

const SYNC_PROVIDERS = ["off", "webdav", "s3"] as const;
type SyncProvider = (typeof SYNC_PROVIDERS)[number];

/**
 * 把后端回来的 provider 收敛成三选一，未知一律当「关闭」。
 *
 * 后端出口已经归一（commands/sync.rs 的 normalized_provider），这里再兜一道是
 * 因为 IPC 传回来的东西在运行时本质是 unknown，而这个字段一旦落在三个值之外，
 * 分段器就一项都不高亮、配置区又当作"已启用"展开——界面上没有任何信号，
 * 用户读不出当前到底开没开。代价极高、兜底极便宜的一类边界。
 */
function normalizeProvider(raw: string): SyncProvider {
  return (SYNC_PROVIDERS as readonly string[]).includes(raw) ? (raw as SyncProvider) : "off";
}

/** 表单结构（校验在组件内 superRefine：必填与格式依赖当前 provider 与已存凭据）。 */
const syncFormShape = z.object({
  provider: z.enum(SYNC_PROVIDERS),
  autoSync: z.boolean(),
  webdav: z.object({
    baseUrl: z.string(),
    username: z.string(),
    remoteRoot: z.string(),
    password: z.string(),
  }),
  s3: z.object({
    region: z.string(),
    bucket: z.string(),
    accessKeyId: z.string(),
    endpoint: z.string(),
    remoteRoot: z.string(),
    secretKey: z.string(),
  }),
});
type SyncFormValues = z.infer<typeof syncFormShape>;

function buildInput(values: SyncFormValues): SyncSettingsInput {
  return {
    provider: values.provider,
    autoSync: values.autoSync,
    webdav: {
      baseUrl: values.webdav.baseUrl,
      username: values.webdav.username,
      remoteRoot: values.webdav.remoteRoot,
      password: values.webdav.password || null,
    },
    s3: {
      region: values.s3.region,
      bucket: values.s3.bucket,
      accessKeyId: values.s3.accessKeyId,
      endpoint: values.s3.endpoint,
      remoteRoot: values.s3.remoteRoot,
      secretKey: values.s3.secretKey || null,
    },
  };
}

export function CloudSyncSection() {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const settings = useQuery({
    queryKey: queryKeys.syncSettings,
    queryFn: ipc.syncSettingsGet,
    retry: false,
  });
  const data = settings.data;

  const [note, showNote] = useActionNote();
  const [confirmAction, setConfirmAction] = useState<{
    kind: "upload" | "download";
    remote: RemoteInfo;
    /** 云端已被其他设备更新（≠ 本机上次同步基线）：确认即强制覆盖。 */
    conflict: boolean;
  } | null>(null);

  // 按 provider 条件校验；凭据已存（has*）时留空表示沿用，首次必填。
  const schema = useMemo(
    () =>
      syncFormShape.superRefine((form, ctx) => {
        const required = (path: string[]) =>
          ctx.addIssue({ code: "custom", path, message: t("validation.required") });
        const httpUrl = (path: string[]) =>
          ctx.addIssue({ code: "custom", path, message: t("validation.httpUrl") });
        if (form.provider === "webdav") {
          const baseUrl = form.webdav.baseUrl.trim();
          if (!baseUrl) required(["webdav", "baseUrl"]);
          else if (!/^https?:\/\//i.test(baseUrl)) httpUrl(["webdav", "baseUrl"]);
          if (!form.webdav.username.trim()) required(["webdav", "username"]);
          if (!data?.webdav.hasPassword && !form.webdav.password) {
            required(["webdav", "password"]);
          }
        }
        if (form.provider === "s3") {
          if (!form.s3.region.trim()) required(["s3", "region"]);
          if (!form.s3.bucket.trim()) required(["s3", "bucket"]);
          if (!form.s3.accessKeyId.trim()) required(["s3", "accessKeyId"]);
          if (!data?.s3.hasSecretKey && !form.s3.secretKey) required(["s3", "secretKey"]);
          const endpoint = form.s3.endpoint.trim();
          if (endpoint.includes("://") && !/^https?:\/\//i.test(endpoint)) {
            httpUrl(["s3", "endpoint"]);
          }
        }
      }),
    [t, data],
  );

  const {
    register,
    handleSubmit,
    watch,
    setValue,
    getValues,
    clearErrors,
    formState: { errors },
  } = useForm<SyncFormValues>({
    resolver: zodResolver(schema),
    // values 因外部 refetch 变化时保留用户未保存的输入，不静默清空。
    resetOptions: { keepDirtyValues: true },
    defaultValues: {
      provider: "off",
      autoSync: false,
      webdav: { baseUrl: "", username: "", remoteRoot: "", password: "" },
      s3: { region: "", bucket: "", accessKeyId: "", endpoint: "", remoteRoot: "", secretKey: "" },
    },
    // 服务端数据落表单（密码字段恒空——留空即沿用）；保存后随 refetch 复位。
    values: data
      ? {
          provider: normalizeProvider(data.provider),
          autoSync: data.autoSync,
          webdav: {
            baseUrl: data.webdav.baseUrl,
            username: data.webdav.username,
            remoteRoot: data.webdav.remoteRoot,
            password: "",
          },
          s3: {
            region: data.s3.region,
            bucket: data.s3.bucket,
            accessKeyId: data.s3.accessKeyId,
            endpoint: data.s3.endpoint,
            remoteRoot: data.s3.remoteRoot,
            secretKey: "",
          },
        }
      : undefined,
  });
  const provider = watch("provider");

  const save = useMutation({
    mutationFn: (values: SyncFormValues) => ipc.syncSettingsSet(buildInput(values)),
    onSuccess: () => {
      showNote("ok", t("settings.sync.saved"));
      queryClient.invalidateQueries({ queryKey: queryKeys.syncSettings });
    },
    onError: (error) => showNote("error", t("common.actionFailed", { error: errorText(error) })),
  });
  const test = useMutation({
    mutationFn: async (values: SyncFormValues) => {
      await ipc.syncSettingsSet(buildInput(values));
      await ipc.syncTest();
    },
    onSuccess: () => {
      showNote("ok", t("settings.sync.testOk"));
      queryClient.invalidateQueries({ queryKey: queryKeys.syncSettings });
    },
    onError: (error) => showNote("error", t("common.actionFailed", { error: errorText(error) })),
  });
  const prepare = useMutation({
    // 刻意合流「保存 + 探测」：远端探测必须用当前表单的配置/凭据，先落库
    // 再探测。因此即使用户在后续确认框点了取消，配置也已保存——这是
    // 预期行为（等同先点保存再点上传）。
    mutationFn: async (input: { kind: "upload" | "download"; values: SyncFormValues }) => {
      await ipc.syncSettingsSet(buildInput(input.values));
      const remote = await ipc.syncRemoteInfo();
      return { kind: input.kind, remote };
    },
    onSuccess: (next) => {
      queryClient.invalidateQueries({ queryKey: queryKeys.syncSettings });
      if (next.kind === "download" && !next.remote.exists) {
        showNote("error", t("settings.sync.noRemote"));
        return;
      }
      if (next.kind === "download" && next.remote.incompatible) {
        showNote("error", next.remote.incompatible);
        return;
      }
      // 上传前预判并发冲突：云端快照 ≠ 本机上次同步基线 → 冲突话术 + 强制语义。
      const conflict =
        next.kind === "upload" &&
        next.remote.exists &&
        next.remote.snapshotId !== (data?.status.lastSnapshotId ?? null);
      setConfirmAction({ ...next, conflict });
    },
    onError: (error) => showNote("error", t("common.actionFailed", { error: errorText(error) })),
  });
  const run = useMutation({
    mutationFn: async (input: { kind: "upload" | "download"; force: boolean }) => {
      if (input.kind === "download") {
        await ipc.syncDownload();
        return null;
      }
      return ipc.syncUpload(input.force);
    },
    onSuccess: (result, input) => {
      // 上传竞态兜底：预判无冲突但引擎检出（对话框开着期间别的设备写入）。
      if (result?.conflict) {
        setConfirmAction({
          kind: "upload",
          remote: {
            exists: true,
            deviceName: result.conflict.deviceName,
            createdAt: result.conflict.createdAt,
            snapshotId: result.conflict.snapshotId,
            incompatible: null,
          },
          conflict: true,
        });
        return;
      }
      showNote(
        "ok",
        t(input.kind === "upload" ? "settings.sync.uploaded" : "settings.sync.downloaded"),
      );
      if (input.kind === "download") {
        queryClient.invalidateQueries();
      } else {
        queryClient.invalidateQueries({ queryKey: queryKeys.syncSettings });
      }
    },
    onError: (error) => showNote("error", t("common.actionFailed", { error: errorText(error) })),
  });

  const busy = save.isPending || test.isPending || prepare.isPending || run.isPending;
  const status = data?.status;
  const autoSync = watch("autoSync");
  // 抬头第二行：关着时讲这功能是干什么的，开着时讲它现在什么状况。同一个位置
  // 两种职责——用户在"要不要开"和"开了怎么样"之间，同一时刻只关心其中一个。
  const headline =
    provider === "off"
      ? t("settings.sync.desc")
      : status?.lastSyncAt
        ? t("settings.sync.summaryAt", {
            time: formatAt(i18n.language, "time", status.lastSyncAt),
            device: status.lastDeviceName ?? "-",
          })
        : t("settings.sync.summaryNever");

  return (
    <Card className="mb-5 p-5">
      {/* 同步方式挪到抬头右侧，与「语言」「外观」那几张卡同构：关着的时候这张卡
          就是一张标准的单行设置卡，而不是一个孤零零悬在卡里的分段器加一片空白。
          选了 WebDAV / S3 才向下展开配置区。 */}
      <div className="flex items-center gap-3">
        <IconChip icon={CloudUpload} tone="slate" />
        <div className="min-w-0 flex-1">
          <p className="font-medium text-ink text-sm">{t("settings.sync.title")}</p>
          <p className="text-muted text-xs">{headline}</p>
        </div>
        {/* 每处反馈都挨着触发它的控件：关掉同步是分段器这一下触发的，结果就显示在
            分段器旁边；展开态那些显式按钮的结果则显示在按钮那一行。 */}
        {provider === "off" && note ? <InlineNote tone={note.tone}>{note.text}</InlineNote> : null}
        <SegmentedControl
          label={t("settings.sync.provider")}
          value={provider}
          options={[
            { value: "off", label: t("settings.sync.off") },
            { value: "webdav", label: "WebDAV" },
            { value: "s3", label: "S3" },
          ]}
          onChange={(next) => {
            setValue("provider", next);
            // 换 provider 即清报错：旧 provider 的错误不留到切回时再现。
            clearErrors();
            /*
             * 「关闭」不需要再填任何东西，就地落库——与代理设置里「跟随系统 /
             * 强制直连」同一条规则（需要补输入的才等用户按保存）。
             *
             * 不落库的话：保存按钮只在选了 WebDAV/S3 时才渲染，于是选「关闭」
             * 永远存不进去。界面看着已经关了（配置区都收起来了），库里还是
             * WebDAV，自动同步照跑，下次打开设置又变回去。
             *
             * 不走 handleSubmit：关掉同步不该被上一个 provider 那张填了一半的
             * 表单的必填校验挡住。字段原样带上，切回来时配置还在。
             */
            if (next === "off") {
              save.mutate(
                { ...getValues(), provider: "off" },
                {
                  // 落库失败就把选择器拨回库里的真实值：否则界面停在「关闭」、
                  // 配置区收起，而实际还开着、自动同步照跑——又是一次"用户读不出
                  // 当前是什么状态"。红字提示与真实选中项要一起给。
                  onError: () => setValue("provider", normalizeProvider(data?.provider ?? "off")),
                },
              );
            }
          }}
        />
      </div>

      {provider === "off" ? null : (
        <div className="mt-4 space-y-4 border-line border-t pt-4">
          {/* 上次同步的报错顶在最前面：它是这一段最重要的状态，压在底部当 11px 灰字
            等于没说。ErrorBanner 带整段复制——同步错误常含远端地址与响应体。 */}
          {status?.lastError ? (
            <ErrorBanner
              message={
                status.lastErrorSource === "auto"
                  ? t("settings.sync.autoError", { error: status.lastError })
                  : status.lastError
              }
            />
          ) : null}

          {provider === "webdav" ? (
            <div className="grid grid-cols-2 gap-3">
              <LabeledField
                label={t("settings.sync.webdavUrl")}
                required
                error={errors.webdav?.baseUrl?.message}
              >
                <Field
                  {...register("webdav.baseUrl")}
                  placeholder="https://dav.jianguoyun.com/dav/mcpane"
                />
              </LabeledField>
              <LabeledField
                label={t("settings.sync.username")}
                required
                error={errors.webdav?.username?.message}
              >
                <Field {...register("webdav.username")} />
              </LabeledField>
              <LabeledField
                label={t("settings.sync.password")}
                required={!data?.webdav.hasPassword}
                error={errors.webdav?.password?.message}
              >
                <Field
                  {...register("webdav.password")}
                  type="password"
                  placeholder={
                    data?.webdav.hasPassword ? t("settings.sync.credentialSaved") : undefined
                  }
                />
              </LabeledField>
              <LabeledField label={t("settings.sync.remoteRoot")}>
                <Field {...register("webdav.remoteRoot")} placeholder="mcpane-sync" />
              </LabeledField>
            </div>
          ) : null}

          {provider === "s3" ? (
            <div className="grid grid-cols-2 gap-3">
              <LabeledField label="Region" required error={errors.s3?.region?.message}>
                <Field {...register("s3.region")} placeholder="us-east-1" />
              </LabeledField>
              <LabeledField label="Bucket" required error={errors.s3?.bucket?.message}>
                <Field {...register("s3.bucket")} />
              </LabeledField>
              <LabeledField label="Access Key ID" required error={errors.s3?.accessKeyId?.message}>
                <Field {...register("s3.accessKeyId")} />
              </LabeledField>
              <LabeledField
                label="Secret Access Key"
                required={!data?.s3.hasSecretKey}
                error={errors.s3?.secretKey?.message}
              >
                <Field
                  {...register("s3.secretKey")}
                  type="password"
                  placeholder={
                    data?.s3.hasSecretKey ? t("settings.sync.credentialSaved") : undefined
                  }
                />
              </LabeledField>
              <LabeledField
                label={t("settings.sync.endpoint")}
                error={errors.s3?.endpoint?.message}
              >
                <Field {...register("s3.endpoint")} placeholder={t("settings.sync.endpointHint")} />
              </LabeledField>
              <LabeledField label={t("settings.sync.remoteRoot")}>
                <Field {...register("s3.remoteRoot")} placeholder="mcpane-sync" />
              </LabeledField>
            </div>
          ) : null}

          {/* 裸 checkbox 换成 iOS 开关：全应用的"开/关"都是这一个语汇，
                只有这里长得像网页表单，扫过去会以为它不是同一类东西。 */}
          <div className="flex items-center gap-3 border-line border-t pt-4">
            <span className="min-w-0 flex-1 text-[13px] text-ink">
              {t("settings.sync.autoSync")}
            </span>
            <Switch
              checked={autoSync}
              label={t("settings.sync.autoSync")}
              onChange={(next) => setValue("autoSync", next, { shouldDirty: true })}
            />
          </div>

          {/* 左组是"配"（改的是设置），右组是"搬"（动的是数据，且都要过确认框）。
                四个按钮此前排成一排，配置与搬运混在一起，"从云端恢复"看着和
                "保存"一样人畜无害。 */}
          <div className="flex flex-wrap items-center gap-2">
            <Button
              disabled={busy}
              loading={test.isPending}
              onClick={handleSubmit((values) => test.mutate(values))}
            >
              {t("settings.sync.test")}
            </Button>
            <Button
              disabled={busy}
              loading={save.isPending}
              onClick={handleSubmit((values) => save.mutate(values))}
            >
              {t("settings.sync.save")}
            </Button>
            <span className="ml-auto flex flex-wrap items-center gap-2">
              <Button
                icon={Upload}
                disabled={busy}
                loading={prepare.isPending || run.isPending}
                onClick={handleSubmit((values) => prepare.mutate({ kind: "upload", values }))}
              >
                {t("settings.sync.upload")}
              </Button>
              <Button
                icon={Download}
                disabled={busy}
                onClick={handleSubmit((values) => prepare.mutate({ kind: "download", values }))}
              >
                {t("settings.sync.download")}
              </Button>
            </span>
          </div>
          {note ? <InlineNote tone={note.tone}>{note.text}</InlineNote> : null}

          <p className="text-[11px] text-faint">{t("settings.sync.securityNote")}</p>
        </div>
      )}

      {confirmAction ? (
        <ConfirmDialog
          title={t(
            confirmAction.kind !== "upload"
              ? "settings.sync.downloadTitle"
              : confirmAction.conflict
                ? "settings.sync.conflictTitle"
                : "settings.sync.uploadTitle",
          )}
          message={
            confirmAction.kind === "upload"
              ? confirmAction.conflict
                ? t("settings.sync.conflictMessage", {
                    device: confirmAction.remote.deviceName ?? "-",
                    time: confirmAction.remote.createdAt
                      ? formatAt(i18n.language, "time", confirmAction.remote.createdAt)
                      : "-",
                  })
                : confirmAction.remote.exists
                  ? t("settings.sync.uploadOverwriteConfirm", {
                      device: confirmAction.remote.deviceName ?? "-",
                      time: confirmAction.remote.createdAt
                        ? formatAt(i18n.language, "time", confirmAction.remote.createdAt)
                        : "-",
                    })
                  : t("settings.sync.uploadFreshConfirm")
              : t("settings.sync.downloadConfirm", {
                  device: confirmAction.remote.deviceName ?? "-",
                  time: confirmAction.remote.createdAt
                    ? formatAt(i18n.language, "time", confirmAction.remote.createdAt)
                    : "-",
                })
          }
          confirmText={t(
            confirmAction.kind !== "upload"
              ? "settings.sync.download"
              : confirmAction.conflict
                ? "settings.sync.forceUpload"
                : "settings.sync.upload",
          )}
          onConfirm={() => {
            run.mutate({ kind: confirmAction.kind, force: confirmAction.conflict });
            setConfirmAction(null);
          }}
          onCancel={() => setConfirmAction(null)}
        />
      ) : null}
    </Card>
  );
}

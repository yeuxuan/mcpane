//! 写入流水线（ADR-005）：
//! 重读 → 解析 → 哈希对比 → 最小 Patch → Diff → Snapshot →
//! 同目录临时文件 → fsync → 验证 → 原子 rename → 重读验证 → 操作历史。
//!
//! 快照脱敏注意：用于回滚的快照如果被脱敏，回滚会丢失被脱敏值。
//! 因此 `redact` 只允许剔除 MCPane 自己管理、可从 Secret Store 重新
//! 物化的字段；用户自有字段一律原样保留。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use crate::diff::{ConfigDiff, unified_diff};
use crate::document::{ConfigDocument, ConfigFormat, FileFormatting, LoadedConfig, content_hash};
use crate::error::ConfigError;
use crate::history::{HistoryStore, OperationRecord, OperationStatus};
use crate::patch::PatchPlan;
use crate::snapshot::{SnapshotMeta, SnapshotStore};

/// 业务验证回调：落盘前在内存中验证解析后的文档。
pub type Validator<'a> = &'a dyn Fn(&ConfigDocument) -> Result<(), String>;

/// 快照脱敏回调：只允许剔除可从 Secret Store 重新物化的 MCPane 管理字段。
pub type Redactor<'a> = &'a dyn Fn(&str) -> String;

/// 最近由 MCPane 自己写入的内容 hash（进程内，跨线程共享）。
///
/// 文件监听要区分「自己写的」与「外部改的」。只靠操作历史的 `hash_after`
/// 有竞态：引擎先 rename 落盘、再 append 历史，而 rename 已触发文件系统事件——
/// 事件可能在 append 之前到达，历史里查不到就把自写误报成外部改动
///（macOS FSEvents 的延迟掩盖了它，Linux inotify 会现形）。
///
/// 这里在**写盘之前**登记自写 hash（早于任何文件系统事件），文件监听优先查它、
/// 操作历史兜底（进程重启后内存清空，旧写入仍可经历史识别）。
static RECENT_OWN_WRITES: Mutex<VecDeque<(PathBuf, String, SystemTime)>> =
    Mutex::new(VecDeque::new());
const OWN_WRITE_TTL: Duration = Duration::from_secs(30);
const OWN_WRITE_CAP: usize = 64;

/// 登记一次自写（写入落盘前调用，早于任何文件系统事件）。
pub fn register_own_write(path: &Path, content_hash: &str) {
    let Ok(mut queue) = RECENT_OWN_WRITES.lock() else {
        return;
    };
    let now = SystemTime::now();
    queue.retain(|(_, _, at)| {
        now.duration_since(*at)
            .map(|d| d < OWN_WRITE_TTL)
            .unwrap_or(false)
    });
    queue.push_back((path.to_path_buf(), content_hash.to_owned(), now));
    while queue.len() > OWN_WRITE_CAP {
        queue.pop_front();
    }
}

/// 该文件是否刚被 MCPane 自己写成这个 hash（TTL 内）。
pub fn is_recent_own_write(path: &Path, content_hash: &str) -> bool {
    let Ok(queue) = RECENT_OWN_WRITES.lock() else {
        return false;
    };
    let now = SystemTime::now();
    queue.iter().any(|(p, h, at)| {
        p == path
            && h == content_hash
            && now
                .duration_since(*at)
                .map(|d| d < OWN_WRITE_TTL)
                .unwrap_or(false)
    })
}

/// 文件系统操作抽象：真实实现 + 测试用失败注入。
pub trait FsOps: Send + Sync {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String>;
    fn write(&self, path: &Path, content: &str) -> std::io::Result<()>;
    fn fsync(&self, path: &Path) -> std::io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn remove_file(&self, path: &Path) -> std::io::Result<()>;
    fn mtime_unix_nanos(&self, path: &Path) -> Option<u128>;
    fn exists(&self, path: &Path) -> bool;
    /// 把文件权限收紧为仅属主可读写（unix 0600）。凭据类文件落盘必须调用，
    /// 让 ADR-015「本机其他用户由文件权限解决」在代码层成立。
    /// 默认 no-op（供测试 mock 与非 unix 平台），真实文件系统在 `RealFs` 覆盖。
    fn harden_permissions(&self, _path: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// 目标是符号链接时解析到它指向的真实文件，否则原样返回。
    ///
    /// 原子写靠 `rename` 替换**目录项本身**：若 `~/.claude.json` 这类路径是
    /// chezmoi / GNU stow 等 dotfiles 工具建的符号链接，直接 rename 会把链接
    /// 换成普通文件——内容不丢，但用户仓库里的那一份从此不再被写入，且没有
    /// 任何提示，等他发现时两边已经分叉。解析到真实路径再做 temp+rename，
    /// 链接关系就得以保留。
    ///
    /// 只在**确实是**符号链接时才 canonicalize：普通路径直接返回，避免引入
    /// 副作用（Windows 上 canonicalize 会加 `\\?\` 前缀，父目录若是链接也会
    /// 被一并展开）。默认 no-op 供测试 mock 覆盖。
    fn resolve_symlink(&self, path: &Path) -> std::path::PathBuf {
        path.to_path_buf()
    }
}

/// 真实文件系统。
pub struct RealFs;

impl FsOps for RealFs {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        std::fs::read_to_string(path)
    }
    fn write(&self, path: &Path, content: &str) -> std::io::Result<()> {
        // 逐级建父目录：客户端 CLI「已安装但从未运行」时 ~/.codex、~/.gemini
        // 等目录尚不存在，否则临时文件写入直接 IO 失败并被包成吓人的
        // RecoveryFailed（原文件本不存在，恢复也失败）。
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        write_private(path, content)
    }
    fn fsync(&self, path: &Path) -> std::io::Result<()> {
        // 必须以写权限打开：Windows 的 FlushFileBuffers（sync_all 的底层实现）
        // 要求句柄具备 GENERIC_WRITE，只读句柄一律返回 ERROR_ACCESS_DENIED(5)。
        // Unix 上只读 fd 可以 fsync，所以用 File::open 在 macOS/Linux 全绿，
        // 到 Windows 才炸——而且炸在原子写路径上，等于所有配置写入全部失败。
        // https://learn.microsoft.com/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers
        // write(true) 不截断（截断要 truncate(true)），临时文件内容不受影响。
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)?
            .sync_all()
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }
    fn mtime_unix_nanos(&self, path: &Path) -> Option<u128> {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
    }
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
    fn harden_permissions(&self, path: &Path) -> std::io::Result<()> {
        crate::permissions::harden_to_owner_only(path)
    }
    fn resolve_symlink(&self, path: &Path) -> std::path::PathBuf {
        // symlink_metadata 不跟随链接，用它判定"这一项本身是不是链接"；
        // 只有是链接才 canonicalize（它会解析整条链，含多级链接）。
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
            }
            _ => path.to_path_buf(),
        }
    }
}

/// 一次成功写入的结果。
#[derive(Debug)]
pub struct ApplyOutcome {
    pub record: OperationRecord,
    pub diff: ConfigDiff,
    /// no-op 时为 false：不写入、不留历史、不落快照（record.snapshot 为 None）。
    pub written: bool,
}

/// 配置安全引擎。
pub struct ConfigEngine<S: SnapshotStore, H: HistoryStore, F: FsOps> {
    snapshots: S,
    history: H,
    fs: F,
}

impl<S: SnapshotStore, H: HistoryStore, F: FsOps> ConfigEngine<S, H, F> {
    pub fn new(snapshots: S, history: H, fs: F) -> Self {
        Self {
            snapshots,
            history,
            fs,
        }
    }

    /// 读取配置（含身份信息）。文件不存在按空文档处理（首次写入场景）。
    pub fn load(&self, path: &Path, format: ConfigFormat) -> Result<LoadedConfig, ConfigError> {
        let (content, missing) = if self.fs.exists(path) {
            (
                self.fs
                    .read_to_string(path)
                    .map_err(|e| ConfigError::io(format!("读取 {}", path.display()), e))?,
                false,
            )
        } else {
            (String::new(), true)
        };
        Ok(LoadedConfig {
            path: path.to_path_buf(),
            format,
            content_hash: content_hash(&content),
            mtime_unix_nanos: self.fs.mtime_unix_nanos(path),
            formatting: FileFormatting::detect(&content),
            content,
            missing,
        })
    }

    /// 计算 Diff（只读预览，供 UI 确认）。
    pub fn preview(
        &self,
        loaded: &LoadedConfig,
        plan: &PatchPlan,
    ) -> Result<ConfigDiff, ConfigError> {
        let doc = ConfigDocument::parse(&loaded.content, loaded.format)?;
        // 与 apply 同路：还原外壳后再算 Diff，预览才与实际落盘一致。
        let (mut diff, mut after_text) = ConfigDiff::compute_with_after(&doc, plan)?;
        loaded.formatting.restore(&mut after_text);
        diff.unified = unified_diff(&loaded.content, &after_text);
        Ok(diff)
    }

    /// 执行写入流水线。
    ///
    /// - `expected` 为之前 `load` 的结果：写入前重读并比对哈希，防外部修改；
    /// - `validate` 为业务验证回调（写入内容落盘前，在内存中验证解析后的文档）；
    /// - `redact` 为快照脱敏回调（只允许剔除可重新物化的 MCPane 管理字段）；
    /// - 计划为 no-op（Diff 无实际变化）时直接短路，不写入不留历史。
    pub fn apply(
        &self,
        expected: &LoadedConfig,
        plan: &PatchPlan,
        description: &str,
        validate: Option<Validator<'_>>,
        redact: Option<Redactor<'_>>,
    ) -> Result<ApplyOutcome, ConfigError> {
        let path = &expected.path;

        // 1. 写入前重读 + 哈希对比（冲突检测）。
        let current = self.load(path, expected.format)?;
        if current.content_hash != expected.content_hash {
            return Err(ConfigError::Conflict {
                path: path.clone(),
                expected_hash: expected.content_hash.clone(),
                actual_hash: current.content_hash,
            });
        }

        // 2. 解析并应用最小 Patch（Diff 与写入文本一次算出）。
        let before_doc = ConfigDocument::parse(&current.content, current.format)?;
        let (mut diff, mut after_text) = ConfigDiff::compute_with_after(&before_doc, plan)?;

        // 必须早于下面的 no-op 判断，否则三处一起错位：no-op 拿 LF 文本比 CRLF 原文
        // （内容没变也判不出，凭空写一次）、自写哈希与落盘内容对不上（监听误报外部
        // 改动）、写后重读校验必失败。
        current.formatting.restore(&mut after_text);

        // Diff 用磁盘原文与最终落盘文本，而非两份规范化文本——后者会把行尾/BOM/缩进
        // 的规整全抹平，UI 显示"只改三行"，落盘却是整份变更。
        diff.unified = unified_diff(&current.content, &after_text);

        // no-op 短路：不产生写入，也不落快照（内容未变，快照无回滚价值）。
        if after_text == current.content {
            return Ok(ApplyOutcome {
                record: self.build_record(
                    path,
                    OperationStatus::Applied,
                    &current.content_hash,
                    Some(current.content_hash.clone()),
                    None,
                    plan,
                    &format!("{description}（no-op）"),
                ),
                diff,
                written: false,
            });
        }

        // 3. 语法验证（重解析写入文本）+ 业务验证。
        let reparsed = ConfigDocument::parse(&after_text, current.format)?;
        if let Some(validate) = validate {
            validate(&reparsed).map_err(ConfigError::ValidationFailed)?;
        }

        // 登记自写 hash（早于 rename，即早于任何文件系统事件），供文件监听在
        // rename 事件到达时立刻认出自写，消除「rename 早于历史落盘」的竞态。
        register_own_write(path, &content_hash(&after_text));

        // 4. Snapshot（写前备份原内容）。
        let snapshot = self.snapshots.save(path, &current.content, redact)?;

        // 5. 同目录临时文件 → fsync → 原子 rename → 重读验证。
        match self.write_atomically(path, &after_text) {
            Ok(()) => {}
            Err(write_error) => {
                // 6. 失败恢复：尝试恢复原内容。
                return Err(self.recover(
                    path,
                    &current.content,
                    !current.missing,
                    &snapshot,
                    write_error,
                ));
            }
        }

        let verify = self.load(path, expected.format)?;
        if verify.content != after_text {
            let write_error = ConfigError::PostWriteVerification { path: path.clone() };
            return Err(self.recover(
                path,
                &current.content,
                !current.missing,
                &snapshot,
                write_error,
            ));
        }

        // 7. 操作历史。
        let record = self.build_record(
            path,
            OperationStatus::Applied,
            &current.content_hash,
            Some(verify.content_hash.clone()),
            Some(snapshot),
            plan,
            description,
        );
        // 写入已成功且通过重读验证——历史落盘失败不能倒过来把 apply 判为失败
        //（那样调用方会以为没写成，实际已写）。降级为 warn；自写识别有内存
        // 注册表兜底，不依赖这条历史。
        if let Err(error) = self.history.append(&record) {
            tracing::warn!(%error, path = %path.display(), "配置写入成功但操作历史记录失败");
        }
        Ok(ApplyOutcome {
            record,
            diff,
            written: true,
        })
    }

    /// 回滚到某快照。回滚前对当前文件再做一次快照。
    pub fn rollback(
        &self,
        snapshot: &SnapshotMeta,
        format: ConfigFormat,
    ) -> Result<OperationRecord, ConfigError> {
        let path = snapshot.source_path.clone();
        let current = self.load(&path, format)?;

        // 回滚前备份当前文件（强制规则）。
        let pre_rollback = self.snapshots.save(&path, &current.content, None)?;

        let target_content = self.snapshots.load(snapshot)?;
        // 快照内容必须能通过语法验证（防回滚进损坏内容）。
        ConfigDocument::parse(&target_content, format)?;

        // 回滚也是自写，同样早于 rename 登记，避免被文件监听误报为外部改动。
        register_own_write(&path, &content_hash(&target_content));

        match self.write_atomically(&path, &target_content) {
            Ok(()) => {}
            Err(write_error) => {
                return Err(self.recover(
                    &path,
                    &current.content,
                    !current.missing,
                    &pre_rollback,
                    write_error,
                ));
            }
        }

        let verify = self.load(&path, format)?;
        let record = OperationRecord {
            id: uuid::Uuid::new_v4().simple().to_string(),
            created_unix_secs: now_unix(),
            target_path: path,
            status: OperationStatus::RolledBack,
            hash_before: current.content_hash,
            hash_after: Some(verify.content_hash),
            snapshot: Some(pre_rollback),
            changed_paths: vec!["<rollback>".to_owned()],
            description: format!("回滚到快照 {}", snapshot.id),
        };
        self.history.append(&record)?;
        Ok(record)
    }

    pub fn history(&self) -> &H {
        &self.history
    }

    /// 快照存储句柄：供调用方按需回溯"某次写入之前"的历史内容（如 Grok
    /// 断开连接时找回被接管前的 `models.default` 原值）。与 [`Self::history`]
    /// 同一用途的只读访问，不重复引擎已有的读写路径。
    pub fn snapshots(&self) -> &S {
        &self.snapshots
    }

    // ---------- 内部 ----------

    fn write_atomically(&self, path: &Path, content: &str) -> Result<(), ConfigError> {
        // 路径若是 dotfiles 工具建的符号链接，先解析到真实文件再写：否则
        // 下面的 rename 会把链接本身换成普通文件，用户仓库里那份从此失联
        // （见 `FsOps::resolve_symlink`）。非链接时原样返回，行为不变。
        let resolved = self.fs.resolve_symlink(path);
        let path = resolved.as_path();
        let tmp = temp_path(path);
        self.fs
            .write(&tmp, content)
            .map_err(|e| ConfigError::io("写入临时文件", e))?;
        // rename 前先收紧临时文件权限，使最终文件落地即为 0600——这些配置里
        // 注入了 Hub / 中转 Token，绝不能以 umask 默认（0644）世界可读落盘。
        if let Err(e) = self.fs.harden_permissions(&tmp) {
            let _ = self.fs.remove_file(&tmp);
            return Err(ConfigError::io("收紧临时文件权限", e));
        }
        if let Err(e) = self.fs.fsync(&tmp) {
            let _ = self.fs.remove_file(&tmp);
            return Err(ConfigError::io("fsync 临时文件", e));
        }
        if let Err(e) = self.fs.rename(&tmp, path) {
            let _ = self.fs.remove_file(&tmp);
            return Err(ConfigError::io("原子替换", e));
        }
        // 兜底：rename 目标若此前已存在且权限更松，这里再夹一次（best-effort）。
        let _ = self.fs.harden_permissions(path);
        Ok(())
    }

    /// 写入失败后的自动恢复：把原内容写回目标文件。
    ///
    /// `existed` 必须来自 `LoadedConfig::missing` 的取反，不能用
    /// `original.is_empty()` 推断：0 字节的**已存在**文件内容同样为空，
    /// 按「本不存在」处理会把用户真实存在的文件删掉——恢复动作反而制造了
    /// 数据丢失。`.claude.json` 在 Claude Code 首次运行前正是 0 字节。
    fn recover(
        &self,
        path: &Path,
        original: &str,
        existed: bool,
        snapshot: &SnapshotMeta,
        write_error: ConfigError,
    ) -> ConfigError {
        // 目标文件可能处于三种状态：原样（rename 未发生）、新内容、缺失。
        if !existed {
            // 原文件本不存在（首次写入）：恢复 = 清掉任何残留，而不是凭空造一个
            // 空文件。对某些客户端「文件存在但为空」≠「文件不存在」，造空文件会
            // 引入原本没有的状态。删除失败（本就不存在）忽略即可。
            let _ = self.fs.remove_file(path);
        } else {
            let needs_restore = match self.fs.read_to_string(path) {
                Ok(current) => current != original,
                Err(_) => true,
            };
            if needs_restore && let Err(recover_error) = self.write_atomically(path, original) {
                let error = ConfigError::RecoveryFailed {
                    write_error: write_error.to_string(),
                    recover_error: recover_error.to_string(),
                    snapshot: snapshot.stored_at.display().to_string(),
                };
                tracing::error!(%error, "配置写入失败且自动恢复失败");
                return error;
            }
        }
        let record = OperationRecord {
            id: uuid::Uuid::new_v4().simple().to_string(),
            created_unix_secs: now_unix(),
            target_path: path.to_path_buf(),
            status: OperationStatus::FailedRecovered,
            hash_before: content_hash(original),
            hash_after: Some(content_hash(original)),
            snapshot: Some(snapshot.clone()),
            changed_paths: Vec::new(),
            description: format!("写入失败已恢复：{write_error}"),
        };
        let _ = self.history.append(&record);
        ConfigError::RecoveredFromFailure {
            source_message: write_error.to_string(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn build_record(
        &self,
        path: &Path,
        status: OperationStatus,
        hash_before: &str,
        hash_after: Option<String>,
        snapshot: Option<SnapshotMeta>,
        plan: &PatchPlan,
        description: &str,
    ) -> OperationRecord {
        OperationRecord {
            id: uuid::Uuid::new_v4().simple().to_string(),
            created_unix_secs: now_unix(),
            target_path: path.to_path_buf(),
            status,
            hash_before: hash_before.to_owned(),
            hash_after,
            snapshot,
            changed_paths: plan.ops.iter().map(|op| op.path().join(".")).collect(),
            description: description.to_owned(),
        }
    }
}

/// 以仅属主可读写（unix 0600）创建并写入文件，消除「先按 umask（0644）
/// 写入、再 chmod」之间的世界可读窗口——配置内容里注入了 Hub / 中转 Token。
/// 非 unix 平台退回普通写入（权限由平台 ACL 决定）。
///
/// 注意 `mode` 仅在**创建**文件时生效；对已存在文件重写不改其权限，故
/// `write_atomically` 始终写全新的临时文件再 rename，保证最终文件是 0600。
fn write_private(path: &Path, content: &str) -> std::io::Result<()> {
    crate::permissions::write_owner_only(path, content)
}

/// 临时文件路径：与目标同目录（保证 rename 原子性），加随机后缀。
fn temp_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    path.with_file_name(format!(".{file_name}.mcpane-tmp-{suffix}"))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

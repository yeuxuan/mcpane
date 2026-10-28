//! 操作历史：每次写入/回滚的可审计记录。
//!
//! P0 以 JSONL 追加存储（`<root>/config-operations.jsonl`）；
//! Phase 4 迁 SQLite `config_operations` 表时保持记录结构不变。

use std::path::PathBuf;
use std::sync::Mutex;

use crate::error::ConfigError;
use crate::snapshot::SnapshotMeta;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OperationStatus {
    Applied,
    RolledBack,
    FailedRecovered,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationRecord {
    pub id: String,
    pub created_unix_secs: u64,
    pub target_path: PathBuf,
    pub status: OperationStatus,
    pub hash_before: String,
    pub hash_after: Option<String>,
    /// 本次操作的写前快照（no-op 操作不产生快照）。
    pub snapshot: Option<SnapshotMeta>,
    /// 变更条目摘要（path 列表，不含值——值可能含 Secret）。
    pub changed_paths: Vec<String>,
    /// 人类可读说明（如"写入 mcpane Hub Entry"）。
    pub description: String,
}

pub trait HistoryStore: Send + Sync {
    fn append(&self, record: &OperationRecord) -> Result<(), ConfigError>;
    fn list(&self) -> Result<Vec<OperationRecord>, ConfigError>;
}

/// 历史文件超过此大小时触发轮转（只保留最近 `KEEP_RECORDS` 条）。
const ROTATE_BYTES: u64 = 512 * 1024;
const KEEP_RECORDS: usize = 500;

/// `append()`「打开+追加+判断轮转+轮转重写」串行化用。
///
/// `JsonlHistoryStore` 每次调用点都 `new` 一份新实例（无共享状态可挂锁），
/// 而这四步合起来不是原子操作：两个并发 append 之间，一个正在轮转（已读完
/// 旧内容准备写 tail）、另一个已经把新行 append 到旧文件后，轮转方
/// rename 覆盖上去——后者刚写的那一行连同它所在的旧文件一起被换掉，
/// 从此在 `list()` 里再也读不到（数据丢失，不报错，非常隐蔽）。
///
/// 本进程是唯一写者（桌面应用单实例，`tauri_plugin_single_instance`），
/// 一把进程级锁把整段临界区串行化即可根治：不同 `root`（如测试里的不同
/// 临时目录）会被无谓地陪绑序列化，但历史写入频率低、临界区只是几个文件
/// 系统调用量级，代价可忽略——比引入按路径分锁的 registry 更 KISS。
static APPEND_LOCK: Mutex<()> = Mutex::new(());

pub struct JsonlHistoryStore {
    root: PathBuf,
}

impl JsonlHistoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn file(&self) -> PathBuf {
        self.root.join("config-operations.jsonl")
    }

    /// 超过大小阈值时把文件收敛到最近 `KEEP_RECORDS` 条（原子替换）。
    /// 失败不阻塞追加——历史轮转是尽力而为的维护动作。
    fn rotate_if_needed(&self) {
        let path = self.file();
        let Ok(meta) = std::fs::metadata(&path) else {
            return;
        };
        if meta.len() <= ROTATE_BYTES {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.len() <= KEEP_RECORDS {
            return;
        }
        let tail = lines[lines.len() - KEEP_RECORDS..].join("\n") + "\n";
        let tmp = path.with_extension("jsonl.tmp");
        // 历史里带写前快照（可能含 Token），临时文件创建即 0600；
        // rename 保留 tmp 权限，顺带把存量 0644 的历史文件收敛掉。
        let _ = std::fs::remove_file(&tmp);
        if crate::permissions::write_owner_only(&tmp, &tail).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

impl HistoryStore for JsonlHistoryStore {
    fn append(&self, record: &OperationRecord) -> Result<(), ConfigError> {
        let _guard = APPEND_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::fs::create_dir_all(&self.root).map_err(|e| ConfigError::io("创建历史目录", e))?;
        let line =
            serde_json::to_string(record).map_err(|e| ConfigError::Snapshot(e.to_string()))?;
        use std::io::Write;
        let path = self.file();
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        // 历史里带写前快照（可能含 Token）：创建即 0600。同目录快照与 meta
        // 都已收紧，此前唯独这份走默认权限。
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let preexisting = path.exists();
        let mut file = options
            .open(&path)
            .map_err(|e| ConfigError::io("打开操作历史", e))?;
        // mode 只在创建时生效（Windows 则完全不经它）：对存量文件显式收紧一次。
        if preexisting || cfg!(windows) {
            crate::permissions::harden_to_owner_only(&path)
                .map_err(|e| ConfigError::io("收紧操作历史权限", e))?;
        }
        writeln!(file, "{line}").map_err(|e| ConfigError::io("追加操作历史", e))?;
        drop(file);
        self.rotate_if_needed();
        Ok(())
    }

    fn list(&self) -> Result<Vec<OperationRecord>, ConfigError> {
        let path = self.file();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content =
            std::fs::read_to_string(&path).map_err(|e| ConfigError::io("读取操作历史", e))?;
        Ok(content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn record(id: &str) -> OperationRecord {
        OperationRecord {
            id: id.to_owned(),
            created_unix_secs: 0,
            target_path: PathBuf::from("/x/settings.json"),
            status: OperationStatus::Applied,
            hash_before: "before".into(),
            hash_after: Some("after".into()),
            snapshot: None,
            changed_paths: vec![],
            description: "测试写入".into(),
        }
    }

    #[test]
    fn append_and_list_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlHistoryStore::new(dir.path());
        store.append(&record("op-1")).unwrap();
        store.append(&record("op-2")).unwrap();
        let records = store.list().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].id, "op-1");
        assert_eq!(records[1].id, "op-2");
    }

    /// 并发 append + 轮转竞态回归：`rotate_if_needed()` 读文件计算尾部时，
    /// 另一个线程的 `append()` 已经把新行写进了同一个（马上被换掉的）文件，
    /// rename 落地后那一行连同它所在的旧内容一起消失——`list()` 里再也读
    /// 不到、也不报错，是最隐蔽的一类数据丢失。
    ///
    /// `rotate_if_needed` 按**字节数**触发、按**条数**截尾：无竞态时它保证
    /// 「截尾后恰好剩 `KEEP_RECORDS` 条，之后随新 append 只增不减，直到
    /// 下次越过字节阈值再截回 `KEEP_RECORDS`」——所以任意时刻的条数只有下限
    /// （`>= KEEP_RECORDS`，还没积攒到 `KEEP_RECORDS` 条时 `>= 总写入数`），
    /// 没有精确值（取决于单条记录字节数与轮转触发的具体时机，断言精确值
    /// 会很脆）。用足够多的并发 append（记录体故意撑大，几百条就能越过
    /// 512KB 轮转阈值）逼出至少一次轮转；断言：文件行数与可解析记录数一致
    /// （没有被并发写撕烂成半行垃圾），条数不低于 `KEEP_RECORDS` 这条下限
    /// （竞态会让它跌破——这正是本测试要抓的缺陷），且无重复 id。
    /// 修复前（`append` 无锁）这条测试会在多次重跑里不稳定地跌破下限；
    /// 修复后（整段临界区靠 `APPEND_LOCK` 串行化）每次都满足。
    #[test]
    fn concurrent_append_survives_rotation_without_silent_loss() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JsonlHistoryStore::new(dir.path()));
        const THREADS: usize = 16;
        const PER_THREAD: usize = 60; // 16*60=960 条；撑大的记录体几百条就越过 512KB。
        let counter = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let store = Arc::clone(&store);
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || {
                    for _ in 0..PER_THREAD {
                        let n = counter.fetch_add(1, Ordering::SeqCst);
                        let mut rec = record(&format!("op-{n:05}"));
                        // 撑大单条记录体积：少量并发写入就能越过轮转阈值，
                        // 测试跑得快，还是能稳定命中至少一次轮转。
                        rec.hash_after = Some("x".repeat(600));
                        store.append(&rec).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let total_written = THREADS * PER_THREAD;
        let raw = std::fs::read_to_string(dir.path().join("config-operations.jsonl")).unwrap();
        let raw_lines = raw.lines().filter(|l| !l.trim().is_empty()).count();
        let records = store.list().unwrap();

        assert_eq!(
            raw_lines,
            records.len(),
            "文件里的行数与能解析出的记录数不一致：说明并发写入把某一行撕烂了"
        );
        assert!(
            records.len() >= total_written.min(KEEP_RECORDS),
            "条数跌破了 KEEP_RECORDS 下限（{}<{}）：说明轮转与并发 append 的竞态丢了数据",
            records.len(),
            total_written.min(KEEP_RECORDS)
        );
        assert!(
            records.len() <= total_written,
            "条数不该超过实际写入总数：{} > {total_written}",
            records.len()
        );
        let ids: std::collections::HashSet<_> = records.iter().map(|r| r.id.clone()).collect();
        assert_eq!(
            ids.len(),
            records.len(),
            "不应出现重复 id（数据被写两次或读重）"
        );
    }
}

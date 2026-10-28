//! 外部配置改动提醒：把 `mcpane-config` 的 `watch_config` 接到前端事件上。
//!
//! 已有的 hash 冲突检测（`ConfigEngine::load` 写前重读比对）只在**下次写入时**
//! 才发现外部改动，用户此前一直蒙在鼓里。本模块把这个发现提前到改动发生的那一刻。
//!
//! ## 怎么区分"我们自己写的"和"外部改的"
//!
//! MCPane 自己写配置同样会触发 watcher，直接转发等于把每次切换都报成外部改动。
//! 判据是 **history 的 `hash_after`**：读到新内容后算 hash，若命中最近若干条
//! 该文件的写入记录，就是我们自己干的。这个判据没有竞态——history 在写入事务内
//! 就记好了，必然早于文件系统事件到达（FSEvents 还有额外延迟）。
//!
//! ## 为什么不监听 auth.json
//!
//! Codex 的 `auth.json` 走 `write_auth_atomically`，刻意绕开引擎（避免 OAuth
//! Token 明文落快照），因此没有 history 记录可比对——监听它只会把我们自己的
//! 每次写入都误报成外部改动。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use mcpane_config::{ConfigWatcher, HistoryStore, JsonlHistoryStore, content_hash, watch_config};
use tauri::{AppHandle, Emitter};

/// 前端监听的事件名。
const EVENT: &str = "config-externally-changed";

/// 尚未被前端取走的改动。
///
/// 光靠 emit 不够：点关闭即退托盘，60s 后 WebView 被销毁（轻量模式），
/// 而用户去终端改配置恰恰就发生在窗口关着的时候——那时 emit 出去无人接收。
/// 不留存的话，这个提醒在最常见的场景下等于没做。
static PENDING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// 取走待读改动（取走即清空）：前端挂载时调用一次，与 emit 共同覆盖
/// "窗口开着"和"窗口关着"两种情形。
pub fn take_pending() -> Vec<String> {
    drain(&PENDING)
}

/// 队列注入版：`PENDING` 是全局单例，测试直接操作它会因 cargo test 并行
/// 而互相清空（后写覆盖前读）。逻辑收在这里，测试给自己的队列。
fn drain(queue: &Mutex<BTreeSet<String>>) -> Vec<String> {
    match queue.lock() {
        Ok(mut slot) => std::mem::take(&mut *slot).into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

/// 只回看**该文件**最近这些次写入：更早的写入不可能是本次事件的来源。
/// 按文件过滤而非按记录数截断——否则夹在一堆别的文件写入之间时会误判。
const HISTORY_LOOKBACK: usize = 20;

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ExternalChange {
    /// 被改动的文件绝对路径。
    path: String,
}

/// 句柄保活槽：`ConfigWatcher` 一旦 Drop 监听就停，必须挂住整个进程生命周期。
static WATCHERS: Mutex<Vec<ConfigWatcher>> = Mutex::new(Vec::new());

/// 启动监听。单个文件失败不影响其余——客户端没装时父目录本就不存在，属正常情况。
pub fn install(app: &AppHandle, targets: &[PathBuf]) {
    let watchers: Vec<ConfigWatcher> = targets
        .iter()
        .filter_map(|path| match watch_one(app, path) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                // 客户端未安装是常态，不值得打扰用户，只留给开发者看。
                eprintln!("[mcpane] 未监听 {}：{error}", path.display());
                None
            }
        })
        .collect();
    if let Ok(mut slot) = WATCHERS.lock() {
        *slot = watchers;
    }
}

fn watch_one(app: &AppHandle, path: &Path) -> Result<ConfigWatcher, mcpane_config::ConfigError> {
    let app = app.clone();
    let target = path.to_path_buf();
    // 基线设为当前内容：启动后内容没真正变过的事件（touch、mtime 抖动）不该打扰用户。
    let reported = Mutex::new(current_hash(path));

    watch_config(path, move |_kind| {
        let Some(hash) = current_hash(&target) else {
            return; // 读不到（被删/权限/读到写入中间态）——下次事件再说。
        };
        // 同一次保存常触发多个事件（写+改 mtime，编辑器原子保存还有 rename）：
        // 内容 hash 没变就不重复打扰。
        {
            let Ok(mut last) = reported.lock() else {
                return;
            };
            if last.as_deref() == Some(hash.as_str()) {
                return;
            }
            *last = Some(hash.clone());
        }
        if is_own_write(&target, &hash) {
            return;
        }
        eprintln!("[mcpane] 检测到外部改动：{}", target.display());
        let path = target.display().to_string();
        // 先入队再 emit：窗口关着时 emit 无人接收，全靠这份留存。
        if let Ok(mut slot) = PENDING.lock() {
            slot.insert(path.clone());
        }
        let _ = app.emit(EVENT, ExternalChange { path });
    })
}

fn current_hash(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|c| content_hash(&c))
}

/// 当前内容是否出自 MCPane 之手。优先查引擎的进程内自写注册表（写盘前登记，
/// 早于文件系统事件，无竞态）；再用操作历史兜底（进程重启后内存清空，旧写入
/// 仍可经历史识别）。
fn is_own_write(path: &Path, hash: &str) -> bool {
    mcpane_config::is_recent_own_write(path, hash)
        || is_own_write_in(&crate::commands::mcpane_home(), path, hash)
}

/// 路径注入版：`MCPANE_HOME` 改不得（`set_var` 需 unsafe，本 crate 禁用），
/// 故把 history 根目录提成参数以便测试。
///
/// history 读失败时保守返回 false——宁可多提醒一次，也不要漏掉真实的外部改动。
fn is_own_write_in(home: &Path, path: &Path, hash: &str) -> bool {
    let store = JsonlHistoryStore::new(home);
    let Ok(records) = store.list() else {
        return false;
    };
    records
        .iter()
        .rev()
        .filter(|record| record.target_path == path)
        .take(HISTORY_LOOKBACK)
        .any(|record| record.hash_after.as_deref() == Some(hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_hash_is_none_for_missing_file() {
        let missing = std::env::temp_dir().join("mcpane-watch-definitely-missing.json");
        let _ = std::fs::remove_file(&missing);
        assert!(current_hash(&missing).is_none());
    }

    /// hash 随内容变化——去重与自写判定都建立在这上面。
    #[test]
    fn current_hash_tracks_content() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("mcpane-watch-hash-{nanos}.json"));
        let _ = std::fs::write(&path, "{}");
        let first = current_hash(&path);
        let _ = std::fs::write(&path, r#"{"a":1}"#);
        let second = current_hash(&path);

        assert!(first.is_some() && second.is_some());
        assert_ne!(first, second, "内容变了 hash 必须变");
        let _ = std::fs::remove_file(&path);
    }

    /// 无 history 时保守判为外部改动——漏报比误报更该避免。
    #[test]
    fn unknown_hash_is_not_own_write() {
        assert!(!is_own_write(
            Path::new("/definitely/not/written/by/us.json"),
            "deadbeef"
        ));
    }

    fn temp_home(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let home = std::env::temp_dir().join(format!("mcpane-watch-{tag}-{nanos}"));
        let _ = std::fs::create_dir_all(&home);
        home
    }

    fn record(target: &Path, hash_after: &str) -> mcpane_config::OperationRecord {
        mcpane_config::OperationRecord {
            id: format!("op-{hash_after}"),
            created_unix_secs: 0,
            target_path: target.to_path_buf(),
            status: mcpane_config::OperationStatus::Applied,
            hash_before: "before".to_owned(),
            hash_after: Some(hash_after.to_owned()),
            snapshot: None,
            changed_paths: vec![],
            description: "测试写入".to_owned(),
        }
    }

    /// 本功能的成败关键：MCPane 自己写的内容必须被认出来，否则每次切换供应商
    /// 都会弹一次"配置被外部修改"。
    #[test]
    fn own_write_is_recognized_via_history() {
        let home = temp_home("own");
        let target = Path::new("/x/settings.json");
        let store = JsonlHistoryStore::new(&home);
        assert!(store.append(&record(target, "hash-we-wrote")).is_ok());

        assert!(
            is_own_write_in(&home, target, "hash-we-wrote"),
            "自己写的内容被误判为外部改动"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 外部改动的 hash 不在 history 里——必须报出来。
    #[test]
    fn external_change_is_not_own_write() {
        let home = temp_home("external");
        let target = Path::new("/x/settings.json");
        let store = JsonlHistoryStore::new(&home);
        assert!(store.append(&record(target, "hash-we-wrote")).is_ok());

        assert!(
            !is_own_write_in(&home, target, "hash-someone-else-wrote"),
            "外部改动被当成自己写的，提醒会漏掉"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// 用独立队列而非全局 PENDING：cargo test 并行跑，共享全局会互相清空。
    fn queue_of(paths: &[&str]) -> Mutex<BTreeSet<String>> {
        Mutex::new(paths.iter().map(|p| (*p).to_owned()).collect())
    }

    /// 取走即清空：前端拉过一次后，重建 WebView 不该再弹同一条。
    #[test]
    fn pending_is_drained_once() {
        let queue = queue_of(&["/x/settings.json"]);
        assert_eq!(drain(&queue), vec!["/x/settings.json".to_owned()]);
        assert!(drain(&queue).is_empty(), "第二次取应为空");
    }

    /// 同一文件反复改动只留一条——BTreeSet 去重，避免重开窗口时刷屏。
    #[test]
    fn pending_dedupes_same_path() {
        let queue = queue_of(&["/x/settings.json", "/x/settings.json", "/x/config.toml"]);
        assert_eq!(drain(&queue).len(), 2);
    }

    /// 全局队列确实接在 take_pending 上（上面两条测的是注入版，
    /// 这条防止 take_pending 哪天忘了接线）。
    #[test]
    fn take_pending_drains_the_global_queue() {
        if let Ok(mut slot) = PENDING.lock() {
            slot.insert("/global/probe.json".to_owned());
        }
        assert!(take_pending().contains(&"/global/probe.json".to_owned()));
    }

    /// 记录按 target_path 隔离：别的文件的 hash 不能顶替本文件的判定。
    #[test]
    fn history_of_other_file_does_not_count() {
        let home = temp_home("otherfile");
        let store = JsonlHistoryStore::new(&home);
        assert!(
            store
                .append(&record(Path::new("/x/config.toml"), "shared-hash"))
                .is_ok()
        );

        assert!(
            !is_own_write_in(&home, Path::new("/x/settings.json"), "shared-hash"),
            "另一个文件的写入记录不该为本文件背书"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}

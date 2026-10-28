//! 自动云同步：业务数据变更 → 防抖 → 上传（对齐 cc-switch 的语义）。
//!
//! - 只自动**上传**，从不自动下载（下载覆盖本地必须用户显式确认）；
//! - 变更信号由命令层埋点（`notify_changed`），worker 防抖 1s、连续变更
//!   最长合并等待 10s；
//! - `suppress()`：download 导入期间的 DB 写入不得反向触发上传；
//! - 是否真的执行由 `commands::sync::auto_upload` 内部判定
//!   （enabled + auto_sync 双开关、与手动同步共享全局互斥）；
//! - 失败只记状态不重试（下次变更自然再试）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

static DIRTY: Notify = Notify::const_new();
static SUPPRESS_COUNT: AtomicUsize = AtomicUsize::new(0);

const DEBOUNCE: Duration = Duration::from_secs(1);
const MAX_WAIT: Duration = Duration::from_secs(10);

/// 业务数据变更埋点（servers/workspaces/providers/connections 等命令收尾调用）。
/// 廉价：只置脏信号，判定与执行都在 worker。
pub fn notify_changed() {
    if SUPPRESS_COUNT.load(Ordering::SeqCst) > 0 {
        return;
    }
    DIRTY.notify_one();
}

/// 抑制守卫（RAII 计数）：download 导入等"自产写入"期间挂起触发。
pub fn suppress() -> SuppressGuard {
    SUPPRESS_COUNT.fetch_add(1, Ordering::SeqCst);
    SuppressGuard
}

pub struct SuppressGuard;

impl Drop for SuppressGuard {
    fn drop(&mut self) {
        SUPPRESS_COUNT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 启动自动同步 worker（应用 setup 时调用一次）。
pub fn spawn_worker() {
    tauri::async_runtime::spawn(async {
        loop {
            // 等第一个脏信号。
            DIRTY.notified().await;
            // 防抖窗口：静默 1s 即触发；连续变更最多合并等待 10s。
            let first_dirty = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = DIRTY.notified() => {
                        if first_dirty.elapsed() >= MAX_WAIT {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(DEBOUNCE) => break,
                }
            }
            crate::commands::auto_upload().await;
        }
    });
}

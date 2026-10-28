//! 外部修改监听：文件变化时回调（Desktop 用于提示"配置已被外部修改"）。

use std::path::Path;

use notify::{RecursiveMode, Watcher};

use crate::error::ConfigError;

/// 监听句柄：Drop 即停止监听。
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
}

/// 目标文件的变更签名（mtime + 长度）；文件不存在时为 None，
/// 因此删除/重建本身也构成一次可感知的变化。
fn change_signature(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    std::fs::metadata(path).ok().map(|meta| {
        (
            meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            meta.len(),
        )
    })
}

/// 监听单个配置文件的外部修改。
///
/// 回调参数为触发变化的事件种类描述。注意平台差异：编辑器的
/// 原子保存可能表现为 rename/remove+create，调用方不应假设只有 Modify。
pub fn watch_config(
    path: &Path,
    on_change: impl Fn(&str) + Send + 'static,
) -> Result<ConfigWatcher, ConfigError> {
    // 监听的是父目录（见下），必须按目标文件过滤事件——
    // 对 `~/.claude.json` 这类文件，父目录是 HOME，不过滤等于监听整个家目录。
    // 事件路径可能是 canonical 形式（macOS 上 /var → /private/var），两种候选都比较。
    let target_path = path.to_path_buf();
    let parent_path = path.parent().map(Path::to_path_buf);
    let canonical_dir = path.parent().and_then(|dir| dir.canonicalize().ok());
    let canonical_path = canonical_dir
        .as_ref()
        .zip(path.file_name())
        .map(|(dir, name)| dir.join(name));
    // FSEvents 在事件被合并/丢弃时会给出空路径或只有父目录的模糊事件。
    // 模糊事件不能直接当命中——监听 HOME 下的配置时，邻居文件的动静就会
    // 变成误报——先核对目标文件的 mtime+长度签名，真变了才回调。
    let last_signature = std::sync::Mutex::new(change_signature(path));
    let mut watcher =
        notify::recommended_watcher(move |result: notify::Result<notify::Event>| match result {
            Ok(event) => {
                let direct_hit = event
                    .paths
                    .iter()
                    .any(|p| p == &target_path || Some(p) == canonical_path.as_ref());
                let ambiguous = event.paths.is_empty()
                    || event.paths.iter().all(|p| {
                        Some(p) == parent_path.as_ref() || Some(p) == canonical_dir.as_ref()
                    });
                let mut last = match last_signature.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let fire = if direct_hit {
                    *last = change_signature(&target_path);
                    true
                } else if ambiguous {
                    let current = change_signature(&target_path);
                    let changed = *last != current;
                    *last = current;
                    changed
                } else {
                    false
                };
                drop(last);
                if fire {
                    on_change(&format!("{:?}", event.kind));
                }
            }
            Err(error) => tracing::warn!(%error, "配置监听错误"),
        })
        .map_err(|e| ConfigError::Snapshot(format!("创建文件监听失败: {e}")))?;

    // 监听父目录：覆盖原子替换（rename 进来）场景。
    let target = path.parent().unwrap_or(path);
    watcher
        .watch(target, RecursiveMode::NonRecursive)
        .map_err(|e| ConfigError::Snapshot(format!("启动文件监听失败: {e}")))?;

    Ok(ConfigWatcher { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// 等 FSEvents 把 setup 阶段（建 tempdir、写 config.json）的历史事件吐完，
    /// 再把计数清零当基线。
    ///
    /// 不能用固定 sleep：FSEvents 的投递延迟在负载高的 CI 机器上能到几百毫秒，
    /// 建文件那一下的事件会晚到、落进后面的断言窗口，把「兄弟文件不触发」
    /// 误判成失败（本地几乎必过，CI 上间歇红）。改为等到连续 400ms 没有新事件
    /// 才认为安静。
    fn settle(hits: &AtomicUsize) {
        let mut last = hits.load(Ordering::SeqCst);
        let mut quiet_since = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            let current = hits.load(Ordering::SeqCst);
            if current != last {
                last = current;
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= Duration::from_millis(400) {
                break;
            }
        }
        hits.store(0, Ordering::SeqCst);
    }

    #[test]
    fn watcher_fires_on_external_modification() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.json");
        std::fs::write(&file, "{}").unwrap();

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let _watcher = watch_config(&file, move |_| {
            hits_clone.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        // 给 watcher 启动留时间，然后外部修改。
        std::thread::sleep(std::time::Duration::from_millis(300));
        std::fs::write(&file, r#"{"changed": true}"#).unwrap();

        // FSEvents 有延迟，轮询等待。
        for _ in 0..40 {
            if hits.load(Ordering::SeqCst) > 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("外部修改未触发监听回调");
    }

    #[test]
    fn watcher_ignores_sibling_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.json");
        std::fs::write(&file, "{}").unwrap();

        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let _watcher = watch_config(&file, move |_| {
            hits_clone.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        settle(&hits);
        // 同目录其他文件的活动不应触发回调（监听父目录但按目标过滤）。
        std::fs::write(dir.path().join("other.txt"), "noise").unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "兄弟文件变动不应触发回调");
    }
}

//! Provider 用量查询结果的进程内缓存：托盘子菜单展示最近一次查询的配额摘要。
//!
//! 只有成功查过的 Provider 才会进缓存（后台刷新也只刷这些，避免首次接触陌生凭据）；
//! 进了缓存之后失败结果同样覆盖进来，托盘据此不再显示过期数字。不落盘。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use mcpane_provider::ProviderQuota;

fn cache() -> &'static Mutex<HashMap<String, ProviderQuota>> {
    static CACHE: OnceLock<Mutex<HashMap<String, ProviderQuota>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn get(provider_id: &str) -> Option<ProviderQuota> {
    cache().lock().ok()?.get(provider_id).cloned()
}

/// 记录一次查询结果：成功的一律写入；失败的只覆盖已有条目（从没成功过的不收）。
/// 返回缓存是否有变化，调用方据此决定要不要重建托盘菜单。
pub fn record(provider_id: String, quota: ProviderQuota) -> bool {
    {
        let Ok(mut guard) = cache().lock() else {
            return false;
        };
        if !quota.success && !guard.contains_key(&provider_id) {
            return false;
        }
        guard.insert(provider_id, quota);
    }
    // 额度写入的唯一收口：本地路由的「额度优先」排序据此同步信号。
    crate::desktop_proxy::push_quota_signals();
    true
}

pub fn snapshot() -> Vec<(String, ProviderQuota)> {
    cache()
        .lock()
        .map(|guard| guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

pub fn cached_ids() -> Vec<String> {
    cache()
        .lock()
        .map(|guard| guard.keys().cloned().collect())
        .unwrap_or_default()
}

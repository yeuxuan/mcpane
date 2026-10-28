//! models.dev 自动定价同步（对齐 cc-switch #5734，选集策略按本项目裁剪）。
//!
//! 与 cc-switch 的差异：不做「家族×N + 手动多选」选集 UI——我们的成本核算按
//! normalize 后的 model_key JOIN 定价表，`usage_events` 已记录用户真用过的全部
//! 键，目标集取「用过的 ∪ 表内既有」即可精准覆盖（unpriced 的键一出现，下次
//! 同步就会补上价格），零选择界面。
//!
//! 落盘语义：INSERT OR REPLACE 覆写目标行——models.dev 数据比内置种子新
//! （厂商调价随时同步），种子只作首装兜底与断网回落。

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use mcpane_storage::{PricingRow, SettingsRepo, UsageRepo};
use serde::{Deserialize, Serialize};

use super::db_pool;

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// 启动自动同步的最小间隔；手动「立即同步」不受限。
const AUTO_SYNC_MIN_INTERVAL_SECS: i64 = 24 * 3600;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// 目录体积护栏：全量目录当前约 3.5MB，32MiB 容得下增长、挡得住异常膨胀。
const MAX_CATALOG_BYTES: u64 = 32 * 1024 * 1024;

/// 同步状态（app_settings.pricing_sync JSON 列；NULL = 默认值）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PricingSyncState {
    pub enabled: bool,
    /// 上次成功同步的 Unix 秒。失败不覆盖（保留上次成功时刻）。
    pub last_synced_at: Option<i64>,
    pub last_error: Option<String>,
    /// 上次成功写入的行数。
    pub last_written: Option<u64>,
}

impl Default for PricingSyncState {
    fn default() -> Self {
        Self {
            enabled: true,
            last_synced_at: None,
            last_error: None,
            last_written: None,
        }
    }
}

/* ---------- models.dev 目录结构（只取所需字段） ---------- */

#[derive(Debug, Deserialize)]
struct CatalogProvider {
    #[serde(default)]
    models: BTreeMap<String, CatalogModel>,
}

#[derive(Debug, Deserialize)]
struct CatalogModel {
    name: Option<String>,
    cost: Option<CatalogCost>,
}

#[derive(Debug, Deserialize)]
struct CatalogCost {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

/// f64 → 十进制字符串（与迁移种子同形态）。非有限值/负值视为无效。
fn format_price(value: f64) -> Option<String> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    // Rust f64 Display 是最短往返表示（"0.0028"、"5"），无科学计数法风险
    //（价格量级恒在 1e-4..1e3，Display 对该区间不会切换 e 记法）。
    Some(format!("{value}"))
}

/// 厂商原价优先表（models.dev 的 provider id，顺序即优先级）。
///
/// 目录里同一模型会被大量聚合/转售站重复收录且加价（实测 hy3 被 "cortecs"
/// 以 3 倍价抢先命中）；先扫厂商列表拿原价，其余 provider 只作兜底。
const PREFERRED_PROVIDERS: [&str; 18] = [
    "anthropic",
    "openai",
    "google",
    "deepseek",
    "moonshotai",
    "moonshotai-cn",
    "kimi-for-coding",
    "zai",
    "alibaba",
    "alibaba-cn",
    "tencent-tokenhub",
    "tencent-coding-plan",
    "minimax",
    "minimax-cn",
    "stepfun",
    "mistral",
    "xai",
    "google-vertex",
];

/// 从目录中挑出命中目标键的定价行。
///
/// 两段式 + 分权：
/// - 厂商段（`PREFERRED_PROVIDERS` 序）：原价可信，可**覆写**定价表既有行；
/// - 兜底段（其余 provider 字典序）：转售/聚合价只配**补缺**（`existing`
///   之外的键），不得覆写既有行——防转售加价洗掉厂商价或内置种子价。
/// - `input`/`output` 双零视为无价跳过（订阅套餐类在 models.dev 标 0，
///   写进去会让成本静默归零、连 unpriced 标注都消失）。
///
/// 同一 normalize 键首见者胜。
fn select_rows(
    catalog: &BTreeMap<String, CatalogProvider>,
    targets: &HashSet<String>,
    existing: &HashSet<String>,
) -> Vec<PricingRow> {
    let mut chosen: HashSet<String> = HashSet::new();
    let mut rows = Vec::new();
    let preferred = PREFERRED_PROVIDERS
        .iter()
        .filter_map(|id| catalog.get(*id).map(|p| (p, true)));
    let rest = catalog
        .iter()
        .filter(|(id, _)| !PREFERRED_PROVIDERS.contains(&id.as_str()))
        .map(|(_, provider)| (provider, false));
    for (provider, is_vendor) in preferred.chain(rest) {
        for (model_id, model) in &provider.models {
            let key = mcpane_usage::normalize_model(model_id);
            if !targets.contains(&key) || chosen.contains(&key) {
                continue;
            }
            if !is_vendor && existing.contains(&key) {
                continue;
            }
            let Some(cost) = &model.cost else { continue };
            let (Some(input), Some(output)) = (cost.input, cost.output) else {
                continue;
            };
            if input == 0.0 && output == 0.0 {
                continue;
            }
            let (Some(input), Some(output)) = (format_price(input), format_price(output)) else {
                continue;
            };
            let cache_read = cost.cache_read.and_then(format_price);
            let cache_write = cost.cache_write.and_then(format_price);
            chosen.insert(key.clone());
            rows.push(PricingRow {
                model_id: key,
                display_name: model.name.clone().unwrap_or_else(|| model_id.clone()),
                input_per_million: input,
                output_per_million: output,
                cache_read_per_million: cache_read.unwrap_or_else(|| "0".into()),
                cache_creation_per_million: cache_write.unwrap_or_else(|| "0".into()),
            });
        }
    }
    rows
}

/* ---------- 状态存取 ---------- */

async fn load_state(settings: &SettingsRepo) -> Result<PricingSyncState, String> {
    let raw = settings
        .get_pricing_sync()
        .await
        .map_err(|e| e.to_string())?;
    Ok(raw
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default())
}

async fn save_state(settings: &SettingsRepo, state: &PricingSyncState) -> Result<(), String> {
    let json = serde_json::to_string(state).map_err(|e| e.to_string())?;
    settings
        .set_pricing_sync(&json)
        .await
        .map_err(|e| e.to_string())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/* ---------- 同步执行 ---------- */

async fn fetch_catalog() -> Result<BTreeMap<String, CatalogProvider>, String> {
    let response = mcpane_net::client_for(MODELS_DEV_URL)
        .get(MODELS_DEV_URL)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("请求 models.dev 失败：{e}"))?;
    if !response.status().is_success() {
        return Err(format!("models.dev 返回 {}", response.status()));
    }
    if response.content_length().unwrap_or(0) > MAX_CATALOG_BYTES {
        return Err("models.dev 目录超过体积上限".into());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("读取 models.dev 响应失败：{e}"))?;
    if bytes.len() as u64 > MAX_CATALOG_BYTES {
        return Err("models.dev 目录超过体积上限".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("解析 models.dev 目录失败：{e}"))
}

/// 执行一次同步；结果（成功时刻/错误）写回状态并返回。
async fn sync_once() -> Result<PricingSyncState, String> {
    let pool = db_pool().await?;
    let settings = SettingsRepo::new(pool.clone());
    let usage = UsageRepo::new(pool);
    let mut state = load_state(&settings).await?;

    let outcome = async {
        let targets: HashSet<String> = usage
            .pricing_target_keys()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect();
        let existing: HashSet<String> = usage
            .pricing_model_ids()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect();
        let catalog = fetch_catalog().await?;
        let rows = select_rows(&catalog, &targets, &existing);
        usage
            .pricing_upsert_batch(&rows)
            .await
            .map_err(|e| e.to_string())
    }
    .await;

    match outcome {
        Ok(written) => {
            state.last_synced_at = Some(now_secs());
            state.last_error = None;
            state.last_written = Some(written);
        }
        Err(error) => state.last_error = Some(error),
    }
    save_state(&settings, &state).await?;
    Ok(state)
}

/// 启动自动同步：延迟 30s 避开启动竞争；24h 间隔内不重复；失败只记状态。
pub fn spawn_startup_sync() {
    tauri::async_runtime::spawn(async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let due = async {
            let pool = db_pool().await.ok()?;
            let settings = SettingsRepo::new(pool);
            let state = load_state(&settings).await.ok()?;
            if !state.enabled {
                return None;
            }
            let last = state.last_synced_at.unwrap_or(0);
            (now_secs() - last >= AUTO_SYNC_MIN_INTERVAL_SECS).then_some(())
        }
        .await;
        if due.is_some() {
            match sync_once().await {
                Ok(state) => {
                    if let Some(error) = state.last_error {
                        eprintln!("[mcpane] models.dev 定价同步失败: {error}");
                    }
                }
                Err(error) => eprintln!("[mcpane] models.dev 定价同步失败: {error}"),
            }
        }
    });
}

/* ---------- 命令 ---------- */

#[tauri::command]
pub async fn pricing_sync_status() -> Result<PricingSyncState, String> {
    let pool = db_pool().await?;
    load_state(&SettingsRepo::new(pool)).await
}

#[tauri::command]
pub async fn pricing_sync_set_enabled(enabled: bool) -> Result<PricingSyncState, String> {
    let pool = db_pool().await?;
    let settings = SettingsRepo::new(pool);
    let mut state = load_state(&settings).await?;
    state.enabled = enabled;
    save_state(&settings, &state).await?;
    Ok(state)
}

/// 手动同步：不受 24h 间隔与开关限制（用户点了就是要同步）。
/// 网络/解析失败折叠进 `lastError` 返回，由 UI 内联展示。
#[tauri::command]
pub async fn pricing_sync_run() -> Result<PricingSyncState, String> {
    sync_once().await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn catalog_json(json: &str) -> BTreeMap<String, CatalogProvider> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn select_rows_normalizes_and_prefers_first_provider() {
        let catalog = catalog_json(
            r#"{
              "anthropic": {"models": {
                "claude-opus-5-20260801": {"name": "Claude Opus 5",
                  "cost": {"input": 5, "output": 25, "cache_read": 0.5, "cache_write": 6.25}}
              }},
              "openrouter": {"models": {
                "claude-opus-5": {"name": "Opus 5 (resold)",
                  "cost": {"input": 6, "output": 30}}
              }}
            }"#,
        );
        let targets: HashSet<String> = ["claude-opus-5".to_owned()].into();
        let rows = select_rows(&catalog, &targets, &HashSet::new());
        // 日期后缀被 normalize 剥掉；anthropic 在厂商优先表内，原价胜过转售价。
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model_id, "claude-opus-5");
        assert_eq!(rows[0].input_per_million, "5");
        assert_eq!(rows[0].cache_creation_per_million, "6.25");
    }

    #[test]
    fn vendor_price_beats_reseller_even_when_reseller_sorts_first() {
        // 实测坑：字典序首见会让 "cortecs" 的 3 倍转售价抢在 tencent 前面。
        let catalog = catalog_json(
            r#"{
              "cortecs": {"models": {"hy3": {"cost": {"input": 0.41, "output": 1.025}}}},
              "tencent-tokenhub": {"models": {"hy3": {"cost": {"input": 0.14, "output": 0.56}}}}
            }"#,
        );
        let targets: HashSet<String> = ["hy3".to_owned()].into();
        let rows = select_rows(&catalog, &targets, &HashSet::new());
        assert_eq!(rows[0].input_per_million, "0.14");
        // 仅聚合站收录的长尾模型仍能兜底补缺。
        let catalog = catalog_json(
            r#"{"some-aggregator": {"models": {"tail-model": {"cost": {"input": 1, "output": 2}}}}}"#,
        );
        let targets: HashSet<String> = ["tail-model".to_owned()].into();
        assert_eq!(select_rows(&catalog, &targets, &HashSet::new()).len(), 1);
    }

    #[test]
    fn resellers_cannot_overwrite_existing_rows_and_zero_price_is_skipped() {
        // 订阅套餐在 models.dev 标 0/0（无按量价）：厂商段也不得把种子价洗成 0。
        // 兜底段的转售价对既有行只读——种子/厂商价不被加价条目顶掉。
        let catalog = catalog_json(
            r#"{
              "cortecs": {"models": {"hy3": {"cost": {"input": 0.41, "output": 1.025}}}},
              "tencent-tokenhub": {"models": {"hy3": {"cost": {"input": 0, "output": 0}}}}
            }"#,
        );
        let targets: HashSet<String> = ["hy3".to_owned()].into();
        let existing: HashSet<String> = ["hy3".to_owned()].into();
        assert!(select_rows(&catalog, &targets, &existing).is_empty());
        // 同样的转售条目，对**缺行**的键允许补缺。
        assert_eq!(
            select_rows(&catalog, &targets, &HashSet::new())[0].input_per_million,
            "0.41"
        );
    }

    #[test]
    fn select_rows_skips_untargeted_and_invalid_cost() {
        let catalog = catalog_json(
            r#"{
              "x": {"models": {
                "not-wanted": {"cost": {"input": 1, "output": 2}},
                "wanted-no-cost": {},
                "wanted-negative": {"cost": {"input": -1, "output": 2}},
                "wanted-ok": {"cost": {"input": 0.0028, "output": 0.28}}
              }}
            }"#,
        );
        let targets: HashSet<String> = [
            "wanted-no-cost".to_owned(),
            "wanted-negative".to_owned(),
            "wanted-ok".to_owned(),
        ]
        .into();
        let rows = select_rows(&catalog, &targets, &HashSet::new());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model_id, "wanted-ok");
        // 小数保持十进制字符串、缺省缓存价回落 "0"。
        assert_eq!(rows[0].input_per_million, "0.0028");
        assert_eq!(rows[0].cache_read_per_million, "0");
        // display_name 缺失回落模型 id。
        assert_eq!(rows[0].display_name, "wanted-ok");
    }

    #[test]
    fn state_defaults_enabled_and_roundtrips() {
        let state = PricingSyncState::default();
        assert!(state.enabled);
        let json = serde_json::to_string(&state).unwrap();
        let back: PricingSyncState = serde_json::from_str(&json).unwrap();
        assert!(back.enabled);
        // 兼容旧列/半损坏 JSON：未知字段忽略、缺字段吃默认。
        let sparse: PricingSyncState = serde_json::from_str(r#"{"enabled": false}"#).unwrap();
        assert!(!sparse.enabled);
        assert_eq!(sparse.last_synced_at, None);
    }
}

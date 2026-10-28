//! 从 cc-switch 迁移：供应商与 MCP 服务一次搬过来。
//!
//! 只读用户那侧的数据（见 `mcpane-ccswitch`），入库全部走本项目既有的通道——
//! 供应商走 `ProviderService::import_relays`（Key 转存系统凭据库），MCP 服务走
//! 与「从客户端导入」同一个 `import_defs`（slug 去重、通知 Hub 重载都在里面）。

use mcpane_ccswitch::{CcSwitchSource, read_all};
use mcpane_provider::{ClientApp, RelayImportOutcome};

use super::connect::user_home;
use super::provider::{provider_service, provider_write_guard};

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppCount {
    pub app: String,
    pub count: usize,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchFound {
    pub path: String,
    pub provider_count: usize,
    pub server_count: usize,
    /// 按客户端分组的可迁移条数，UI 用来写「Claude Code 3 · Codex 1」。
    pub apps: Vec<AppCount>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchDetection {
    /// 查找过的目录。没找到时也要告诉用户查的是哪儿，否则无从判断是没装还是装在别处。
    pub searched: String,
    pub found: Option<CcSwitchFound>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CcSwitchImportSummary {
    pub providers: RelayImportOutcome,
    pub servers: super::import::ImportSummary,
    /// 读取阶段就搬不动的条目（proxy 模式的 Desktop、MCPane 没有的客户端等）。
    pub warnings: Vec<String>,
    /// 并入 MCPane 故障转移队列的候选数（开启本地路由后生效）。
    pub failover_migrated: usize,
}

fn source() -> Option<CcSwitchSource> {
    mcpane_ccswitch::discover(&user_home())
}

/// 展示给用户的"查过哪儿"。要走 `config_dir` 而不是硬拼 `~/.cc-switch`：
/// 用户把 cc-switch 的数据目录挪走时，报一个他早就不用的路径只会更糊涂。
fn searched_dir() -> String {
    mcpane_ccswitch::config_dir(&user_home())
        .display()
        .to_string()
}

#[tauri::command]
pub async fn ccswitch_detect() -> Result<CcSwitchDetection, String> {
    let searched = searched_dir();
    let Some(source) = source() else {
        return Ok(CcSwitchDetection {
            searched,
            found: None,
        });
    };
    let data = read_all(&source).await.map_err(|e| e.to_string())?;
    let mut apps: Vec<AppCount> = Vec::new();
    for provider in &data.providers {
        let app = provider.profile.app.as_str().to_string();
        match apps.iter_mut().find(|entry| entry.app == app) {
            Some(entry) => entry.count += 1,
            None => apps.push(AppCount { app, count: 1 }),
        }
    }
    Ok(CcSwitchDetection {
        searched,
        found: Some(CcSwitchFound {
            path: source.path().display().to_string(),
            provider_count: data.providers.len(),
            server_count: data.servers.len(),
            apps,
        }),
    })
}

/// 导入。两类各自独立：勾了哪类就搬哪类，另一类的结果为空。
/// 幂等——重复导入只会得到一堆 skipped。
#[tauri::command]
pub async fn ccswitch_import(
    app: tauri::AppHandle,
    providers: bool,
    servers: bool,
) -> Result<CcSwitchImportSummary, String> {
    let source = source()
        .ok_or_else(|| format!("没找到 cc-switch 的配置，在 {} 里找过了", searched_dir()))?;
    let data = read_all(&source).await.map_err(|e| e.to_string())?;

    let mut failover_migrated = 0;
    let provider_outcome = if providers {
        // 故障转移名次按「客户端 + 地址」记下：导入时同地址的已有条目会被跳过，
        // 名次要落到那条已有供应商上。
        let mut ranked: Vec<(i64, ClientApp, Option<String>)> = data
            .providers
            .iter()
            .filter_map(|p| {
                p.failover_rank
                    .map(|r| (r, p.profile.app, p.profile.base_url.clone()))
            })
            .collect();
        ranked.sort_by_key(|(rank, _, _)| *rank);
        let incoming: Vec<_> = data
            .providers
            .into_iter()
            .map(|p| (p.profile, p.api_key))
            .collect();
        let tuning = data.routing_tuning;
        let (outcome, migrated) = tauri::async_runtime::spawn_blocking(move || {
            let _guard = provider_write_guard();
            let svc = provider_service();
            let outcome = svc.import_relays(incoming).map_err(|e| e.to_string())?;
            let profiles = svc.list().map_err(|e| e.to_string())?;
            let mut migrated = 0;
            for app in ClientApp::ALL
                .into_iter()
                .filter(|app| app.supports_routing())
            {
                let ids: Vec<String> = ranked
                    .iter()
                    .filter(|(_, a, _)| *a == app)
                    .filter_map(|(_, _, base)| {
                        profiles
                            .iter()
                            .find(|p| p.app == app && p.base_url == *base)
                            .map(|p| p.id.clone())
                    })
                    .collect();
                migrated += svc
                    .append_failover_candidates(app, &ids)
                    .map_err(|e| e.to_string())?;
            }
            for (app, tuning) in tuning {
                svc.adopt_routing_tuning(app, tuning)
                    .map_err(|e| e.to_string())?;
            }
            Ok::<_, String>((outcome, migrated))
        })
        .await
        .map_err(|e| e.to_string())??;
        failover_migrated = migrated;
        // 已开路由的 app 需要刷新路由快照，新并入的备用与调参才立即生效。
        if let Err(error) = crate::desktop_proxy::sync_from_store().await {
            eprintln!("[mcpane] 迁移后刷新本地路由失败: {error}");
        }
        if !outcome.imported.is_empty() {
            crate::refresh_tray_menu(&app);
        }
        outcome
    } else {
        RelayImportOutcome::default()
    };

    let server_outcome = if servers {
        let candidates: Vec<_> = data
            .servers
            .iter()
            .filter_map(|(name, entry)| super::import::json_entry_to_def(name, entry))
            .collect();
        super::import::import_defs(&app, candidates, Vec::new()).await?
    } else {
        super::import::ImportSummary::default()
    };

    Ok(CcSwitchImportSummary {
        providers: provider_outcome,
        servers: server_outcome,
        warnings: data.warnings,
        failover_migrated,
    })
}

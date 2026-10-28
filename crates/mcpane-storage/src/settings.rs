//! 应用级偏好设置（单行表，见 0002_app_settings.sql）。

use mcpane_net::ProxyConfig;
use sqlx::Row;

use crate::{SqlitePool, StorageError};

pub struct SettingsRepo {
    pool: SqlitePool,
}

impl SettingsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get_hub_enabled(&self) -> Result<bool, StorageError> {
        let row = sqlx::query("SELECT hub_enabled FROM app_settings WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("hub_enabled") == 1)
    }

    pub async fn set_hub_enabled(&self, enabled: bool) -> Result<(), StorageError> {
        sqlx::query("UPDATE app_settings SET hub_enabled = ? WHERE id = 1")
            .bind(enabled as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 云同步配置（JSON 串；语义由命令层解释）。
    pub async fn get_sync_config(&self) -> Result<Option<String>, StorageError> {
        let row = sqlx::query("SELECT sync_config FROM app_settings WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("sync_config"))
    }

    pub async fn set_sync_config(&self, config: &str) -> Result<(), StorageError> {
        sqlx::query("UPDATE app_settings SET sync_config = ? WHERE id = 1")
            .bind(config)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// models.dev 定价同步状态（JSON 串；语义由命令层解释）。
    pub async fn get_pricing_sync(&self) -> Result<Option<String>, StorageError> {
        let row = sqlx::query("SELECT pricing_sync FROM app_settings WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("pricing_sync"))
    }

    pub async fn set_pricing_sync(&self, state: &str) -> Result<(), StorageError> {
        sqlx::query("UPDATE app_settings SET pricing_sync = ? WHERE id = 1")
            .bind(state)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn get_auto_update_check(&self) -> Result<bool, StorageError> {
        let row = sqlx::query("SELECT auto_update_check FROM app_settings WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("auto_update_check") == 1)
    }

    pub async fn set_auto_update_check(&self, enabled: bool) -> Result<(), StorageError> {
        sqlx::query("UPDATE app_settings SET auto_update_check = ? WHERE id = 1")
            .bind(enabled as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 开机自启的默认开启是否已经向系统推过一次。
    ///
    /// 不是「当前开没开」——那个只问操作系统。见 migrations/0022。
    pub async fn get_autostart_seeded(&self) -> Result<bool, StorageError> {
        let row = sqlx::query("SELECT autostart_seeded FROM app_settings WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get::<i64, _>("autostart_seeded") == 1)
    }

    pub async fn set_autostart_seeded(&self, seeded: bool) -> Result<(), StorageError> {
        sqlx::query("UPDATE app_settings SET autostart_seeded = ? WHERE id = 1")
            .bind(seeded as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// 读取全局出网代理配置（ADR-017）。
    ///
    /// 库里存的是 mode + url 两列，映射收在这里一处：桌面端与 Hub 都读它，
    /// 各自解释一遍必然漂移。
    ///
    /// 存量数据不可信任时一律退回 `System`（而非报错）：出网配置读不出来就让
    /// 整个应用连不上网，代价远大于「悄悄按默认走」。异常会记进日志。
    pub async fn get_proxy(&self) -> Result<ProxyConfig, StorageError> {
        let row = sqlx::query("SELECT proxy_mode, proxy_url FROM app_settings WHERE id = 1")
            .fetch_one(&self.pool)
            .await?;
        let mode = row.get::<String, _>("proxy_mode");
        let url = row.get::<Option<String>, _>("proxy_url");

        Ok(match (mode.as_str(), url) {
            ("direct", _) => ProxyConfig::Direct,
            ("manual", Some(url)) => ProxyConfig::Manual { url },
            // manual 却没有地址：CHECK 约束管不到两列之间的一致性，真出现了
            // 只能算数据损坏。跟随系统总比让用户彻底出不了网强。
            ("manual", None) => {
                tracing::warn!("代理配置为 manual 但地址为空，已退回跟随系统");
                ProxyConfig::System
            }
            ("system", _) => ProxyConfig::System,
            (unknown, _) => {
                tracing::warn!(mode = unknown, "无法识别的代理模式，已退回跟随系统");
                ProxyConfig::System
            }
        })
    }

    /// 写入全局出网代理配置。非 manual 模式一并清空地址，不留悬挂值。
    pub async fn set_proxy(&self, config: &ProxyConfig) -> Result<(), StorageError> {
        let (mode, url) = match config {
            ProxyConfig::System => ("system", None),
            ProxyConfig::Direct => ("direct", None),
            ProxyConfig::Manual { url } => ("manual", Some(url.as_str())),
        };
        sqlx::query("UPDATE app_settings SET proxy_mode = ?, proxy_url = ? WHERE id = 1")
            .bind(mode)
            .bind(url)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::open_in_memory;

    /// 新装与升级都是「尚未播种」：启动路径据此把默认开启推给系统一次。
    #[tokio::test]
    async fn autostart_seeded_defaults_false_and_roundtrips() {
        let pool = open_in_memory().await.unwrap();
        let repo = SettingsRepo::new(pool);

        assert!(!repo.get_autostart_seeded().await.unwrap());

        repo.set_autostart_seeded(true).await.unwrap();
        assert!(repo.get_autostart_seeded().await.unwrap());

        repo.set_autostart_seeded(false).await.unwrap();
        assert!(!repo.get_autostart_seeded().await.unwrap());
    }

    #[tokio::test]
    async fn hub_enabled_defaults_true_and_roundtrips() {
        let pool = open_in_memory().await.unwrap();
        let repo = SettingsRepo::new(pool);

        assert!(repo.get_hub_enabled().await.unwrap());

        repo.set_hub_enabled(false).await.unwrap();
        assert!(!repo.get_hub_enabled().await.unwrap());

        repo.set_hub_enabled(true).await.unwrap();
        assert!(repo.get_hub_enabled().await.unwrap());
    }

    /// 装完即跟随系统：这与本次改动前 reqwest 的默认行为一致，
    /// 老用户升级上来不该因为多了这两列就改变出网行为。
    #[tokio::test]
    async fn proxy_defaults_to_follow_system() {
        let pool = open_in_memory().await.unwrap();
        let repo = SettingsRepo::new(pool);
        assert_eq!(repo.get_proxy().await.unwrap(), ProxyConfig::System);
    }

    #[tokio::test]
    async fn proxy_roundtrips_every_mode() {
        let pool = open_in_memory().await.unwrap();
        let repo = SettingsRepo::new(pool);

        for config in [
            ProxyConfig::Direct,
            ProxyConfig::Manual {
                url: "http://127.0.0.1:7897".to_owned(),
            },
            ProxyConfig::System,
        ] {
            repo.set_proxy(&config).await.unwrap();
            assert_eq!(repo.get_proxy().await.unwrap(), config);
        }
    }

    /// 从 manual 切回其它模式必须清空地址，否则再切回 manual 会诈尸出旧代理。
    #[tokio::test]
    async fn leaving_manual_clears_the_url() {
        let pool = open_in_memory().await.unwrap();
        let repo = SettingsRepo::new(pool);

        repo.set_proxy(&ProxyConfig::Manual {
            url: "http://127.0.0.1:7897".to_owned(),
        })
        .await
        .unwrap();
        repo.set_proxy(&ProxyConfig::System).await.unwrap();

        let url: Option<String> = sqlx::query("SELECT proxy_url FROM app_settings WHERE id = 1")
            .fetch_one(&repo.pool)
            .await
            .unwrap()
            .get("proxy_url");
        assert_eq!(url, None, "离开 manual 后地址应被清空");
    }

    /// 数据损坏（manual 但无地址）不该让用户彻底出不了网。
    #[tokio::test]
    async fn manual_without_url_falls_back_to_system() {
        let pool = open_in_memory().await.unwrap();
        let repo = SettingsRepo::new(pool.clone());

        sqlx::query("UPDATE app_settings SET proxy_mode = 'manual', proxy_url = NULL WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap();

        assert_eq!(repo.get_proxy().await.unwrap(), ProxyConfig::System);
    }

    /// 枚举之外的模式写不进去——防的是未来加模式时漏改 CHECK 约束。
    #[tokio::test]
    async fn schema_rejects_unknown_mode() {
        let pool = open_in_memory().await.unwrap();
        let result =
            sqlx::query("UPDATE app_settings SET proxy_mode = 'socks-everything' WHERE id = 1")
                .execute(&pool)
                .await;
        assert!(result.is_err(), "CHECK 约束应挡下未知模式");
    }
}

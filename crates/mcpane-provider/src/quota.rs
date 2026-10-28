//! Coding Plan 用量/配额查询（内置厂商查询）。
//!
//! 只内置与 MCPane 预设厂商对应、且能用已存 API Key 直接查询的三家：
//! - Kimi For Coding（Bearer）
//! - 智谱 GLM 个人版（Authorization 不带 Bearer 前缀）
//! - MiniMax（Bearer，国内/国际双域名）
//!
//! 未内置（需要额外凭据形态，待后续）：火山方舟（AK/SK + 火山签名 V4）、
//! 智谱团队版（组织/项目 ID 头）、ZenMux（不在预设集）。
//! 不做自定义用量脚本扩展（需嵌入 JS 引擎，复杂度不成比例）。

use serde_json::Value;

/// 支持配额查询的 Coding Plan 厂商（按 base_url 子串识别，顺序即匹配优先级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CodingPlanProvider {
    Kimi,
    Zhipu,
    Minimax,
}

/// 按 base_url 识别 Coding Plan 厂商；未命中返回 None（= 不支持用量查询）。
pub fn detect_coding_plan(base_url: &str) -> Option<CodingPlanProvider> {
    let url = base_url.to_lowercase();
    if url.contains("api.kimi.com/coding") {
        Some(CodingPlanProvider::Kimi)
    } else if url.contains("bigmodel.cn") || url.contains("api.z.ai") {
        Some(CodingPlanProvider::Zhipu)
    } else if url.contains("api.minimaxi.com") || url.contains("api.minimax.io") {
        Some(CodingPlanProvider::Minimax)
    } else {
        None
    }
}

/// 单个配额窗口。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaTier {
    /// "five-hour" | "weekly"。
    pub window: QuotaWindow,
    /// 已用百分比 0-100（异常值透传，由前端裁剪显示）。
    pub utilization: f64,
    /// 重置时间（Unix 毫秒）。
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuotaWindow {
    FiveHour,
    Weekly,
    Monthly,
}

/// 账户余额（DeepSeek/StepFun/SiliconFlow/OpenRouter/Novita 等按余额计费的中转站）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Balance {
    pub amount: f64,
    pub currency: String,
}

/// 配额查询结果（success=false 时看 error；网络瞬时失败走 Err 由调用方透传）。
/// `tiers` 与 `balances` 互斥：窗口式配额（Coding Plan）填 tiers，
/// 按余额计费的中转站填 balances。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderQuota {
    pub success: bool,
    pub tiers: Vec<QuotaTier>,
    pub balances: Vec<Balance>,
    /// 套餐等级（智谱 level 等），有则展示。
    pub plan_label: Option<String>,
    pub error: Option<String>,
}

impl ProviderQuota {
    pub(crate) fn failure(message: impl Into<String>) -> Self {
        Self {
            success: false,
            tiers: Vec::new(),
            balances: Vec::new(),
            plan_label: None,
            error: Some(message.into()),
        }
    }

    /// 额度是否已用尽及何时恢复，供本地路由把它后置（ADR-022 三期）。
    ///
    /// 只在有确凿数据时下结论：查询失败、无窗口/余额、窗口重置时间已过（数据陈旧）
    /// 一律视为「无信号」，宁可多试一次也不误伤可用的供应商。
    pub fn exhausted_until(&self, now_ms: i64) -> Option<ExhaustedUntil> {
        if !self.success {
            return None;
        }
        let mut blocking_reset = None::<i64>;
        let mut blocked_without_reset = false;
        for tier in self.tiers.iter().filter(|t| t.utilization >= 100.0) {
            match tier.resets_at {
                Some(reset) if reset > now_ms => {
                    blocking_reset = Some(blocking_reset.map_or(reset, |r| r.max(reset)));
                }
                Some(_) => {}
                None => blocked_without_reset = true,
            }
        }
        if let Some(reset) = blocking_reset {
            return Some(ExhaustedUntil::At(reset));
        }
        if blocked_without_reset
            || (!self.balances.is_empty() && self.balances.iter().all(|b| b.amount <= 0.0))
        {
            return Some(ExhaustedUntil::NextRefresh);
        }
        None
    }
}

/// 额度用尽的恢复时点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExhaustedUntil {
    /// 到该时刻（Unix 毫秒）窗口重置。
    At(i64),
    /// 厂商没给重置时间（余额耗尽等），以下一次额度刷新为准。
    NextRefresh,
}

/// 查询厂商配额（api_key 为该供应商在 Keychain 中的 Key 明文）。
pub async fn query_quota(
    provider: CodingPlanProvider,
    base_url: &str,
    api_key: &str,
) -> Result<ProviderQuota, String> {
    if api_key.trim().is_empty() {
        return Ok(ProviderQuota::failure("尚未录入 API Key"));
    }
    // 先定地址、再取客户端：客户端按目标决定走不走代理（ADR-017），
    // 地址没定下来就取不到对的那一个。
    let (url, headers): (String, Vec<(&str, String)>) = match provider {
        CodingPlanProvider::Kimi => (
            "https://api.kimi.com/coding/v1/usages".to_owned(),
            vec![
                ("Authorization", format!("Bearer {api_key}")),
                ("Accept", "application/json".to_owned()),
            ],
        ),
        CodingPlanProvider::Zhipu => {
            // 国内站/国际站二选一（对齐 cc-switch zhipu_quota_base）。
            let quota_base = if base_url.to_lowercase().contains("bigmodel.cn") {
                "https://open.bigmodel.cn"
            } else {
                "https://api.z.ai"
            };
            (
                format!("{quota_base}/api/monitor/usage/quota/limit"),
                vec![
                    // 智谱配额接口的 Authorization 不带 Bearer 前缀。
                    ("Authorization", api_key.to_owned()),
                    ("Content-Type", "application/json".to_owned()),
                    ("Accept-Language", "en-US,en".to_owned()),
                ],
            )
        }
        CodingPlanProvider::Minimax => {
            let domain = if base_url.to_lowercase().contains("api.minimax.io") {
                "api.minimax.io"
            } else {
                "api.minimaxi.com"
            };
            (
                format!("https://{domain}/v1/api/openplatform/coding_plan/remains"),
                vec![
                    ("Authorization", format!("Bearer {api_key}")),
                    ("Content-Type", "application/json".to_owned()),
                ],
            )
        }
    };

    let mut request = crate::http_client(&url).get(&url);
    for (name, value) in headers {
        request = request.header(name, value);
    }

    let response = request
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Ok(ProviderQuota::failure("API Key 无效或已过期"));
    }
    let bytes = response.bytes().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Ok(ProviderQuota::failure(format!("HTTP {status}")));
    }
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(error) => return Ok(ProviderQuota::failure(format!("响应解析失败：{error}"))),
    };
    Ok(match provider {
        CodingPlanProvider::Kimi => parse_kimi(&body),
        CodingPlanProvider::Zhipu => parse_zhipu(&body),
        CodingPlanProvider::Minimax => parse_minimax(&body),
    })
}

/// 兼容数字与字符串两种编码的数值字段（balance.rs 的余额厂商也复用）。
pub(crate) fn as_f64(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

/// 重置时间自适应：秒/毫秒的数字或字符串（对齐 cc-switch extract_reset_time）。
fn as_epoch_ms(value: &Value) -> Option<i64> {
    let raw = as_f64(value)?;
    if raw <= 0.0 {
        return None;
    }
    // 小于 1e12 视为秒级时间戳。
    Some(if raw < 1e12 {
        (raw * 1000.0) as i64
    } else {
        raw as i64
    })
}

fn parse_kimi(body: &Value) -> ProviderQuota {
    let mut tiers = Vec::new();
    // 5 小时窗口：limits[].detail.{limit, remaining, resetTime}。
    if let Some(limits) = body["limits"].as_array() {
        for entry in limits {
            let detail = &entry["detail"];
            if let (Some(limit), Some(remaining)) =
                (as_f64(&detail["limit"]), as_f64(&detail["remaining"]))
                && limit > 0.0
            {
                tiers.push(QuotaTier {
                    window: QuotaWindow::FiveHour,
                    utilization: ((limit - remaining).max(0.0) / limit) * 100.0,
                    resets_at: as_epoch_ms(&detail["resetTime"]),
                });
            }
        }
    }
    // 周窗口：usage.{limit, remaining, resetTime}。
    let usage = &body["usage"];
    if let (Some(limit), Some(remaining)) = (as_f64(&usage["limit"]), as_f64(&usage["remaining"]))
        && limit > 0.0
    {
        tiers.push(QuotaTier {
            window: QuotaWindow::Weekly,
            utilization: ((limit - remaining).max(0.0) / limit) * 100.0,
            resets_at: as_epoch_ms(&usage["resetTime"]),
        });
    }
    if tiers.is_empty() {
        return ProviderQuota::failure("响应中没有配额数据");
    }
    ProviderQuota {
        success: true,
        tiers,
        balances: Vec::new(),
        plan_label: None,
        error: None,
    }
}

fn parse_zhipu(body: &Value) -> ProviderQuota {
    if body["success"] == Value::Bool(false) {
        let message = body["msg"].as_str().unwrap_or("查询失败").to_owned();
        return ProviderQuota::failure(message);
    }
    let mut tiers = Vec::new();
    if let Some(limits) = body["data"]["limits"].as_array() {
        // unit==3 → 5 小时，unit==6 → 周；unit 缺失按 nextResetTime 升序
        // 依次填 5 小时→周（对齐 cc-switch classify_zhipu_window）。
        let mut unclassified: Vec<(f64, Option<i64>)> = Vec::new();
        for entry in limits {
            let is_tokens = entry["type"]
                .as_str()
                .is_some_and(|t| t.eq_ignore_ascii_case("TOKENS_LIMIT"));
            if !is_tokens {
                continue;
            }
            let Some(percentage) = as_f64(&entry["percentage"]) else {
                continue;
            };
            let resets_at = as_epoch_ms(&entry["nextResetTime"]);
            match entry["unit"].as_i64() {
                Some(3) => tiers.push(QuotaTier {
                    window: QuotaWindow::FiveHour,
                    utilization: percentage,
                    resets_at,
                }),
                Some(6) => tiers.push(QuotaTier {
                    window: QuotaWindow::Weekly,
                    utilization: percentage,
                    resets_at,
                }),
                _ => unclassified.push((percentage, resets_at)),
            }
        }
        unclassified.sort_by_key(|(_, resets_at)| resets_at.unwrap_or(i64::MAX));
        for (index, (percentage, resets_at)) in unclassified.into_iter().take(2).enumerate() {
            tiers.push(QuotaTier {
                window: if index == 0 {
                    QuotaWindow::FiveHour
                } else {
                    QuotaWindow::Weekly
                },
                utilization: percentage,
                resets_at,
            });
        }
    }
    if tiers.is_empty() {
        return ProviderQuota::failure("响应中没有配额数据");
    }
    ProviderQuota {
        success: true,
        tiers,
        balances: Vec::new(),
        plan_label: body["data"]["level"].as_str().map(str::to_owned),
        error: None,
    }
}

fn parse_minimax(body: &Value) -> ProviderQuota {
    let status_code = body["base_resp"]["status_code"].as_i64().unwrap_or(0);
    if status_code != 0 {
        let message = body["base_resp"]["status_msg"]
            .as_str()
            .unwrap_or("查询失败")
            .to_owned();
        return ProviderQuota::failure(message);
    }
    let mut tiers = Vec::new();
    if let Some(remains) = body["model_remains"].as_array() {
        for entry in remains {
            // 只取通用模型桶（跳过 video 等）。
            if entry["model_name"].as_str() != Some("general") {
                continue;
            }
            if let Some(remain_pct) = as_f64(&entry["current_interval_remaining_percent"]) {
                tiers.push(QuotaTier {
                    window: QuotaWindow::FiveHour,
                    utilization: 100.0 - remain_pct,
                    resets_at: as_epoch_ms(&entry["end_time"]),
                });
            }
            // 周桶仅在 current_weekly_status == 1 时展示。
            if entry["current_weekly_status"].as_i64() == Some(1)
                && let Some(remain_pct) = as_f64(&entry["current_weekly_remaining_percent"])
            {
                tiers.push(QuotaTier {
                    window: QuotaWindow::Weekly,
                    utilization: 100.0 - remain_pct,
                    resets_at: as_epoch_ms(&entry["weekly_end_time"]),
                });
            }
        }
    }
    if tiers.is_empty() {
        return ProviderQuota::failure("响应中没有配额数据");
    }
    ProviderQuota {
        success: true,
        tiers,
        balances: Vec::new(),
        plan_label: None,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn quota_with(tiers: Vec<QuotaTier>, balances: Vec<Balance>) -> ProviderQuota {
        ProviderQuota {
            success: true,
            tiers,
            balances,
            plan_label: None,
            error: None,
        }
    }

    fn tier(window: QuotaWindow, utilization: f64, resets_at: Option<i64>) -> QuotaTier {
        QuotaTier {
            window,
            utilization,
            resets_at,
        }
    }

    #[test]
    fn exhausted_until_picks_latest_reset_among_full_windows() {
        let quota = quota_with(
            vec![
                tier(QuotaWindow::FiveHour, 100.0, Some(2_000)),
                tier(QuotaWindow::Weekly, 130.0, Some(9_000)),
                tier(QuotaWindow::Monthly, 40.0, Some(50_000)),
            ],
            vec![],
        );
        assert_eq!(
            quota.exhausted_until(1_000),
            Some(ExhaustedUntil::At(9_000))
        );
    }

    #[test]
    fn exhausted_until_ignores_windows_that_already_reset() {
        let quota = quota_with(vec![tier(QuotaWindow::FiveHour, 100.0, Some(500))], vec![]);
        assert_eq!(
            quota.exhausted_until(1_000),
            None,
            "重置时间已过 = 数据陈旧"
        );
        // 边界：99.9% 不算用尽。
        let almost = quota_with(vec![tier(QuotaWindow::FiveHour, 99.9, Some(9_000))], vec![]);
        assert_eq!(almost.exhausted_until(1_000), None);
    }

    #[test]
    fn exhausted_until_full_window_without_reset_waits_for_next_refresh() {
        let quota = quota_with(vec![tier(QuotaWindow::Weekly, 100.0, None)], vec![]);
        assert_eq!(
            quota.exhausted_until(1_000),
            Some(ExhaustedUntil::NextRefresh)
        );
    }

    #[test]
    fn exhausted_until_reads_balances_only_when_all_are_empty() {
        let money = |amount: f64, currency: &str| Balance {
            amount,
            currency: currency.to_owned(),
        };
        let empty = quota_with(vec![], vec![money(0.0, "CNY")]);
        assert_eq!(empty.exhausted_until(0), Some(ExhaustedUntil::NextRefresh));
        let mixed = quota_with(vec![], vec![money(0.0, "CNY"), money(3.5, "USD")]);
        assert_eq!(mixed.exhausted_until(0), None, "任一币种还有余额就不后置");
    }

    #[test]
    fn exhausted_until_has_no_opinion_without_trustworthy_data() {
        assert_eq!(quota_with(vec![], vec![]).exhausted_until(0), None);
        let failed = ProviderQuota::failure("登录过期");
        assert_eq!(failed.exhausted_until(0), None, "查询失败不得惩罚供应商");
    }

    #[test]
    fn detect_matches_preset_base_urls() {
        assert_eq!(
            detect_coding_plan("https://api.kimi.com/coding/"),
            Some(CodingPlanProvider::Kimi)
        );
        assert_eq!(
            detect_coding_plan("https://open.bigmodel.cn/api/anthropic"),
            Some(CodingPlanProvider::Zhipu)
        );
        assert_eq!(
            detect_coding_plan("https://api.z.ai/api/coding/paas/v4"),
            Some(CodingPlanProvider::Zhipu)
        );
        assert_eq!(
            detect_coding_plan("https://api.minimaxi.com/anthropic"),
            Some(CodingPlanProvider::Minimax)
        );
        assert_eq!(
            detect_coding_plan("https://api.minimax.io/v1"),
            Some(CodingPlanProvider::Minimax)
        );
        // 普通 Kimi 开放平台不是 Coding Plan，不支持。
        assert_eq!(
            detect_coding_plan("https://api.moonshot.cn/anthropic"),
            None
        );
        assert_eq!(detect_coding_plan("https://api.deepseek.com"), None);
    }

    #[test]
    fn kimi_response_parses_five_hour_and_weekly() {
        let body = serde_json::json!({
            "limits": [{"detail": {"limit": 100, "remaining": 40, "resetTime": 1760000000}}],
            "usage": {"limit": "1000", "remaining": "900", "resetTime": 1760600000000i64}
        });
        let quota = parse_kimi(&body);
        assert!(quota.success);
        assert_eq!(quota.tiers.len(), 2);
        assert_eq!(quota.tiers[0].window, QuotaWindow::FiveHour);
        assert!((quota.tiers[0].utilization - 60.0).abs() < 1e-9);
        assert_eq!(quota.tiers[0].resets_at, Some(1_760_000_000_000));
        assert_eq!(quota.tiers[1].window, QuotaWindow::Weekly);
        assert!((quota.tiers[1].utilization - 10.0).abs() < 1e-9);
    }

    #[test]
    fn zhipu_response_classifies_by_unit_and_reads_level() {
        let body = serde_json::json!({
            "success": true,
            "data": {
                "level": "pro",
                "limits": [
                    {"type": "TOKENS_LIMIT", "percentage": 12.5, "unit": 3, "nextResetTime": 1760000000000i64},
                    {"type": "TOKENS_LIMIT", "percentage": 42.0, "unit": 6, "nextResetTime": 1760600000000i64},
                    {"type": "OTHER", "percentage": 99.0}
                ]
            }
        });
        let quota = parse_zhipu(&body);
        assert!(quota.success);
        assert_eq!(quota.plan_label.as_deref(), Some("pro"));
        assert_eq!(quota.tiers.len(), 2);
        assert_eq!(quota.tiers[0].window, QuotaWindow::FiveHour);
        assert_eq!(quota.tiers[1].window, QuotaWindow::Weekly);

        // 业务错误信封。
        let error = parse_zhipu(&serde_json::json!({"success": false, "msg": "invalid key"}));
        assert!(!error.success);
        assert_eq!(error.error.as_deref(), Some("invalid key"));
    }

    #[test]
    fn minimax_response_inverts_remaining_percent() {
        let body = serde_json::json!({
            "base_resp": {"status_code": 0},
            "model_remains": [
                {"model_name": "video", "current_interval_remaining_percent": 1},
                {
                    "model_name": "general",
                    "current_interval_remaining_percent": 70.0,
                    "end_time": 1760000000000i64,
                    "current_weekly_status": 1,
                    "current_weekly_remaining_percent": 55.0,
                    "weekly_end_time": 1760600000000i64
                }
            ]
        });
        let quota = parse_minimax(&body);
        assert!(quota.success);
        assert_eq!(quota.tiers.len(), 2);
        assert!((quota.tiers[0].utilization - 30.0).abs() < 1e-9);
        assert!((quota.tiers[1].utilization - 45.0).abs() < 1e-9);

        let error = parse_minimax(
            &serde_json::json!({"base_resp": {"status_code": 1004, "status_msg": "auth failed"}}),
        );
        assert!(!error.success);
        assert_eq!(error.error.as_deref(), Some("auth failed"));
    }
}

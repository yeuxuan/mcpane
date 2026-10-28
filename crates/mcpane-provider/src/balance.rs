//! 中转站账户余额查询（DeepSeek/StepFun/SiliconFlow/OpenRouter/Novita）。
//!
//! 与 quota.rs 的窗口式配额（tiers）不同，这类厂商只暴露账户余额（balances）；
//! 按 base_url 子串识别，与 detect_coding_plan 平级但互斥——service.rs 里
//! 先查 Coding Plan 未命中，再查这里。

use serde_json::Value;

use crate::quota::{Balance, ProviderQuota, as_f64};

/// 支持余额查询的中转站厂商（按 base_url 子串识别）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BalanceProvider {
    DeepSeek,
    StepFun,
    SiliconFlow,
    OpenRouter,
    Novita,
}

/// 按 base_url 识别余额厂商；未命中返回 None。
pub fn detect_balance_provider(base_url: &str) -> Option<BalanceProvider> {
    let url = base_url.to_lowercase();
    if url.contains("deepseek.com") {
        Some(BalanceProvider::DeepSeek)
    } else if url.contains("stepfun.com") || url.contains("stepfun.ai") {
        Some(BalanceProvider::StepFun)
    } else if url.contains("siliconflow.cn") || url.contains("siliconflow.com") {
        Some(BalanceProvider::SiliconFlow)
    } else if url.contains("openrouter.ai") {
        Some(BalanceProvider::OpenRouter)
    } else if url.contains("novita.ai") {
        Some(BalanceProvider::Novita)
    } else {
        None
    }
}

/// 查询账户余额（api_key 为该供应商在 Keychain 中的 Key 明文）。
pub async fn query_balance(
    provider: BalanceProvider,
    base_url: &str,
    api_key: &str,
) -> Result<ProviderQuota, String> {
    if api_key.trim().is_empty() {
        return Ok(ProviderQuota::failure("尚未录入 API Key"));
    }
    let lower = base_url.to_lowercase();
    let url = match provider {
        BalanceProvider::DeepSeek => "https://api.deepseek.com/user/balance".to_owned(),
        BalanceProvider::StepFun => {
            if lower.contains("stepfun.ai") {
                "https://api.stepfun.ai/v1/accounts".to_owned()
            } else {
                "https://api.stepfun.com/v1/accounts".to_owned()
            }
        }
        BalanceProvider::SiliconFlow => {
            if lower.contains("siliconflow.com") {
                "https://api.siliconflow.com/v1/user/info".to_owned()
            } else {
                "https://api.siliconflow.cn/v1/user/info".to_owned()
            }
        }
        BalanceProvider::OpenRouter => "https://openrouter.ai/api/v1/credits".to_owned(),
        BalanceProvider::Novita => "https://api.novita.ai/v3/user/balance".to_owned(),
    };

    let response = crate::http_client(&url)
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
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
        BalanceProvider::DeepSeek => parse_deepseek(&body),
        BalanceProvider::StepFun => parse_stepfun(&body, &lower),
        BalanceProvider::SiliconFlow => parse_siliconflow(&body, &lower),
        BalanceProvider::OpenRouter => parse_openrouter(&body),
        BalanceProvider::Novita => parse_novita(&body),
    })
}

fn success(balances: Vec<Balance>) -> ProviderQuota {
    ProviderQuota {
        success: true,
        tiers: Vec::new(),
        balances,
        plan_label: None,
        error: None,
    }
}

fn parse_deepseek(body: &Value) -> ProviderQuota {
    let balances: Vec<Balance> = body["balance_infos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let currency = entry["currency"].as_str()?.to_owned();
            let amount = as_f64(&entry["total_balance"])?;
            Some(Balance { amount, currency })
        })
        .collect();
    if balances.is_empty() {
        return ProviderQuota::failure("响应中没有余额数据");
    }
    success(balances)
}

fn parse_stepfun(body: &Value, lower_base_url: &str) -> ProviderQuota {
    let Some(amount) = as_f64(&body["balance"]) else {
        return ProviderQuota::failure("响应中没有余额数据");
    };
    let currency = if lower_base_url.contains("stepfun.ai") {
        "USD"
    } else {
        "CNY"
    };
    success(vec![Balance {
        amount,
        currency: currency.to_owned(),
    }])
}

fn parse_siliconflow(body: &Value, lower_base_url: &str) -> ProviderQuota {
    let Some(amount) = as_f64(&body["data"]["totalBalance"]) else {
        return ProviderQuota::failure("响应中没有余额数据");
    };
    let currency = if lower_base_url.contains("siliconflow.com") {
        "USD"
    } else {
        "CNY"
    };
    success(vec![Balance {
        amount,
        currency: currency.to_owned(),
    }])
}

fn parse_openrouter(body: &Value) -> ProviderQuota {
    let (Some(total), Some(usage)) = (
        as_f64(&body["data"]["total_credits"]),
        as_f64(&body["data"]["total_usage"]),
    ) else {
        return ProviderQuota::failure("响应中没有余额数据");
    };
    success(vec![Balance {
        amount: total - usage,
        currency: "USD".to_owned(),
    }])
}

fn parse_novita(body: &Value) -> ProviderQuota {
    // availableBalance 单位是 0.0001 美元（Novita 的定点表示）。
    let Some(raw) = as_f64(&body["availableBalance"]) else {
        return ProviderQuota::failure("响应中没有余额数据");
    };
    success(vec![Balance {
        amount: raw / 10_000.0,
        currency: "USD".to_owned(),
    }])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn detect_matches_all_vendors_and_domains() {
        assert_eq!(
            detect_balance_provider("https://api.deepseek.com/v1"),
            Some(BalanceProvider::DeepSeek)
        );
        assert_eq!(
            detect_balance_provider("https://api.stepfun.com/v1"),
            Some(BalanceProvider::StepFun)
        );
        assert_eq!(
            detect_balance_provider("https://api.stepfun.ai/v1"),
            Some(BalanceProvider::StepFun)
        );
        assert_eq!(
            detect_balance_provider("https://api.siliconflow.cn/v1"),
            Some(BalanceProvider::SiliconFlow)
        );
        assert_eq!(
            detect_balance_provider("https://api.siliconflow.com/v1"),
            Some(BalanceProvider::SiliconFlow)
        );
        assert_eq!(
            detect_balance_provider("https://openrouter.ai/api/v1"),
            Some(BalanceProvider::OpenRouter)
        );
        assert_eq!(
            detect_balance_provider("https://api.novita.ai/v3/openai"),
            Some(BalanceProvider::Novita)
        );
        assert_eq!(detect_balance_provider("https://api.moonshot.cn"), None);
    }

    #[test]
    fn deepseek_response_reads_all_currency_entries() {
        let body = serde_json::json!({
            "is_available": true,
            "balance_infos": [
                {"currency": "CNY", "total_balance": "12.34", "granted_balance": "0", "topped_up_balance": "12.34"},
                {"currency": "USD", "total_balance": 5.5}
            ]
        });
        let quota = parse_deepseek(&body);
        assert!(quota.success);
        assert_eq!(quota.balances.len(), 2);
        assert_eq!(quota.balances[0].currency, "CNY");
        assert!((quota.balances[0].amount - 12.34).abs() < 1e-9);
        assert_eq!(quota.balances[1].currency, "USD");

        let empty = parse_deepseek(&serde_json::json!({"balance_infos": []}));
        assert!(!empty.success);
    }

    #[test]
    fn stepfun_response_picks_currency_by_domain() {
        let body = serde_json::json!({"balance": 88.8});
        let cn = parse_stepfun(&body, "https://api.stepfun.com/v1/accounts");
        assert_eq!(cn.balances[0].currency, "CNY");
        let us = parse_stepfun(&body, "https://api.stepfun.ai/v1/accounts");
        assert_eq!(us.balances[0].currency, "USD");
    }

    #[test]
    fn siliconflow_response_reads_string_total_balance() {
        let body = serde_json::json!({"data": {"totalBalance": "99.90"}});
        let quota = parse_siliconflow(&body, "https://api.siliconflow.cn/v1/user/info");
        assert!(quota.success);
        assert!((quota.balances[0].amount - 99.90).abs() < 1e-9);
        assert_eq!(quota.balances[0].currency, "CNY");
    }

    #[test]
    fn openrouter_response_subtracts_usage_from_credits() {
        let body = serde_json::json!({"data": {"total_credits": 100.0, "total_usage": 37.5}});
        let quota = parse_openrouter(&body);
        assert!(quota.success);
        assert!((quota.balances[0].amount - 62.5).abs() < 1e-9);
        assert_eq!(quota.balances[0].currency, "USD");
    }

    #[test]
    fn novita_response_divides_by_ten_thousand() {
        let body = serde_json::json!({"availableBalance": 123_400});
        let quota = parse_novita(&body);
        assert!(quota.success);
        assert!((quota.balances[0].amount - 12.34).abs() < 1e-9);
    }
}

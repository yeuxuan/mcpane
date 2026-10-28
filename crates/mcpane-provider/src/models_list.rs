//! 拉取供应商可用模型列表（对齐 cc-switch model_fetch.rs）。
//!
//! 纯粹基于 base_url 工作、与客户端 app 无关：一律以 `Authorization: Bearer`
//! 请求 OpenAI 兼容的 `/models` 端点，解析 `data[].id`。多数中转站即便报文端点
//! 要 `x-api-key`，其 `/models` 也接受 Bearer（cc-switch 大量用户验证）。
//!
//! 端点候选按 base_url 形态推导（版本段感知 + 已知兼容子路径剥离），逐个尝试：
//! 404/405/解析失败 → 试下一个；401/403 → 直接判定 Key 无效。部分中转站不暴露
//! `/models`，全部候选失败时返回错误，前端回退为手动填写。

use serde_json::Value;
use std::time::Duration;

/// 已知的「兼容层子路径」后缀（按长度降序，最长前缀优先剥离）：base_url 命中时，
/// 剥掉后缀再从根域推导 `/v1/models`、`/models`（如 DeepSeek 的 `/anthropic`
/// 兼容层挂子路径、`/models` 却在根域）。对齐 cc-switch KNOWN_COMPAT_SUFFIXES。
const KNOWN_COMPAT_SUFFIXES: &[&str] = &[
    "/api/claudecode",
    "/api/anthropic",
    "/apps/anthropic",
    "/api/coding",
    "/claudecode",
    "/anthropic",
    "/step_plan",
    "/coding",
    "/claude",
];

/// 拉取模型 id 列表（升序去重）。api_key 为该供应商 Key 明文；
/// models_url 非空时只用它（覆写候选推导）。
pub async fn fetch_models(
    base_url: &str,
    api_key: &str,
    models_url: Option<&str>,
) -> Result<Vec<String>, String> {
    if api_key.trim().is_empty() {
        return Err("尚未录入 API Key".to_owned());
    }
    let candidates = build_url_candidates(base_url, models_url);
    if candidates.is_empty() {
        return Err("无法从 base_url 推导模型列表地址".to_owned());
    }
    let mut last_err = String::new();
    for url in &candidates {
        match try_fetch(url, api_key).await {
            Ok(ids) => return Ok(ids),
            // 401/403 是确定性失败，无需再试其他候选。
            Err(FetchErr::Fatal(msg)) => return Err(msg),
            Err(FetchErr::Retry(msg)) => last_err = msg,
        }
    }
    Err(if last_err.is_empty() {
        "所有候选地址均未返回模型列表".to_owned()
    } else {
        last_err
    })
}

enum FetchErr {
    /// 该候选不行，试下一个（404/405/网络/解析/空列表）。
    Retry(String),
    /// 确定性失败，中止所有候选（鉴权错误）。
    Fatal(String),
}

async fn try_fetch(url: &str, api_key: &str) -> Result<Vec<String>, FetchErr> {
    let response = crate::http_client(url)
        .get(url)
        // 带一个应用 UA：部分 /models 端点会挡裸 reqwest 默认 UA（对齐 cc-switch
        // 传 User-Agent 的意图）。个别厂商的 UA 白名单（如 Kimi Coding Plan 要求
        // 特定编码代理 UA）此处不复刻，命中失败即回退候选/手动填写。
        .header("User-Agent", concat!("MCPane/", env!("CARGO_PKG_VERSION")))
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| FetchErr::Retry(e.to_string()))?;
    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(FetchErr::Fatal("API Key 无效或无权限".to_owned()));
    }
    if !status.is_success() {
        // 404/405 及其余非 2xx：换下一个候选。
        return Err(FetchErr::Retry(format!("HTTP {status}")));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| FetchErr::Retry(e.to_string()))?;
    let body: Value = serde_json::from_slice(&bytes)
        .map_err(|e| FetchErr::Retry(format!("响应解析失败：{e}")))?;
    // 有 data/models 容器即认定命中 models 端点（哪怕列表为空）——否则首个正确端点
    // 恰好返回空列表时会继续尝试，反被后续错误候选的无关 JSON 顶替。
    match parse_model_ids(&body) {
        Some(ids) => Ok(ids),
        None => Err(FetchErr::Retry("非模型列表响应".to_owned())),
    }
}

/// 候选推导：
/// 1. models_url 覆写非空 → 只用它；
/// 2. base_url 以版本段 `/v{N}` 结尾（`/v1`、智谱 `/api/coding/paas/v4` 等）→ 拼
///    `/models`；版本段非 v1 时再兜底 `/v1/models`；否则直接 `/v1/models`；
/// 3. base_url 命中已知兼容子路径 → 剥离后从根域再补 `/v1/models`、`/models`。
fn build_url_candidates(base_url: &str, models_url: Option<&str>) -> Vec<String> {
    if let Some(url) = models_url.map(str::trim).filter(|u| !u.is_empty()) {
        return vec![url.to_owned()];
    }
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    if ends_with_version_segment(base) {
        push_unique(&mut out, format!("{base}/models"));
        if !base.ends_with("/v1") {
            push_unique(&mut out, format!("{base}/v1/models"));
        }
    } else {
        push_unique(&mut out, format!("{base}/v1/models"));
    }
    if let Some(stripped) = strip_compat_suffix(base) {
        let stripped = stripped.trim_end_matches('/');
        if !stripped.is_empty() {
            push_unique(&mut out, format!("{stripped}/v1/models"));
            push_unique(&mut out, format!("{stripped}/models"));
        }
    }
    out
}

fn push_unique(out: &mut Vec<String>, candidate: String) {
    if !out.contains(&candidate) {
        out.push(candidate);
    }
}

/// 末段是否形如 `v` + 纯数字（`v1`、`v4`）。
fn ends_with_version_segment(url: &str) -> bool {
    url.rsplit('/')
        .next()
        .and_then(|last| last.strip_prefix('v'))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn strip_compat_suffix(base: &str) -> Option<&str> {
    KNOWN_COMPAT_SUFFIXES
        .iter()
        .find_map(|suffix| base.strip_suffix(suffix))
}

/// 从响应提取模型 id（升序去重）。兼容：
/// - OpenAI/Anthropic：`{"data":[{"id":..}]}`；
/// - 其他：`{"models":[{"id"/"name":..}]}` 或 `{"models":["..."]}`（Gemini 的
///   `name` 形如 `models/xxx`，剥前缀）。
///
/// 返回 `Some`（含空列表）= 命中 models 端点；`None` = 无 data/models 容器、
/// 不是模型响应（据此换下一个候选）。
fn parse_model_ids(body: &Value) -> Option<Vec<String>> {
    let mut ids: Vec<String> = Vec::new();
    let mut recognized = false;
    if let Some(arr) = body.get("data").and_then(Value::as_array) {
        recognized = true;
        collect_ids(arr, &mut ids);
    }
    if ids.is_empty()
        && let Some(arr) = body.get("models").and_then(Value::as_array)
    {
        recognized = true;
        collect_ids(arr, &mut ids);
    }
    if !recognized {
        return None;
    }
    ids.sort();
    Some(ids)
}

fn collect_ids(arr: &[Value], ids: &mut Vec<String>) {
    for entry in arr {
        let id = if let Some(s) = entry.as_str() {
            Some(s.to_owned())
        } else if let Some(s) = entry.get("id").and_then(Value::as_str) {
            Some(s.to_owned())
        } else {
            entry
                .get("name")
                .and_then(Value::as_str)
                .map(|s| s.strip_prefix("models/").unwrap_or(s).to_owned())
        };
        if let Some(id) = id
            && !id.is_empty()
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_base_appends_models() {
        // 末段 /v1 → /models（不再多余追加 /v1/models）。
        assert_eq!(
            build_url_candidates("https://api.moonshot.cn/v1", None),
            vec!["https://api.moonshot.cn/v1/models"]
        );
        // 末段 /v4（非 v1）→ /models 再兜底 /v1/models。
        assert_eq!(
            build_url_candidates("https://open.bigmodel.cn/api/coding/paas/v4", None),
            vec![
                "https://open.bigmodel.cn/api/coding/paas/v4/models",
                "https://open.bigmodel.cn/api/coding/paas/v4/v1/models",
            ]
        );
    }

    #[test]
    fn plain_host_appends_v1_models() {
        assert_eq!(
            build_url_candidates("https://api.deepseek.com", None),
            vec!["https://api.deepseek.com/v1/models"]
        );
    }

    #[test]
    fn compat_suffix_stripped_to_root() {
        // Anthropic 兼容层挂 /anthropic 子路径，/models 在根域。
        assert_eq!(
            build_url_candidates("https://api.deepseek.com/anthropic", None),
            vec![
                "https://api.deepseek.com/anthropic/v1/models",
                "https://api.deepseek.com/v1/models",
                "https://api.deepseek.com/models",
            ]
        );
        assert_eq!(
            build_url_candidates("https://open.bigmodel.cn/api/anthropic", None),
            vec![
                "https://open.bigmodel.cn/api/anthropic/v1/models",
                "https://open.bigmodel.cn/v1/models",
                "https://open.bigmodel.cn/models",
            ]
        );
    }

    #[test]
    fn models_url_override_wins() {
        assert_eq!(
            build_url_candidates(
                "https://api.deepseek.com/anthropic",
                Some("https://api.deepseek.com/models")
            ),
            vec!["https://api.deepseek.com/models"]
        );
    }

    #[test]
    fn trailing_slash_and_empty_handled() {
        assert_eq!(
            build_url_candidates("https://api.x.ai/v1/", None),
            vec!["https://api.x.ai/v1/models"]
        );
        assert!(build_url_candidates("   ", None).is_empty());
    }

    #[test]
    fn parses_openai_data_shape() {
        let body = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-5.5", "owned_by": "openai"},
                {"id": "gpt-4", "object": "model"},
            ]
        });
        // 升序排序。
        assert_eq!(
            parse_model_ids(&body),
            Some(vec!["gpt-4".to_owned(), "gpt-5.5".to_owned()])
        );
    }

    #[test]
    fn parses_models_array_and_gemini_name() {
        // {"models":[{"name":"models/xxx"}]} → 剥 models/ 前缀。
        let gemini = serde_json::json!({
            "models": [{"name": "models/gemini-3.5-flash"}, {"name": "models/gemini-3.5-pro"}]
        });
        assert_eq!(
            parse_model_ids(&gemini),
            Some(vec![
                "gemini-3.5-flash".to_owned(),
                "gemini-3.5-pro".to_owned()
            ])
        );
        // 纯字符串数组。
        let plain = serde_json::json!({"models": ["b-model", "a-model", "b-model"]});
        assert_eq!(
            parse_model_ids(&plain),
            Some(vec!["a-model".to_owned(), "b-model".to_owned()])
        );
    }

    #[test]
    fn empty_container_is_recognized_but_unknown_shape_is_none() {
        // 有 data 容器但为空 → 命中端点、空列表（Some(空)），不再顺延候选。
        assert_eq!(
            parse_model_ids(&serde_json::json!({"data": []})),
            Some(Vec::<String>::new())
        );
        // 无 data/models 容器 → 不是模型响应（None），换下一个候选。
        assert_eq!(parse_model_ids(&serde_json::json!({"foo": "bar"})), None);
    }
}

//! JSON5 文档模型（保格式）。
//!
//! OpenClaw 的 `~/.openclaw/openclaw.json` 是 JSON5——无引号键、单引号字符串、
//! 注释、尾逗号都合法（它自带的初始配置就长这样：`{ models: { mode: 'merge' } }`）。
//! 用 serde_json 读它直接解析失败；就算能读，写回也会把用户手写的注释和风格
//! 全抹平。所以这里走 `json-five` 的 round-trip 解析器——地位等同 TOML 侧的
//! `toml_edit`：改哪个键就只动哪个键，其余字节原样保留。
//!
//! 新插入的键统一用双引号 + 紧凑值（我们没有立场替用户猜他偏好哪种风格），
//! 但**已存在的键保留原有形态**：改一个单引号字符串的值，它仍是单引号。

use json_five::rt::parser::{JSONKeyValuePair, JSONText, JSONValue, KeyValuePairContext};

use crate::error::ConfigError;

/// 解析 JSON5 文本。空文本视为空对象（配置文件初次创建前可能不存在/为空）。
pub(crate) fn parse(content: &str) -> Result<JSONText, ConfigError> {
    if content.trim().is_empty() {
        return Ok(JSONText {
            value: empty_object(),
            context: None,
        });
    }
    json_five::rt::parser::from_str(content).map_err(|e| ConfigError::Parse {
        format: "json5",
        message: e.to_string(),
    })
}

pub(crate) fn serialize(doc: &JSONText) -> String {
    let mut text = doc.value.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// 序列化后立刻重解析，产物读不回来就报错。
///
/// 「自己刚写出来的文本一定能读回来」在这一路**不成立**：json-five 0.3.1 的
/// 词法器给块注释算错了 token 边界——`tokenize.rs` 里行注释返回 `last_idx + 1`
/// （右开），块注释却返回 `last_idx`，闭合的那个 `/` 被切在区间外。于是
/// `/* 注释 */` 一经解析就变成 `/* 注释 *`，写回去是一份未闭合注释的文件，
/// 用户的 OpenClaw 从此读不了自己的配置。0.3.1 已是最新发行版（2026-01-08 核对），
/// 上游无修复；文本侧也无法无损修补（注释体本身可能含 `*`，闭合位置不可判定）。
///
/// 所以这一层只做一件事：**发现往返有损就停下**。配置文件原样不动，用户拿到
/// 一条明确错误，而不是一份被写坏（甚至被清空）的配置。不含块注释的 JSON5
/// ——包括 OpenClaw 自带的那份初始配置——照常工作。
pub(crate) fn reparse_own_output(doc: &JSONText) -> Result<JSONText, ConfigError> {
    let text = serialize(doc);
    parse(&text).map_err(|error| ConfigError::LossySerialize {
        format: "json5",
        message: if text.contains("/*") {
            format!(
                "配置含块注释（/* ... */），当前 JSON5 解析器无法无损往返，\
                 已放弃写入以免损坏文件；改成行注释（// ...）即可正常写入。原始错误：{error}"
            )
        } else {
            error.to_string()
        },
    })
}

/// 按路径读取，转成 serde_json 值（Diff 与上层统一用 JSON 表示）。
pub(crate) fn get(doc: &JSONText, path: &[&str]) -> Option<serde_json::Value> {
    let mut cur = &doc.value;
    for segment in path {
        cur = pairs(cur)?
            .iter()
            .find(|pair| key_name(&pair.key).as_deref() == Some(*segment))
            .map(|pair| &pair.value)?;
    }
    Some(to_json(cur))
}

/// 按路径写入。中间层缺失时补建对象；已存在的键只换值，保留其键形态与周边格式。
pub(crate) fn set(doc: &mut JSONText, path: &[String], value: &serde_json::Value) {
    if path.is_empty() {
        doc.value = from_json(value);
        return;
    }
    ensure_object(&mut doc.value);
    let mut cur = &mut doc.value;
    for segment in &path[..path.len() - 1] {
        cur = child_object_mut(cur, segment);
    }
    let last = &path[path.len() - 1];
    let new_value = from_json(value);
    let Some(list) = pairs_mut(cur) else { return };
    if let Some(existing) = list
        .iter_mut()
        .find(|pair| key_name(&pair.key).as_deref() == Some(last.as_str()))
    {
        existing.value = new_value;
    } else {
        append_pair(list, last, new_value);
    }
}

/// 按路径删除；路径不存在时静默返回（Remove 是幂等的）。
pub(crate) fn remove(doc: &mut JSONText, path: &[String]) {
    if path.is_empty() {
        return;
    }
    let mut cur = &mut doc.value;
    for segment in &path[..path.len() - 1] {
        let Some(list) = pairs_mut(cur) else { return };
        let Some(next) = list
            .iter_mut()
            .find(|pair| key_name(&pair.key).as_deref() == Some(segment.as_str()))
        else {
            return;
        };
        cur = &mut next.value;
    }
    let last = &path[path.len() - 1];
    if let Some(list) = pairs_mut(cur) {
        // 删的是不是末位项——决定要不要善后逗号（见下）。
        let dropped_tail =
            list.last().and_then(|pair| key_name(&pair.key)).as_deref() == Some(last.as_str());
        list.retain(|pair| key_name(&pair.key).as_deref() != Some(last.as_str()));
        // 删掉末位项会把它前面那个逗号留成尾逗号，还带着本属于被删项的缩进：
        // `{ "a": 1,\n  }`。JSON5 容忍这种写法，但同一份文件的其他消费方
        // （jq、用户脚本，以及 opencode.json 这种官方声明 JSON/JSONC 双支持的
        // 配置）会当语法错误。末位项的逗号连同其后空白一并清掉。
        if dropped_tail
            && let Some(tail) = list.last_mut()
            && let Some(context) = tail.context.as_mut()
        {
            context.wsc.3 = None;
        }
    }
}

// ---------- 内部助手 ----------

fn empty_object() -> JSONValue {
    JSONValue::JSONObject {
        key_value_pairs: Vec::new(),
        context: None,
    }
}

fn pairs(value: &JSONValue) -> Option<&Vec<JSONKeyValuePair>> {
    match value {
        JSONValue::JSONObject {
            key_value_pairs, ..
        } => Some(key_value_pairs),
        _ => None,
    }
}

fn pairs_mut(value: &mut JSONValue) -> Option<&mut Vec<JSONKeyValuePair>> {
    match value {
        JSONValue::JSONObject {
            key_value_pairs, ..
        } => Some(key_value_pairs),
        _ => None,
    }
}

/// 键名（三种合法键形态都要认：无引号标识符、双引号、单引号）。
fn key_name(key: &JSONValue) -> Option<String> {
    match key {
        JSONValue::Identifier(name)
        | JSONValue::DoubleQuotedString(name)
        | JSONValue::SingleQuotedString(name) => Some(name.clone()),
        _ => None,
    }
}

/// 在对象末尾追加键值对。
///
/// `KeyValuePairContext.wsc` 的四段是 `key {0} : {1} value {2} [, {3}]`——
/// 注意**没有"键前"这一段**：新项的前导空白属于**前一项逗号之后**（wsc.3），
/// 空对象则属于对象自己的 `context.wsc.0`。搞错这点会拼出 `"k"\n :v` 这种
/// 冒号跑到行首的非法文本（实测踩过）。
/// 前一项没有逗号时（JSON5 允许省略尾逗号，原文末项通常就没有）必须补上，
/// 否则两项会粘成 `a: 1 "b": 2`。
fn append_pair(list: &mut Vec<JSONKeyValuePair>, key: &str, value: JSONValue) {
    const INDENT: &str = "\n  ";
    if let Some(previous) = list.last_mut() {
        match previous.context.as_mut() {
            // 已有逗号：其后的空白就是新项的前导，原样沿用（缩进自然跟上文一致）。
            Some(context) if context.wsc.3.is_some() => {}
            Some(context) => context.wsc.3 = Some(INDENT.to_owned()),
            None => {
                previous.context = Some(KeyValuePairContext {
                    wsc: (
                        String::new(),
                        String::new(),
                        String::new(),
                        Some(INDENT.to_owned()),
                    ),
                });
            }
        }
    }
    list.push(new_pair(key, value));
}

/// 新键值对：双引号键 + `: ` 分隔。前导空白由 [`append_pair`] 负责（见其文档）。
fn new_pair(key: &str, value: JSONValue) -> JSONKeyValuePair {
    JSONKeyValuePair {
        key: JSONValue::DoubleQuotedString(key.to_owned()),
        value,
        context: Some(KeyValuePairContext {
            wsc: (String::new(), " ".to_owned(), String::new(), None),
        }),
    }
}

/// 取（必要时补建）子对象的可变引用。非对象值会被替换为空对象——
/// 写入路径要求父级是对象，标量挡路时只能让位。
fn child_object_mut<'a>(parent: &'a mut JSONValue, key: &str) -> &'a mut JSONValue {
    ensure_object(parent);
    // ensure_object 刚把它变成对象，这里必然拿得到；用 match 而不是 expect，
    // 免得为一个不可能的分支引入 panic 点。
    let list = match parent {
        JSONValue::JSONObject {
            key_value_pairs, ..
        } => key_value_pairs,
        other => return other,
    };
    let index = list
        .iter()
        .position(|pair| key_name(&pair.key).as_deref() == Some(key));
    let index = match index {
        Some(i) => i,
        None => {
            append_pair(list, key, empty_object());
            list.len() - 1
        }
    };
    let slot = &mut list[index].value;
    ensure_object(slot);
    slot
}

fn ensure_object(value: &mut JSONValue) {
    if !matches!(value, JSONValue::JSONObject { .. }) {
        *value = empty_object();
    }
}

fn to_json(value: &JSONValue) -> serde_json::Value {
    match value {
        JSONValue::JSONObject {
            key_value_pairs, ..
        } => serde_json::Value::Object(
            key_value_pairs
                .iter()
                .filter_map(|pair| key_name(&pair.key).map(|k| (k, to_json(&pair.value))))
                .collect(),
        ),
        JSONValue::JSONArray { values, .. } => {
            serde_json::Value::Array(values.iter().map(|item| to_json(&item.value)).collect())
        }
        JSONValue::Integer(raw) | JSONValue::Hexadecimal(raw) => raw
            .parse::<i64>()
            .ok()
            .or_else(|| i64::from_str_radix(raw.trim_start_matches("0x"), 16).ok())
            .map(Into::into)
            .unwrap_or(serde_json::Value::Null),
        JSONValue::Float(raw) | JSONValue::Exponent(raw) => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        JSONValue::Bool(b) => serde_json::Value::Bool(*b),
        JSONValue::DoubleQuotedString(s)
        | JSONValue::SingleQuotedString(s)
        | JSONValue::Identifier(s) => serde_json::Value::String(s.clone()),
        // Null / NaN / Infinity / Unary：JSON 里没有对应表示，统一按 null 读。
        _ => serde_json::Value::Null,
    }
}

fn from_json(value: &serde_json::Value) -> JSONValue {
    match value {
        serde_json::Value::Null => JSONValue::Null,
        serde_json::Value::Bool(b) => JSONValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if n.is_f64() && n.as_i64().is_none() {
                JSONValue::Float(n.to_string())
            } else {
                JSONValue::Integer(n.to_string())
            }
        }
        serde_json::Value::String(s) => JSONValue::DoubleQuotedString(s.clone()),
        // 数组/对象：**每个非末项都必须显式带逗号**。context 为 None 时
        // json-five 不会输出分隔符，多元素会粘成 `{"a": 1 "b": 2}` 这种非法
        // 文本（单元素时恰好看不出来，实测踩过）。
        serde_json::Value::Array(items) => JSONValue::JSONArray {
            values: items
                .iter()
                .enumerate()
                .map(|(i, item)| json_five::rt::parser::JSONArrayValue {
                    value: from_json(item),
                    context: Some(json_five::rt::parser::ArrayValueContext {
                        wsc: (String::new(), (i + 1 < items.len()).then(|| " ".to_owned())),
                    }),
                })
                .collect(),
            context: None,
        },
        serde_json::Value::Object(map) => JSONValue::JSONObject {
            key_value_pairs: map
                .iter()
                .enumerate()
                .map(|(i, (k, v))| JSONKeyValuePair {
                    key: JSONValue::DoubleQuotedString(k.clone()),
                    value: from_json(v),
                    context: Some(KeyValuePairContext {
                        wsc: (
                            String::new(),
                            " ".to_owned(),
                            String::new(),
                            (i + 1 < map.len()).then(|| " ".to_owned()),
                        ),
                    }),
                })
                .collect(),
            context: None,
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// OpenClaw 自带的初始配置就是这个样子：无引号键 + 单引号 + 尾逗号。
    const OPENCLAW_DEFAULT: &str =
        "{\n  models: {\n    mode: 'merge',\n    providers: {},\n  },\n}\n";

    #[test]
    fn reads_unquoted_keys_and_single_quotes() {
        let doc = parse(OPENCLAW_DEFAULT).unwrap();
        assert_eq!(get(&doc, &["models", "mode"]).unwrap(), "merge");
        assert!(get(&doc, &["models", "providers"]).unwrap().is_object());
        assert!(get(&doc, &["nope"]).is_none());
    }

    #[test]
    fn edit_preserves_untouched_bytes() {
        let source =
            "{\n  // 我手写的注释\n  models: {\n    mode: 'merge',\n    providers: {},\n  },\n}\n";
        let mut doc = parse(source).unwrap();
        set(
            &mut doc,
            &["models".into(), "providers".into(), "kimi".into()],
            &serde_json::json!({ "baseUrl": "https://api.moonshot.cn/v1" }),
        );
        let out = serialize(&doc);
        assert!(out.contains("// 我手写的注释"), "注释必须留着：{out}");
        assert!(out.contains("mode: 'merge'"), "单引号与无引号键不动：{out}");
        assert!(out.contains("kimi"), "新键已写入：{out}");
        // 写完还能读回来（round-trip 合法）。
        let doc = parse(&out).unwrap();
        assert_eq!(
            get(&doc, &["models", "providers", "kimi", "baseUrl"]).unwrap(),
            "https://api.moonshot.cn/v1"
        );
    }

    #[test]
    fn set_existing_key_keeps_its_quote_style() {
        let mut doc = parse("{\n  mode: 'merge',\n}\n").unwrap();
        set(&mut doc, &["mode".into()], &serde_json::json!("replace"));
        let out = serialize(&doc);
        assert!(out.contains("mode:"), "键形态不变（仍无引号）：{out}");
        assert_eq!(get(&parse(&out).unwrap(), &["mode"]).unwrap(), "replace");
    }

    #[test]
    fn remove_is_idempotent_and_scoped() {
        let mut doc = parse("{ a: 1, b: { c: 2, d: 3 } }").unwrap();
        remove(&mut doc, &["b".into(), "c".into()]);
        remove(&mut doc, &["b".into(), "c".into()]); // 再删一次不报错
        remove(&mut doc, &["nope".into(), "deep".into()]);
        let out = serialize(&doc);
        let doc = parse(&out).unwrap();
        assert!(get(&doc, &["b", "c"]).is_none());
        assert_eq!(get(&doc, &["b", "d"]).unwrap(), 3);
        assert_eq!(get(&doc, &["a"]).unwrap(), 1);
    }

    /// 删掉末位键不留尾逗号：JSON5 容忍 `{a: 1,}`，但同一份配置的其他消费方
    /// （jq、用户脚本，以及官方声明 JSON/JSONC 双支持的 opencode.json）不容忍。
    /// 加键再删键要能逐字回到原文。
    #[test]
    fn removing_tail_key_leaves_no_trailing_comma() {
        const SRC: &str = r#"{"provider":{"mine":{"npm":"@ai-sdk/anthropic"}},"mcp":{"x":{}}}"#;
        let mut doc = parse(SRC).unwrap();
        set(
            &mut doc,
            &["provider".into(), "kimi".into(), "npm".into()],
            &serde_json::json!("@ai-sdk/openai-compatible"),
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(&serialize(&doc)).is_ok(),
            "新增键之后仍是合法严格 JSON：{}",
            serialize(&doc)
        );

        remove(&mut doc, &["provider".into(), "kimi".into()]);
        let out = serialize(&doc);
        // serialize 会补规范的末尾换行，除此之外要逐字还原。
        assert_eq!(
            out,
            format!("{SRC}\n"),
            "删回去要逐字还原，不留尾逗号与孤立缩进"
        );
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok());
    }

    /// 删非末位键时不该动末位项自己的逗号风格（用户原本写的尾逗号保留）。
    #[test]
    fn removing_middle_key_keeps_existing_trailing_comma() {
        let mut doc = parse("{ a: 1, b: 2, }").unwrap();
        remove(&mut doc, &["a".into()]);
        let out = serialize(&doc);
        assert!(out.contains("2,"), "用户自己的尾逗号风格不动：{out}");
        assert_eq!(get(&parse(&out).unwrap(), &["b"]).unwrap(), 2);
    }

    /// 多键对象 / 多元素数组写出去必须仍是合法 JSON5——键之间要有逗号。
    #[test]
    fn multi_key_values_round_trip() {
        let mut doc = parse("{}").unwrap();
        set(
            &mut doc,
            &["p".into()],
            &serde_json::json!({
                "baseUrl": "https://x",
                "apiKey": "sk",
                "models": [{ "id": "a", "name": "A" }, { "id": "b", "name": "B" }],
            }),
        );
        let out = serialize(&doc);
        let doc = parse(&out).unwrap_or_else(|e| panic!("写出的文本必须能重解析：{e}\n{out}"));
        assert_eq!(get(&doc, &["p", "apiKey"]).unwrap(), "sk");
        assert_eq!(get(&doc, &["p", "models"]).unwrap()[1]["name"], "B");
    }

    #[test]
    fn creates_missing_intermediate_objects() {
        let mut doc = parse("").unwrap();
        set(
            &mut doc,
            &["a".into(), "b".into()],
            &serde_json::json!(true),
        );
        let doc = parse(&serialize(&doc)).unwrap();
        assert_eq!(get(&doc, &["a", "b"]).unwrap(), true);
    }
}

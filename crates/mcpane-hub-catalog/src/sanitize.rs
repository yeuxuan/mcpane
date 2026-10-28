//! 下游工具 schema 规范化（ADR-020）。
//!
//! 聚合网关的职责之一：**一个坏下游不能毒死整张聚合表**。真机与生态实证了
//! 三类「规范合法但客户端吃不下」的 wire 形态（anthropics/claude-code#25081、
//! #45106、#10031）：
//!
//! 1. 布尔 schema（`properties` 成员为 `true`/`false`）——Claude Code 的解析层
//!    直接失败并**静默丢弃该 server 的全部工具**；
//! 2. 顶层缺 `"type":"object"`——MCP 规范要求工具 schema 顶层为 object，
//!    严格校验的客户端按无效处理；
//! 3. 顶层 `anyOf`/`oneOf`/`allOf`——Claude API 直接 400，且客户端每次请求都
//!    带全量工具表，**会话永久不可恢复**。
//!
//! 4. 根级 `$schema` 声明了非 2020-12 的方言（draft-07 是生态默认，真机 44 个
//!    下游工具里 30+ 个如此）——客户端校验器只认 2020-12，调用前即整工具拒收。
//!
//! 社区的通行 workaround 是在客户端与 server 之间手写 stdio 代理做同样的事；
//! Hub 站在聚合位上内建它。1、2、4 无损；3 有损（析取约束被放宽为
//! properties 并集 + required 交集，与社区修复 PR 同法），有损处必发告警。

/// 布尔 schema 会出现且已证实致命的容器键：这些键的 map 成员应为 schema。
const SCHEMA_MAP_KEYS: &[&str] = &["properties", "patternProperties", "$defs", "definitions"];

/// 成员应为 schema 的数组容器键。
const SCHEMA_ARRAY_KEYS: &[&str] = &["anyOf", "oneOf", "allOf", "prefixItems"];

/// 值本身应为单个 schema 的键。`additionalProperties`/`unevaluatedProperties`
/// **刻意不在列**：其布尔形态是被广泛支持的生态惯例，动它反而制造差异。
const SCHEMA_VALUE_KEYS: &[&str] = &[
    "items",
    "contains",
    "propertyNames",
    "not",
    "if",
    "then",
    "else",
];

/// 就地规范化一个工具 schema（input 或 output），返回人读的改动记录
/// （空 = 未动；非空由调用方决定告警级别）。
pub fn sanitize_tool_schema(schema: &mut serde_json::Value) -> Vec<String> {
    let mut changes = Vec::new();
    strip_foreign_dialect(schema);
    normalize_booleans(schema, "", &mut changes);
    flatten_root_combinators(schema, &mut changes);
    ensure_root_object_type(schema, &mut changes);
    changes
}

/// MCP 缺省方言即 2020-12：根级 `$schema` 指向其他方言时直接摘掉。无损且极常见，
/// 不计入 changes（否则每次重建 Catalog 都对几十个工具刷 warn）。
fn strip_foreign_dialect(schema: &mut serde_json::Value) {
    const DIALECT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";
    let Some(root) = schema.as_object_mut() else {
        return;
    };
    let foreign = root.get("$schema").is_some_and(|v| {
        v.as_str()
            .is_none_or(|s| s.trim_end_matches('#') != DIALECT_2020_12)
    });
    if foreign {
        root.remove("$schema");
    }
}

/// 布尔 schema → 语义等价的对象形态（`true`→`{}`，`false`→`{"not":{}}`）。
fn normalize_booleans(value: &mut serde_json::Value, path: &str, changes: &mut Vec<String>) {
    let Some(map) = value.as_object_mut() else {
        if let Some(items) = value.as_array_mut() {
            for (i, item) in items.iter_mut().enumerate() {
                normalize_booleans(item, &format!("{path}[{i}]"), changes);
            }
        }
        return;
    };
    for (key, entry) in map.iter_mut() {
        let child_path = format!("{path}.{key}");
        if SCHEMA_MAP_KEYS.contains(&key.as_str())
            && let Some(members) = entry.as_object_mut()
        {
            for (name, member) in members.iter_mut() {
                replace_bool_schema(member, &format!("{child_path}.{name}"), changes);
                normalize_booleans(member, &format!("{child_path}.{name}"), changes);
            }
            continue;
        }
        if SCHEMA_ARRAY_KEYS.contains(&key.as_str())
            && let Some(members) = entry.as_array_mut()
        {
            for (i, member) in members.iter_mut().enumerate() {
                replace_bool_schema(member, &format!("{child_path}[{i}]"), changes);
                normalize_booleans(member, &format!("{child_path}[{i}]"), changes);
            }
            continue;
        }
        if SCHEMA_VALUE_KEYS.contains(&key.as_str()) {
            replace_bool_schema(entry, &child_path, changes);
        }
        normalize_booleans(entry, &child_path, changes);
    }
}

fn replace_bool_schema(value: &mut serde_json::Value, path: &str, changes: &mut Vec<String>) {
    match value {
        serde_json::Value::Bool(true) => {
            *value = serde_json::json!({});
            changes.push(format!("{path}: 布尔 schema true → {{}}"));
        }
        serde_json::Value::Bool(false) => {
            *value = serde_json::json!({"not": {}});
            changes.push(format!("{path}: 布尔 schema false → {{\"not\":{{}}}}"));
        }
        _ => {}
    }
}

/// 顶层 `anyOf`/`oneOf`/`allOf` 降级（Claude API 不接受顶层组合子）：
/// properties 取并集（先到先得，不覆盖顶层已有的）；required 对 allOf 取并集
/// （合取语义），对 anyOf/oneOf 取各分支交集（析取下只有交集是必然必填）。
/// 析取降级是有损放宽——记入 changes 由调用方告警。
fn flatten_root_combinators(schema: &mut serde_json::Value, changes: &mut Vec<String>) {
    let Some(root) = schema.as_object_mut() else {
        return;
    };
    for key in ["allOf", "anyOf", "oneOf"] {
        let Some(branches) = root.remove(key) else {
            continue;
        };
        let Some(branches) = branches.as_array() else {
            root.insert(key.to_owned(), branches);
            continue;
        };
        let mut merged_properties = serde_json::Map::new();
        let mut branch_required: Vec<Vec<String>> = Vec::new();
        for branch in branches {
            if let Some(properties) = branch.get("properties").and_then(|p| p.as_object()) {
                for (name, member) in properties {
                    merged_properties
                        .entry(name.clone())
                        .or_insert_with(|| member.clone());
                }
            }
            branch_required.push(
                branch
                    .get("required")
                    .and_then(|r| r.as_array())
                    .map(|r| {
                        r.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default(),
            );
        }
        // properties 并入顶层（不覆盖既有项）。
        if !merged_properties.is_empty() {
            let target = root
                .entry("properties")
                .or_insert_with(|| serde_json::json!({}));
            if let Some(target) = target.as_object_mut() {
                for (name, member) in merged_properties {
                    target.entry(name).or_insert(member);
                }
            }
        }
        // required 合并：合取并集 / 析取交集，再并入顶层既有 required。
        let combined: Vec<String> = if key == "allOf" {
            let mut all: Vec<String> = branch_required.into_iter().flatten().collect();
            all.dedup();
            all
        } else {
            branch_required.iter().skip(1).fold(
                branch_required.first().cloned().unwrap_or_default(),
                |acc, next| acc.into_iter().filter(|f| next.contains(f)).collect(),
            )
        };
        if !combined.is_empty() {
            let target = root
                .entry("required")
                .or_insert_with(|| serde_json::json!([]));
            if let Some(target) = target.as_array_mut() {
                for field in combined {
                    if !target.iter().any(|v| v.as_str() == Some(field.as_str())) {
                        target.push(serde_json::Value::String(field));
                    }
                }
            }
        }
        changes.push(if key == "allOf" {
            format!(".{key}: 顶层组合子已合并展开（合取，无损）")
        } else {
            format!(".{key}: 顶层组合子已降级展开（析取约束被放宽，有损）")
        });
    }
}

/// MCP 规范要求工具 schema 顶层为 object；缺 `type` 时补上。
fn ensure_root_object_type(schema: &mut serde_json::Value, changes: &mut Vec<String>) {
    if let Some(root) = schema.as_object_mut()
        && !root.contains_key("type")
    {
        root.insert("type".to_owned(), serde_json::json!("object"));
        changes.push(".type: 顶层缺 type，已补 \"object\"".to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn boolean_schemas_become_equivalent_objects() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "anything": true,
                "nothing": false,
                "nested": {"type": "object", "properties": {"inner": true}}
            },
            "$defs": {"Free": true}
        });
        let changes = sanitize_tool_schema(&mut schema);
        assert_eq!(schema["properties"]["anything"], json!({}));
        assert_eq!(schema["properties"]["nothing"], json!({"not": {}}));
        assert_eq!(
            schema["properties"]["nested"]["properties"]["inner"],
            json!({})
        );
        assert_eq!(schema["$defs"]["Free"], json!({}));
        assert_eq!(changes.len(), 4, "{changes:?}");
    }

    #[test]
    fn additional_properties_boolean_is_left_alone() {
        // 生态惯例形态，动它反而制造差异。
        let mut schema = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": false
        });
        let changes = sanitize_tool_schema(&mut schema);
        assert_eq!(schema["additionalProperties"], json!(false));
        assert!(changes.is_empty(), "{changes:?}");
    }

    #[test]
    fn missing_root_type_gets_object() {
        let mut schema = json!({"properties": {"a": {"type": "string"}}});
        sanitize_tool_schema(&mut schema);
        assert_eq!(schema["type"], "object");
    }

    /// anthropics/claude-code#45106 的真实案例（Fantastical modifyCalendarItem）：
    /// 顶层 anyOf 会让 Claude API 400 且会话不可恢复。
    #[test]
    fn root_anyof_is_flattened_with_required_intersection() {
        let mut schema = json!({
            "type": "object",
            "anyOf": [
                {"required": ["id", "title"]},
                {"required": ["id", "when"]}
            ],
            "properties": {
                "id": {"type": "string"},
                "title": {"type": "string"},
                "when": {"type": "string"}
            }
        });
        let changes = sanitize_tool_schema(&mut schema);
        assert!(schema.get("anyOf").is_none());
        assert_eq!(schema["required"], json!(["id"]), "析取取交集");
        assert!(changes.iter().any(|c| c.contains("有损")), "{changes:?}");
    }

    #[test]
    fn root_allof_merges_properties_and_required_union() {
        let mut schema = json!({
            "allOf": [
                {"properties": {"a": {"type": "string"}}, "required": ["a"]},
                {"properties": {"b": {"type": "integer"}}, "required": ["b"]}
            ]
        });
        let changes = sanitize_tool_schema(&mut schema);
        assert!(schema.get("allOf").is_none());
        assert_eq!(schema["required"], json!(["a", "b"]), "合取取并集");
        assert_eq!(schema["properties"]["a"]["type"], "string");
        assert_eq!(schema["properties"]["b"]["type"], "integer");
        assert_eq!(schema["type"], "object");
        assert!(changes.iter().any(|c| c.contains("无损")), "{changes:?}");
    }

    /// firecrawl 等下游的真实形态：客户端只认 2020-12，带 draft-07 声明会整工具拒收。
    #[test]
    fn foreign_dialect_declaration_is_stripped_silently() {
        for dialect in [
            "http://json-schema.org/draft-07/schema#",
            "http://json-schema.org/draft-07/schema",
            "http://json-schema.org/draft-04/schema#",
            "https://json-schema.org/draft/2019-09/schema",
        ] {
            let mut schema = json!({
                "$schema": dialect,
                "type": "object",
                "properties": {"q": {"type": "string"}}
            });
            let changes = sanitize_tool_schema(&mut schema);
            assert!(schema.get("$schema").is_none(), "{dialect}");
            assert_eq!(schema["properties"]["q"]["type"], "string");
            assert!(changes.is_empty(), "无损改动不应告警：{changes:?}");
        }
    }

    #[test]
    fn dialect_2020_12_declaration_is_kept_with_or_without_hash() {
        for dialect in [
            "https://json-schema.org/draft/2020-12/schema",
            "https://json-schema.org/draft/2020-12/schema#",
        ] {
            let mut schema = json!({"$schema": dialect, "type": "object"});
            sanitize_tool_schema(&mut schema);
            assert_eq!(schema["$schema"], dialect);
        }
    }

    #[test]
    fn non_string_dialect_is_stripped_and_nested_declarations_are_left() {
        let mut schema = json!({
            "$schema": 7,
            "type": "object",
            "properties": {"sub": {"$schema": "http://json-schema.org/draft-07/schema#", "type": "string"}}
        });
        sanitize_tool_schema(&mut schema);
        assert!(schema.get("$schema").is_none());
        // 只有根级声明决定校验方言，嵌套的不动。
        assert!(schema["properties"]["sub"].get("$schema").is_some());
    }

    #[test]
    fn clean_schema_is_untouched() {
        let original = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "tags": {"type": "array", "items": {"type": "string"}},
                "mode": {"anyOf": [{"const": "a"}, {"const": "b"}]}
            },
            "required": ["query"],
            "additionalProperties": false
        });
        let mut schema = original.clone();
        let changes = sanitize_tool_schema(&mut schema);
        assert_eq!(schema, original, "干净 schema 必须逐字节不变");
        assert!(changes.is_empty(), "{changes:?}");
    }
}

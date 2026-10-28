//! 最小 Patch：只触碰目标路径，其余内容原样保留（ADR-005）。

use crate::document::ConfigDocument;
use crate::error::ConfigError;

/// 单个 Patch 操作。`path` 为从根开始的键路径。
#[derive(Debug, Clone, PartialEq)]
pub enum PatchOp {
    /// 设置路径为给定值；中间对象/表不存在时自动创建。
    Set {
        path: Vec<String>,
        value: serde_json::Value,
    },
    /// 删除路径；路径不存在时为幂等 no-op。
    Remove { path: Vec<String> },
}

impl PatchOp {
    pub fn set(path: &[&str], value: serde_json::Value) -> Self {
        Self::Set {
            path: path.iter().map(|s| (*s).to_owned()).collect(),
            value,
        }
    }

    pub fn remove(path: &[&str]) -> Self {
        Self::Remove {
            path: path.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    pub fn path(&self) -> &[String] {
        match self {
            Self::Set { path, .. } | Self::Remove { path } => path,
        }
    }
}

/// 一次写入的完整 Patch 计划。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PatchPlan {
    pub ops: Vec<PatchOp>,
}

impl PatchPlan {
    pub fn new(ops: Vec<PatchOp>) -> Self {
        Self { ops }
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// 将计划应用到文档（就地修改）。
    pub fn apply(&self, doc: &mut ConfigDocument) -> Result<(), ConfigError> {
        for op in &self.ops {
            apply_op(doc, op)?;
        }
        Ok(())
    }
}

fn apply_op(doc: &mut ConfigDocument, op: &PatchOp) -> Result<(), ConfigError> {
    match doc {
        // YAML 内部就是 JSON 值模型，路径写入与 JSON 完全一致。
        ConfigDocument::Json(root) | ConfigDocument::Yaml(root) => apply_json(root, op),
        ConfigDocument::Json5(root) => {
            match op {
                PatchOp::Set { path, value } => crate::json5::set(root, path, value),
                PatchOp::Remove { path } => crate::json5::remove(root, path),
            }
            Ok(())
        }
        ConfigDocument::Toml(root) => apply_toml(root, op),
        ConfigDocument::Env(root) => apply_env(root, op),
    }
}

// ---------- JSON ----------

fn apply_json(root: &mut serde_json::Value, op: &PatchOp) -> Result<(), ConfigError> {
    let path = op.path();
    let Some((leaf, parents)) = path.split_last() else {
        return Err(ConfigError::InvalidPatchPath {
            path: String::new(),
            reason: "空路径".into(),
        });
    };

    match op {
        PatchOp::Set { value, .. } => {
            let mut cur = root;
            for seg in parents {
                if !cur.is_object() {
                    return Err(invalid_path(path, "中间节点不是对象"));
                }
                let map = cur
                    .as_object_mut()
                    .ok_or_else(|| invalid_path(path, "中间节点不是对象"))?;
                cur = map
                    .entry(seg.clone())
                    .or_insert_with(|| serde_json::Value::Object(Default::default()));
            }
            let map = cur
                .as_object_mut()
                .ok_or_else(|| invalid_path(path, "父节点不是对象"))?;
            map.insert(leaf.clone(), value.clone());
            Ok(())
        }
        PatchOp::Remove { .. } => {
            let mut cur = root;
            for seg in parents {
                match cur.get_mut(seg) {
                    Some(next) => cur = next,
                    None => return Ok(()), // 路径不存在：幂等
                }
            }
            if let Some(map) = cur.as_object_mut() {
                map.remove(leaf);
            }
            Ok(())
        }
    }
}

// ---------- TOML（语法树级最小修改） ----------

fn apply_toml(root: &mut toml_edit::DocumentMut, op: &PatchOp) -> Result<(), ConfigError> {
    let path = op.path();
    let Some((leaf, parents)) = path.split_last() else {
        return Err(ConfigError::InvalidPatchPath {
            path: String::new(),
            reason: "空路径".into(),
        });
    };

    match op {
        PatchOp::Set { value, .. } => {
            let mut cur = root.as_item_mut();
            for seg in parents {
                let table = cur
                    .as_table_like_mut()
                    .ok_or_else(|| invalid_path(path, "中间节点不是 Table"))?;
                if table.get(seg).is_none() {
                    let mut implicit = toml_edit::Table::new();
                    // 隐式中间表：`[a]` 不单独落盘，只体现为 `[a.b]`。
                    implicit.set_implicit(true);
                    table.insert(seg, toml_edit::Item::Table(implicit));
                }
                cur = cur
                    .get_mut(seg)
                    .ok_or_else(|| invalid_path(path, "无法进入中间节点"))?;
            }
            let table = cur
                .as_table_like_mut()
                .ok_or_else(|| invalid_path(path, "父节点不是 Table"))?;
            let mut new_item = json_to_toml_item(value);
            match table.get_mut(leaf) {
                Some(existing) => {
                    // 就地赋值：保留键上的装饰（前置注释、空行）。
                    // 标量→标量时进一步拷贝值装饰（行内注释）。
                    if let (Some(old_value), Some(new_value)) =
                        (existing.as_value(), new_item.as_value_mut())
                    {
                        *new_value.decor_mut() = old_value.decor().clone();
                    }
                    *existing = new_item;
                }
                None => {
                    table.insert(leaf, new_item);
                }
            }
            Ok(())
        }
        PatchOp::Remove { .. } => {
            let mut cur = root.as_item_mut();
            for seg in parents {
                match cur.get_mut(seg) {
                    Some(next) => cur = next,
                    None => return Ok(()),
                }
            }
            if let Some(table) = cur.as_table_like_mut() {
                table.remove(leaf);
            }
            Ok(())
        }
    }
}

/// JSON 值 → toml_edit Item。对象转为显式 Table（新增条目场景）。
fn json_to_toml_item(value: &serde_json::Value) -> toml_edit::Item {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            let mut table = toml_edit::Table::new();
            for (k, v) in map {
                table.insert(k, json_to_toml_item(v));
            }
            toml_edit::Item::Table(table)
        }
        other => toml_edit::Item::Value(json_to_toml_value(other)),
    }
}

fn json_to_toml_value(value: &serde_json::Value) -> toml_edit::Value {
    use serde_json::Value;
    match value {
        Value::Null => toml_edit::Value::from(""),
        Value::Bool(b) => toml_edit::Value::from(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml_edit::Value::from(i)
            } else {
                toml_edit::Value::from(n.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(s) => toml_edit::Value::from(s.as_str()),
        Value::Array(items) => {
            let mut arr = toml_edit::Array::new();
            for item in items {
                arr.push(json_to_toml_value(item));
            }
            toml_edit::Value::Array(arr)
        }
        Value::Object(map) => {
            let mut table = toml_edit::InlineTable::new();
            for (k, v) in map {
                table.insert(k, json_to_toml_value(v));
            }
            toml_edit::Value::InlineTable(table)
        }
    }
}

// ---------- Env（dotenv 行级最小修改） ----------

fn apply_env(root: &mut crate::document::EnvDocument, op: &PatchOp) -> Result<(), ConfigError> {
    use crate::document::{EnvLine, quote_env};

    let path = op.path();
    let [key] = path else {
        return Err(invalid_path(path, "env 格式只支持单段键路径"));
    };

    match op {
        PatchOp::Set { value, .. } => {
            // env 值恒为字符串；数字/布尔按字符串写入，结构类型拒绝。
            let text = match value {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                _ => return Err(invalid_path(path, "env 值必须是标量")),
            };
            // 装不进任何一种 dotenv 引号形态（含真换行，或同时含 `'` 与 `"`/`\`）：
            // 报错而不是写出一份会被消费方读成别的值的配置。
            let Some(quoted) = quote_env(&text) else {
                return Err(invalid_path(
                    path,
                    "值无法在 dotenv 中无损表达（含换行，或同时含单引号与双引号/反斜杠）",
                ));
            };
            let raw = format!("{key}={quoted}");
            // dotenv 先到先得：就地更新第一个匹配（保行位），其余同名行删除防歧义。
            let mut updated = false;
            root.lines.retain_mut(|line| {
                if let EnvLine::Pair {
                    key: k,
                    value: v,
                    raw: r,
                } = line
                    && k == key
                {
                    if updated {
                        return false;
                    }
                    *v = text.clone();
                    *r = raw.clone();
                    updated = true;
                }
                true
            });
            if !updated {
                root.lines.push(EnvLine::Pair {
                    key: key.clone(),
                    value: text,
                    raw,
                });
            }
            Ok(())
        }
        PatchOp::Remove { .. } => {
            root.lines
                .retain(|line| !matches!(line, EnvLine::Pair { key: k, .. } if k == key));
            Ok(())
        }
    }
}

fn invalid_path(path: &[String], reason: &str) -> ConfigError {
    ConfigError::InvalidPatchPath {
        path: path.join("."),
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    mod env {
        use crate::document::{ConfigDocument, ConfigFormat};
        use crate::patch::{PatchOp, PatchPlan};

        fn apply(raw: &str, ops: Vec<PatchOp>) -> String {
            let mut doc = ConfigDocument::parse(raw, ConfigFormat::Env).unwrap();
            PatchPlan::new(ops).apply(&mut doc).unwrap();
            doc.serialize()
        }

        #[test]
        fn set_updates_in_place_and_appends_new_keys() {
            let out = apply(
                "# 注释保留\nGEMINI_MODEL=old\nOTHER=keep\n",
                vec![
                    PatchOp::set(&["GEMINI_MODEL"], serde_json::json!("gemini-3-pro")),
                    PatchOp::set(&["GEMINI_API_KEY"], serde_json::json!("sk-new")),
                ],
            );
            assert_eq!(
                out,
                "# 注释保留\nGEMINI_MODEL=gemini-3-pro\nOTHER=keep\nGEMINI_API_KEY=sk-new\n"
            );
        }

        #[test]
        fn set_dedupes_duplicate_keys_updating_first() {
            let out = apply(
                "K=a\nMID=x\nK=b\n",
                vec![PatchOp::set(&["K"], serde_json::json!("new"))],
            );
            assert_eq!(out, "K=new\nMID=x\n");
        }

        #[test]
        fn remove_is_idempotent_and_keeps_others() {
            let out = apply(
                "# 头注释\nGEMINI_API_KEY=sk\nUSER_VAR=1\n",
                vec![
                    PatchOp::remove(&["GEMINI_API_KEY"]),
                    PatchOp::remove(&["MISSING"]),
                ],
            );
            assert_eq!(out, "# 头注释\nUSER_VAR=1\n");
        }

        /// 需要引号时一律走**单引号**：dotenv 对单引号内容逐字取用，而双引号
        /// 形态会展开 `\n`/`\r` 又不认 `\"` 转义（node 内置 `--env-file` 与
        /// dotenv npm 一致，2026-08-06 实测）。曾无条件用双引号 + `\"` 转义，
        /// 结果是含双引号的 Key 被消费方截断、含 `\n` 字面量的值被拆成两行。
        #[test]
        fn values_needing_quotes_use_single_quotes() {
            let out = apply(
                "",
                vec![PatchOp::set(&["K"], serde_json::json!("has space #hash"))],
            );
            assert_eq!(out, "K='has space #hash'\n");
        }

        /// 含双引号的值：单引号形态原样装下，dotenv 读回一字不差。
        /// 旧实现写成 `K="sk-a\"b"`，被 dotenv 读成 `sk-a\`（值截断）。
        #[test]
        fn value_with_double_quote_survives_round_trip() {
            let secret = r#"sk-a"b"#;
            let out = apply("", vec![PatchOp::set(&["K"], serde_json::json!(secret))]);
            assert_eq!(out, "K='sk-a\"b'\n");
            let doc = ConfigDocument::parse(&out, ConfigFormat::Env).unwrap();
            assert_eq!(doc.get(&["K"]).unwrap(), serde_json::json!(secret));
        }

        /// 反斜杠字面量（Windows 路径、`\n` 两字符序列）同样只有单引号扛得住：
        /// 双引号里 dotenv 会把 `\n` 展开成真换行。
        #[test]
        fn backslash_literals_are_not_expanded() {
            let value = r"C:\new\table";
            let out = apply("", vec![PatchOp::set(&["P"], serde_json::json!(value))]);
            let doc = ConfigDocument::parse(&out, ConfigFormat::Env).unwrap();
            assert_eq!(doc.get(&["P"]).unwrap(), serde_json::json!(value));
        }

        /// 装不进任何 dotenv 引号形态的值必须报错，不能静默写出会被消费方
        /// 读成别的东西的内容。
        #[test]
        fn values_unrepresentable_in_dotenv_are_rejected() {
            let mut doc = ConfigDocument::parse("", ConfigFormat::Env).unwrap();
            // 真换行：行模型一行一条，任何引号都表达不了。
            assert!(
                PatchPlan::new(vec![PatchOp::set(&["K"], serde_json::json!("a\nb"))])
                    .apply(&mut doc)
                    .is_err()
            );
            // 同时含单引号与双引号：两种引号形态都无损不了。
            assert!(
                PatchPlan::new(vec![PatchOp::set(&["K"], serde_json::json!("a'b\"c"))])
                    .apply(&mut doc)
                    .is_err()
            );
            assert_eq!(doc.serialize(), "", "被拒的写入不得留下半截内容");
        }

        /// 读取侧与真实消费方对齐：用户手写的双引号值里 `\n` 会被 dotenv 展开成
        /// 真换行，我们必须读到同一个值——否则回填会把一个 CLI 从没见过的值写回去。
        #[test]
        fn double_quoted_escapes_are_read_like_dotenv_does() {
            let doc = ConfigDocument::parse("A=\"x\\ny\"\nB='x\\ny'\nC=x\\ny\n", ConfigFormat::Env)
                .unwrap();
            assert_eq!(doc.get(&["A"]).unwrap(), serde_json::json!("x\ny"));
            assert_eq!(doc.get(&["B"]).unwrap(), serde_json::json!(r"x\ny"));
            assert_eq!(doc.get(&["C"]).unwrap(), serde_json::json!(r"x\ny"));
        }

        #[test]
        fn nested_paths_and_structured_values_are_rejected() {
            let mut doc = ConfigDocument::parse("", ConfigFormat::Env).unwrap();
            assert!(
                PatchPlan::new(vec![PatchOp::set(&["a", "b"], serde_json::json!("v"))])
                    .apply(&mut doc)
                    .is_err()
            );
            assert!(
                PatchPlan::new(vec![PatchOp::set(&["a"], serde_json::json!({"o": 1}))])
                    .apply(&mut doc)
                    .is_err()
            );
        }
    }

    use super::*;
    use crate::document::ConfigFormat;

    #[test]
    fn json_set_creates_intermediate_objects_and_preserves_rest() {
        let raw = r#"{"keep": {"x": 1}, "mcpServers": {"other": {"type": "stdio"}}}"#;
        let mut doc = ConfigDocument::parse(raw, ConfigFormat::Json).unwrap();
        PatchPlan::new(vec![PatchOp::set(
            &["mcpServers", "mcpane"],
            serde_json::json!({ "type": "http", "url": "http://127.0.0.1:24860/mcp" }),
        )])
        .apply(&mut doc)
        .unwrap();

        assert_eq!(
            doc.get(&["mcpServers", "mcpane", "url"]).unwrap(),
            serde_json::json!("http://127.0.0.1:24860/mcp")
        );
        assert_eq!(
            doc.get(&["mcpServers", "other", "type"]).unwrap(),
            serde_json::json!("stdio"),
            "既有兄弟条目必须保留"
        );
        assert_eq!(doc.get(&["keep", "x"]).unwrap(), serde_json::json!(1));
    }

    #[test]
    fn json_remove_is_idempotent_and_scoped() {
        let raw = r#"{"mcpServers": {"mcpane": {"a":1}, "other": {"b":2}}}"#;
        let mut doc = ConfigDocument::parse(raw, ConfigFormat::Json).unwrap();
        let plan = PatchPlan::new(vec![PatchOp::remove(&["mcpServers", "mcpane"])]);
        plan.apply(&mut doc).unwrap();
        plan.apply(&mut doc).unwrap(); // 第二次为 no-op
        assert!(doc.get(&["mcpServers", "mcpane"]).is_none());
        assert_eq!(doc.get(&["mcpServers", "other", "b"]).unwrap(), 2);
    }

    #[test]
    fn toml_set_preserves_comments_and_unknown_tables() {
        let raw =
            "# 用户注释\nmodel = \"o3\" # 保留我\n\n[mcp_servers.existing]\ncommand = \"npx\"\n";
        let mut doc = ConfigDocument::parse(raw, ConfigFormat::Toml).unwrap();
        PatchPlan::new(vec![
            PatchOp::set(
                &["mcp_servers", "mcpane", "url"],
                serde_json::json!("http://127.0.0.1:24860/mcp"),
            ),
            PatchOp::set(
                &["mcp_servers", "mcpane", "bearer_token_env_var"],
                serde_json::json!("MCPANE_CODEX_TOKEN"),
            ),
        ])
        .apply(&mut doc)
        .unwrap();

        let out = doc.serialize();
        assert!(out.contains("# 用户注释"), "顶部注释必须保留: {out}");
        assert!(out.contains("# 保留我"), "行内注释必须保留: {out}");
        assert!(
            out.contains("[mcp_servers.existing]"),
            "未知表必须保留: {out}"
        );
        assert!(
            out.contains("[mcp_servers.mcpane]"),
            "新表以标准形式写入: {out}"
        );
        assert_eq!(
            doc.get(&["mcp_servers", "mcpane", "bearer_token_env_var"])
                .unwrap(),
            serde_json::json!("MCPANE_CODEX_TOKEN")
        );
    }

    #[test]
    fn toml_remove_only_touches_target_table() {
        let raw =
            "[mcp_servers.mcpane]\nurl = \"x\"\n\n[mcp_servers.other] # 注释\ncommand = \"npx\"\n";
        let mut doc = ConfigDocument::parse(raw, ConfigFormat::Toml).unwrap();
        PatchPlan::new(vec![PatchOp::remove(&["mcp_servers", "mcpane"])])
            .apply(&mut doc)
            .unwrap();
        let out = doc.serialize();
        assert!(!out.contains("mcpane"), "目标表必须删除: {out}");
        assert!(
            out.contains("[mcp_servers.other] # 注释"),
            "其他表连注释保留: {out}"
        );
    }
}

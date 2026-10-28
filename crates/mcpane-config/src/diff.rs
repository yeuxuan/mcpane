//! 写入前 Diff：结构化条目 + 统一文本 Diff（供 UI 展示与操作历史）。

use similar::TextDiff;

use crate::document::ConfigDocument;
use crate::patch::{PatchOp, PatchPlan};

/// 单条结构化变更。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffEntry {
    pub path: String,
    pub before: Option<serde_json::Value>,
    pub after: Option<serde_json::Value>,
}

/// 一次写入的完整 Diff。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDiff {
    pub entries: Vec<DiffEntry>,
    /// unified diff 文本（配置整体视角）。
    pub unified: String,
}

/// 统一 Diff 文本。传磁盘原文与最终落盘文本即可如实反映本次写入。
pub(crate) fn unified_diff(before: &str, after: &str) -> String {
    TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(2)
        .header("before", "after")
        .to_string()
}

impl ConfigDiff {
    /// 基于"应用前文档 + 计划"计算 Diff（不落盘）。
    pub fn compute(
        before: &ConfigDocument,
        plan: &PatchPlan,
    ) -> Result<Self, crate::error::ConfigError> {
        Ok(Self::compute_with_after(before, plan)?.0)
    }

    /// 计算 Diff 并一并返回应用后的序列化文本——engine 写入流水线复用，
    /// 避免同一计划再做一次 clone + apply + serialize（目标可能是数 MB 的 ~/.claude.json）。
    pub(crate) fn compute_with_after(
        before: &ConfigDocument,
        plan: &PatchPlan,
    ) -> Result<(Self, String), crate::error::ConfigError> {
        // try_clone 而非 clone：JSON5 这一路的克隆要绕「序列化 → 重解析」，
        // 而它真的会失败。失败必须在这里就中止整条写入流水线（见
        // `ConfigDocument::try_clone` 的文档：静默兜底曾导致整份配置被写成 null）。
        let mut after = before.try_clone()?;
        plan.apply(&mut after)?;

        let entries = plan
            .ops
            .iter()
            .map(|op| {
                let segs: Vec<&str> = op.path().iter().map(String::as_str).collect();
                DiffEntry {
                    path: op.path().join("."),
                    before: before.get(&segs),
                    after: match op {
                        PatchOp::Set { .. } => after.get(&segs),
                        PatchOp::Remove { .. } => None,
                    },
                }
            })
            .collect();

        let before_text = before.serialize();
        let after_text = after.serialize();
        // 两侧都是规范化文本，只反映结构变更；调用方应拿磁盘原文与落盘文本
        // 用 `unified_diff` 覆写它。
        let unified = unified_diff(&before_text, &after_text);

        Ok((Self { entries, unified }, after_text))
    }

    pub fn is_noop(&self) -> bool {
        self.entries.iter().all(|e| e.before == e.after)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::document::ConfigFormat;

    #[test]
    fn diff_reports_before_after_and_unified_text() {
        let doc = ConfigDocument::parse(r#"{"a": 1}"#, ConfigFormat::Json).unwrap();
        let plan = PatchPlan::new(vec![
            PatchOp::set(&["a"], serde_json::json!(2)),
            PatchOp::set(&["b"], serde_json::json!("new")),
            PatchOp::remove(&["missing"]),
        ]);
        let diff = ConfigDiff::compute(&doc, &plan).unwrap();

        assert_eq!(diff.entries.len(), 3);
        assert_eq!(diff.entries[0].before, Some(serde_json::json!(1)));
        assert_eq!(diff.entries[0].after, Some(serde_json::json!(2)));
        assert_eq!(diff.entries[1].before, None);
        assert!(diff.unified.contains("-  \"a\": 1"));
        assert!(diff.unified.contains("+  \"a\": 2"));
        assert!(!diff.is_noop());
    }

    #[test]
    fn noop_plan_detected() {
        let doc = ConfigDocument::parse(r#"{"a": 1}"#, ConfigFormat::Json).unwrap();
        let plan = PatchPlan::new(vec![PatchOp::set(&["a"], serde_json::json!(1))]);
        let diff = ConfigDiff::compute(&doc, &plan).unwrap();
        assert!(diff.is_noop());
    }
}

//! 配置引擎错误类型：每个失败路径都有明确分类（DoD 要求）。

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// 写入前重读发现文件与预期哈希不一致（外部修改），拒绝写入。
    #[error("配置冲突：{path} 自读取后已被外部修改（预期 {expected_hash}，实际 {actual_hash}）")]
    Conflict {
        path: PathBuf,
        expected_hash: String,
        actual_hash: String,
    },

    #[error("解析失败（{format}）：{message}")]
    Parse {
        format: &'static str,
        message: String,
    },

    #[error("Patch 路径不可用：{path}（{reason}）")]
    InvalidPatchPath { path: String, reason: String },

    /// 文档能读、但写不回去：序列化产物重解析失败，说明这一路的格式往返有损。
    /// 报出来而不是硬写——写下去就是一份用户客户端读不了的配置。
    #[error("序列化产物无法重解析（{format}）：{message}")]
    LossySerialize {
        format: &'static str,
        message: String,
    },

    #[error("业务验证失败：{0}")]
    ValidationFailed(String),

    /// 写入后重读验证不通过（磁盘内容与期望不符）。
    #[error("写后验证失败：{path}")]
    PostWriteVerification { path: PathBuf },

    /// 写入失败但已成功恢复原文件。
    #[error("写入失败已恢复原文件：{source_message}")]
    RecoveredFromFailure { source_message: String },

    /// 写入失败且恢复也失败——最严重路径，快照仍可手动恢复。
    #[error("写入失败且自动恢复失败：{write_error}；恢复错误：{recover_error}；快照：{snapshot}")]
    RecoveryFailed {
        write_error: String,
        recover_error: String,
        snapshot: String,
    },

    #[error("快照错误：{0}")]
    Snapshot(String),

    #[error("IO 错误（{context}）：{source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

impl ConfigError {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

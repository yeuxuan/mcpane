//! 客户端凭据投递方式（ADR-015）。
//!
//! Hub Bearer Token 写进客户端配置有两种形态：
//! - **EnvVar（默认）**：只写环境变量引用，Token 由用户自行注入运行环境，
//!   不落任何配置文件；`required=false` 保证变量未设置时客户端仍可正常启动。
//! - **Inline（可选）**：明文直写 `Authorization: Bearer <token>`，接入零手动步骤；
//!   威胁模型见 ADR-015——Token 防的是浏览器侧 DNS rebinding/CSRF，
//!   对本地同权限进程明文落盘与环境变量暴露面等同。

/// Patch 生成期的凭据来源：inline 携带明文 Token，env 只写变量引用。
#[derive(Debug, Clone, Copy)]
pub enum CredentialDelivery<'a> {
    /// 明文 Bearer Token 直写配置。
    Inline(&'a str),
    /// 环境变量引用（变量名由各 adapter 自己的约定决定）。
    EnvVar,
}

impl CredentialDelivery<'_> {
    /// 对应的校验模式（校验不携带 Token 值）。
    pub fn mode(&self) -> CredentialMode {
        match self {
            Self::Inline(_) => CredentialMode::Inline,
            Self::EnvVar => CredentialMode::EnvVar,
        }
    }
}

/// 校验期望的凭据形态。Inline 只要求 `Bearer ` 前缀 + 非空非占位，
/// 不比对具体 Token 值（容忍用户手改后轮换自愈）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialMode {
    Inline,
    EnvVar,
}

impl CredentialMode {
    /// 从 Tauri 命令的字符串参数解析；`None` 缺省 EnvVar（默认不落盘）。
    pub fn parse(mode: Option<&str>) -> Result<Self, String> {
        match mode {
            None | Some("env") => Ok(Self::EnvVar),
            Some("inline") => Ok(Self::Inline),
            Some(other) => Err(format!("未知凭据模式：{other}（支持 inline / env）")),
        }
    }
}

/// Inline 形态下 Authorization 值是否可用：`Bearer ` 前缀 + 非空 + 非占位。
pub fn is_usable_inline_bearer(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("Bearer ") else {
        return false;
    };
    !rest.trim().is_empty() && !rest.contains('<') && !rest.contains("${")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parse_mode_defaults_to_env() {
        assert_eq!(CredentialMode::parse(None).unwrap(), CredentialMode::EnvVar);
        assert_eq!(
            CredentialMode::parse(Some("inline")).unwrap(),
            CredentialMode::Inline
        );
        assert_eq!(
            CredentialMode::parse(Some("env")).unwrap(),
            CredentialMode::EnvVar
        );
        assert!(CredentialMode::parse(Some("plaintext")).is_err());
    }

    #[test]
    fn usable_inline_bearer_rejects_placeholder_and_env_ref() {
        assert!(is_usable_inline_bearer("Bearer abc123"));
        assert!(!is_usable_inline_bearer("Bearer "));
        assert!(!is_usable_inline_bearer("Bearer <接入时自动发放>"));
        assert!(!is_usable_inline_bearer("Bearer ${MCPANE_CLAUDE_TOKEN}"));
        assert!(!is_usable_inline_bearer("token abc"));
    }
}

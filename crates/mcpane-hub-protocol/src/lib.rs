//! Hub 控制面协议共享层：数据目录、socket 路径、端口常量与命令 wire 类型。
//!
//! Hub（服务端）与桌面 App（客户端）此前各自实现 `mcpane_home` 与 socket
//! 路径拼接，hub 侧的 UDS `SUN_LEN` 超限回退桌面侧没有——`MCPANE_HOME`
//! 路径过长时桌面会连错 socket、把活着的 hub 误判为未运行。双端唯一事实
//! 源收敛到本 crate，杜绝再次漂移。

use std::path::PathBuf;

pub mod cli_path;
pub mod panic_hook;

/// 默认数据面监听地址（冻结标识，见 docs/branding/NAMING.md）。
pub const DEFAULT_BIND_ADDR: &str = "127.0.0.1:24860";

/// 默认数据面 MCP 入口 URL。
pub const HUB_MCP_URL: &str = "http://127.0.0.1:24860/mcp";

/// `mcpane-hub bridge`（stdio ↔ Streamable HTTP 桥）的环境变量名。
/// 双端共享：桥进程读取；桌面端把它们写进 stdio 客户端配置的 `env` 块。
pub mod bridge_env {
    /// Bearer Token（必填）。
    pub const TOKEN: &str = "MCPANE_TOKEN";
    /// Hub 数据面端点（缺省 [`crate::HUB_MCP_URL`]）。
    pub const URL: &str = "MCPANE_HUB_URL";
}

/// 逻辑数据目录：`MCPANE_HOME` 覆盖，否则用平台默认（NAMING.md）。
pub fn mcpane_home() -> PathBuf {
    if let Ok(home) = std::env::var("MCPANE_HOME") {
        return PathBuf::from(home);
    }
    #[cfg(target_os = "macos")]
    {
        let user_home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
        PathBuf::from(user_home).join("Library/Application Support/MCPane")
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_owned());
        PathBuf::from(appdata).join("MCPane")
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // 绝对路径：下游要把该路径写进 shell rc（envsync），相对路径会产出坏行。
        let user_home = std::env::var("HOME").unwrap_or_else(|_| ".".to_owned());
        PathBuf::from(user_home).join(".mcpane")
    }
}

/// 控制面 socket 路径：`$MCPANE_HOME/hub-control.sock`。
///
/// Unix Domain Socket 路径受 `SUN_LEN` 限制（macOS 约 104 字节）；
/// 超限时回退到 `/tmp/mcpane-{uid}/hub-control.sock`（目录 0700）。
#[cfg(unix)]
pub fn control_socket_path() -> PathBuf {
    control_socket_path_in(mcpane_home())
}

/// `control_socket_path` 的可测试形态：home 由调用方给定。
#[cfg(unix)]
pub fn control_socket_path_in(home: PathBuf) -> PathBuf {
    const SUN_PATH_MAX: usize = 100;
    let preferred = home.join("hub-control.sock");
    if preferred.as_os_str().len() <= SUN_PATH_MAX {
        return preferred;
    }
    // 以当前用户 HOME 的属主 uid 隔离回退目录（无需新增 libc 依赖）。
    use std::os::unix::fs::MetadataExt;
    let uid = std::env::var("HOME")
        .ok()
        .and_then(|home| std::fs::metadata(home).ok())
        .map(|meta| meta.uid())
        .unwrap_or(0);
    let fallback_dir = PathBuf::from(format!("/tmp/mcpane-{uid}"));
    tracing::warn!(
        preferred = %preferred.display(),
        fallback = %fallback_dir.display(),
        "MCPANE_HOME 路径超过 UDS SUN_LEN 限制，控制面 socket 回退到短路径"
    );
    fallback_dir.join("hub-control.sock")
}

/// 控制面 Named Pipe 名（Windows）：`\\.\pipe\mcpane-hub-control-{user}`。
///
/// 按用户名隔离，避免同一机器多用户/多会话撞名（UDS 用路径隔离，管道用名字）。
/// 访问控制不靠命名本身：默认安全描述符会给 Everyone 与匿名账户读权限，
/// 收紧到「创建者 + SYSTEM」的 DACL 由 hub 建管道时显式设置（见 control.rs）。
#[cfg(windows)]
pub fn control_pipe_name() -> String {
    let user = std::env::var("USERNAME").unwrap_or_else(|_| "default".to_owned());
    // 只留字母数字：用户名里的空格/反斜杠等会破坏管道名。
    let sanitized: String = user.chars().filter(|c| c.is_alphanumeric()).collect();
    let sanitized = if sanitized.is_empty() {
        "default".to_owned()
    } else {
        sanitized
    };
    format!(r"\\.\pipe\mcpane-hub-control-{sanitized}")
}

/// 控制面命令（P0 最小集；完整协议见 TASK-400）。
/// 一行 JSON 命令 → 一行 JSON 响应；空输入（EOF）→ 返回状态。
///
/// 双端共用：hub 反序列化执行，桌面序列化发送——命令格式不可能再漂移。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum ControlCommand {
    Status,
    /// 优雅退出：收尾数据面、下游与 SQLite 后进程退出。
    /// Windows 没有 SIGTERM，Desktop 靠这条对齐 Unix 关停。
    Shutdown,
    RevokeToken {
        client_connection_id: String,
    },
    EnableToken {
        client_connection_id: String,
    },
    /// 下游/Catalog 诊断（TASK-400）。
    Diagnostics,
    /// 创建客户端连接（完整 Token 只在响应中出现一次）。
    CreateConnection {
        name: String,
        client_type: String,
    },
    /// 轮换 Token（旧 Token 立即失效；新 Token 只在响应中出现一次）。
    RotateToken {
        client_connection_id: String,
    },
    /// 永久删除连接（Token 立即失效且不可恢复；区别于 RevokeToken 的可逆撤销）。
    DeleteConnection {
        client_connection_id: String,
    },
    /// 重新从 SQLite 装载下游 Server 并重建 Catalog（UI 增删改后调用）。
    ReloadServers,
    /// 列出某 Workspace 当前 Catalog 的全部工具路由（缺省 ws_default）。
    ListCatalog {
        workspace_id: Option<String>,
    },
    /// 设置连接的默认 Workspace（写库 + 重建映射）。
    SetConnectionWorkspace {
        client_connection_id: String,
        workspace_id: Option<String>,
    },
    /// 访问控制：按连接开关某 Server 的访问权（写库 + 更新内存映射，
    /// 不重连下游；对活跃 Session 立即生效）。
    SetConnectionServerAccess {
        client_connection_id: String,
        server_id: String,
        allowed: bool,
    },
    /// 设置连接的工具暴露模式：flat（平铺）/ gateway（网关）/ auto（按客户端自适应）。
    /// 写库 + 更新内存映射，对活跃连接的下一次 tools/list 生效。
    SetConnectionToolExposure {
        client_connection_id: String,
        mode: String,
    },
    /// 远程 MCP 的 OAuth 登录第一段（ADR-024）：Hub 做授权服务器发现与动态注册，
    /// 返回要在浏览器里打开的授权链接。`redirect_uri` 是桌面端先绑好的本机回环地址。
    #[serde(rename = "oauth-begin")]
    OAuthBegin {
        server_id: String,
        redirect_uri: String,
    },
    /// 第二段：凭浏览器回调带回的 `code` 与 `state` 换令牌；成功后 Hub 自行重连该服务器。
    #[serde(rename = "oauth-complete")]
    OAuthComplete {
        server_id: String,
        code: String,
        state: String,
    },
    /// 放弃一次进行中的登录（用户关了浏览器 / 点了取消）。
    #[serde(rename = "oauth-cancel")]
    OAuthCancel {
        server_id: String,
    },
    /// 退出登录：删令牌并让该服务器回到「需要登录」。
    #[serde(rename = "oauth-logout")]
    OAuthLogout {
        server_id: String,
    },
    /// 测试一份 Server 配置能否连通（不落库；connect + tools/list 后即关闭）。
    /// 经 Hub 执行保证 bearer_env/进程环境解析与真实运行一致。
    TestServer {
        transport: String,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        bearer: Option<String>,
        #[serde(default)]
        bearer_env: Option<String>,
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
    },
}

/// 用户为某个下游工具**自己声明**的 `ToolAnnotations`（ADR-002 Gate 的例外口）。
///
/// 存在的唯一理由：下游 Server 不申报 annotations 时，Codex `exec`（非交互）
/// 会把该工具误路由进 app-tool 审批路径，stdin EOF 被当成"用户拒绝"，调用
/// 一律报 `user cancelled MCP tool call`（openai/codex#16685，修复 PR #16632
/// 截至 0.133.0 未合入）。Hub 不能替下游编注解，但可以让用户为自己装的工具
/// 具名声明一份——署名在用户，不在下游。
///
/// 四个 hint 一次全给，不做部分声明：触发上游缺陷的是"整个 annotations 缺席"，
/// 半份声明既修不干净也让界面多出一堆三态。字段语义与 MCP 规范同名字段一致。
///
/// 键是 `(server_id, tool)`、不含 workspace：注解陈述的是工具行为，跨工作区
/// 恒成立（迁移 0025 有完整理由）。
///
/// 下沉在 protocol：storage 存它、catalog 读它、桌面端填它，三方共用一份定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclaredAnnotations {
    /// 只读：不修改环境。
    #[serde(default = "spec_default_read_only")]
    pub read_only_hint: bool,
    /// 破坏性：可能做删除/覆盖类更新（仅在 `read_only_hint == false` 时有意义）。
    #[serde(default = "spec_default_destructive")]
    pub destructive_hint: bool,
    /// 幂等：同参重复调用无额外影响（仅在 `read_only_hint == false` 时有意义）。
    #[serde(default = "spec_default_idempotent")]
    pub idempotent_hint: bool,
    /// 开放世界：会与外部实体交互（联网、调用第三方服务）。
    #[serde(default = "spec_default_open_world")]
    pub open_world_hint: bool,
}

// 缺字段时的回退取 MCP 规范给各 hint 写明的默认值，而不是一律 false：
// 这些函数只在读到残缺 JSON（手改过库、跨版本回滚）时才生效，此时按规范
// 默认最保守——尤其 destructiveHint 规范默认为 true，写成 false 等于替
// 用户把一个未知工具申报成安全的。
fn spec_default_read_only() -> bool {
    false
}
fn spec_default_destructive() -> bool {
    true
}
fn spec_default_idempotent() -> bool {
    false
}
fn spec_default_open_world() -> bool {
    true
}

impl Default for DeclaredAnnotations {
    fn default() -> Self {
        Self {
            read_only_hint: spec_default_read_only(),
            destructive_hint: spec_default_destructive(),
            idempotent_hint: spec_default_idempotent(),
            open_world_hint: spec_default_open_world(),
        }
    }
}

/// Hub 内建工具的保留命名空间前缀（`mcpane__echo` 等，ADR-006/ADR-011 冻结）。
/// 下沉在 protocol：桌面端与 hub/catalog 共用同一份规则，杜绝双端漂移。
pub const TOOL_PREFIX: &str = "mcpane__";

/// Server slug 规范化：小写、非法字符转 `-`、折叠连续分隔符。
///
/// 结果恰好等于保留命名空间字（`mcpane`，见 [`TOOL_PREFIX`]）时追加后缀——
/// 否则该 Server 不用任何 alias、走纯默认命名就会产出 `mcpane__xxx`，
/// 直接撞上 Hub 内建工具（空结果回退 `"server"` 是同一个道理的既有先例）。
///
/// 这是 slug 规则的唯一实现：桌面端建 Server、导入、hub 侧建 Catalog 都走
/// 这里。此前桌面端有一份"最小复刻"，恰恰漏了保留字转义分支。
pub fn normalize_slug(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for c in input.chars() {
        let mapped = if c.is_ascii_alphanumeric() {
            last_dash = false;
            c.to_ascii_lowercase()
        } else if last_dash {
            continue;
        } else {
            last_dash = true;
            '-'
        };
        slug.push(mapped);
    }
    let slug = slug.trim_matches('-').to_owned();
    let slug = if slug.is_empty() {
        "server".to_owned()
    } else {
        slug
    };
    if slug == reserved_namespace() {
        format!("{slug}-server")
    } else {
        slug
    }
}

/// [`TOOL_PREFIX`] 去掉尾部分隔符的裸命名空间字（`"mcpane"`）。
///
/// 用 `trim_end_matches` 而不是 `strip_suffix(..).expect(..)`：后者要在
/// 每次调用时为一个编译期就已确定的常量担一次 panic 风险。两者对
/// `"mcpane__"` 结果相同，一致性由 `reserved_namespace_strips_prefix_separator`
/// 守住。
fn reserved_namespace() -> &'static str {
    TOOL_PREFIX.trim_end_matches('_')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// OAuth 命令的线上形态是桌面与 Hub 之间的契约：字面量写死，改名会让两端同时失联。
    #[test]
    fn oauth_commands_have_stable_wire_names() {
        let begin = serde_json::to_value(ControlCommand::OAuthBegin {
            server_id: "s1".into(),
            redirect_uri: "http://127.0.0.1:5555/callback".into(),
        })
        .unwrap();
        assert_eq!(begin["cmd"], "oauth-begin");
        assert_eq!(begin["server_id"], "s1");
        assert_eq!(begin["redirect_uri"], "http://127.0.0.1:5555/callback");
        for (cmd, json) in [
            (
                "oauth-complete",
                serde_json::json!({"cmd":"oauth-complete","server_id":"s1","code":"c","state":"x"}),
            ),
            (
                "oauth-cancel",
                serde_json::json!({"cmd":"oauth-cancel","server_id":"s1"}),
            ),
            (
                "oauth-logout",
                serde_json::json!({"cmd":"oauth-logout","server_id":"s1"}),
            ),
        ] {
            let parsed: ControlCommand = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), json, "{cmd}");
        }
    }

    #[test]
    fn reserved_namespace_strips_prefix_separator() {
        assert_eq!(reserved_namespace(), "mcpane");
        assert_eq!(format!("{}__", reserved_namespace()), TOOL_PREFIX);
    }

    #[test]
    fn normalize_slug_basic_mapping() {
        assert_eq!(normalize_slug("Context7 MCP"), "context7-mcp");
        assert_eq!(normalize_slug("  My__Server!! "), "my-server");
        // 非 ASCII 字符按分隔符处理，只保留 ASCII 字母数字。
        assert_eq!(normalize_slug("文A文B"), "a-b");
        assert_eq!(normalize_slug(""), "server");
        assert_eq!(normalize_slug("!!!"), "server");
        assert_eq!(normalize_slug("中文名"), "server");
    }

    /// 保留字转义：裸 `mcpane` 会撞上内建工具前缀，必须追加后缀。
    #[test]
    fn normalize_slug_escapes_reserved_word() {
        assert_eq!(normalize_slug("mcpane"), "mcpane-server");
        assert_eq!(normalize_slug("MCPane!"), "mcpane-server");
    }

    #[cfg(unix)]
    #[test]
    fn socket_path_prefers_home_when_short() {
        let home = PathBuf::from("/tmp/mcpane-test");
        assert_eq!(
            control_socket_path_in(home),
            PathBuf::from("/tmp/mcpane-test/hub-control.sock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn socket_path_falls_back_when_exceeding_sun_len() {
        let home = PathBuf::from(format!("/tmp/{}", "x".repeat(120)));
        let path = control_socket_path_in(home);
        assert!(path.starts_with("/tmp/"));
        assert!(
            path.to_string_lossy().starts_with("/tmp/mcpane-"),
            "超长 home 必须回退到 /tmp/mcpane-<uid>/：{}",
            path.display()
        );
        assert!(path.as_os_str().len() <= 100);
    }

    #[test]
    fn command_wire_format_is_kebab_tagged() {
        let json = serde_json::to_string(&ControlCommand::Status).unwrap();
        assert_eq!(json, r#"{"cmd":"status"}"#);
        let json = serde_json::to_string(&ControlCommand::Shutdown).unwrap();
        assert_eq!(json, r#"{"cmd":"shutdown"}"#);
        let json = serde_json::to_string(&ControlCommand::SetConnectionWorkspace {
            client_connection_id: "c1".into(),
            workspace_id: None,
        })
        .unwrap();
        let back: ControlCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ControlCommand::SetConnectionWorkspace {
                workspace_id: None,
                ..
            }
        ));
    }

    #[test]
    fn test_server_optional_fields_default() {
        // 前端只传相关字段：缺省字段必须能反序列化。
        let back: ControlCommand = serde_json::from_str(
            r#"{"cmd":"test-server","transport":"http","url":"https://example.com/mcp","bearer":"tok"}"#,
        )
        .unwrap();
        match back {
            ControlCommand::TestServer {
                transport,
                url,
                bearer,
                command,
                args,
                env,
                headers,
                ..
            } => {
                assert_eq!(transport, "http");
                assert_eq!(url.as_deref(), Some("https://example.com/mcp"));
                assert_eq!(bearer.as_deref(), Some("tok"));
                assert!(command.is_none());
                assert!(args.is_empty());
                assert!(env.is_empty());
                assert!(headers.is_empty());
            }
            other => panic!("解析成了 {other:?}"),
        }
    }

    #[test]
    fn set_connection_server_access_wire_roundtrip() {
        let json = serde_json::to_string(&ControlCommand::SetConnectionServerAccess {
            client_connection_id: "c1".into(),
            server_id: "srv_1".into(),
            allowed: false,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"set-connection-server-access","client_connection_id":"c1","server_id":"srv_1","allowed":false}"#
        );
        let back: ControlCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ControlCommand::SetConnectionServerAccess { allowed: false, .. }
        ));
    }

    #[test]
    fn set_connection_tool_exposure_wire_roundtrip() {
        let json = serde_json::to_string(&ControlCommand::SetConnectionToolExposure {
            client_connection_id: "c1".into(),
            mode: "flat".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"set-connection-tool-exposure","client_connection_id":"c1","mode":"flat"}"#
        );
        let back: ControlCommand = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ControlCommand::SetConnectionToolExposure { mode, .. } if mode == "flat"
        ));
    }

    #[test]
    fn delete_connection_wire_format_is_kebab_tagged() {
        let json = serde_json::to_string(&ControlCommand::DeleteConnection {
            client_connection_id: "c1".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"cmd":"delete-connection","client_connection_id":"c1"}"#
        );
    }
}

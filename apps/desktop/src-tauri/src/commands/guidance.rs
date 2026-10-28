//! 使用准则投递到宿主规则文件（ADR-020 主动性轴的强通道）。
//!
//! Hub 注入的 `instructions` 由 host 决定要不要读；宿主自己的规则文件则进
//! 系统提示词层。同一份 `workspaces.usage_guidance` 两路投递，这里管第二路。
//!
//! **默认不投递**：这是用户自己的文件，得他逐个点头。撤销必须能还原原样。

use mcpane_config::guidance;
use std::path::PathBuf;

/// 一个可投递目标的完整状态——支持与否、写哪儿、现在投没投。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuidanceTarget {
    /// 与 `ClientType` 同一套取值，前端按它调 deliver/revoke。
    pub client_type: String,
    pub label: String,
    /// 规则文件绝对路径；不支持的客户端为 None。
    pub path: Option<String>,
    pub supported: bool,
    /// 该文件当前是否有 MCPane 托管区块。
    pub delivered: bool,
    /// 区块里的内容与**当前工作区**的准则不一致。多工作区时这很常见——
    /// 投递的是工作区 A 的准则，切到 B 再看，「已投递」说的就不是 B 的内容了。
    pub stale: bool,
    /// 为什么得手动来。不能自动投递的目标照样列出来并说清楚——
    /// 静默隐藏只会让用户以为 MCPane 漏了它。
    pub unsupported_reason: Option<String>,
    /// 手动投递的操作步骤（自动投递的目标为空）。
    pub steps: Vec<String>,
    /// 落点是否经过实测。false = 路径按文档推定，写进去客户端读不读还没验过——
    /// UI 必须说出来，否则就是让用户以为配好了。
    pub path_verified: bool,
    /// 读取该文件时的错误（如托管区块残缺）。有值时 UI 应拦住投递。
    pub error: Option<String>,
}

/// 各客户端的全局规则文件位置。目录约定与各 adapter 的 discovery 保持一致
/// （`CLAUDE_CONFIG_DIR` / `CODEX_HOME` 覆盖照旧生效）。
///
/// 只列**真机确认存在**的落点。GUI 编辑器类客户端把规则存在应用内部存储或
/// 项目目录里，MCPane 够不着——那条路走手动指引（[`MANUAL`]）。
fn rules_path(client_type: &str) -> Option<PathBuf> {
    let home = super::connect::user_home();
    match client_type {
        "claude" => Some(mcpane_adapter_claude::claude_config_dir(&home).join("CLAUDE.md")),
        "codex" => Some(
            std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".codex"))
                .join("AGENTS.md"),
        ),
        "gemini-cli" => Some(home.join(".gemini").join("GEMINI.md")),
        "zcode" => Some(home.join(".zcode").join("AGENTS.md")),
        _ => None,
    }
}

/// 能自动投递的客户端：`(id, 显示名, 是否**实测过该客户端会读这个文件**)`。
/// 顺序即 UI 顺序。
///
/// 第三项的判据要卡死在「读取侧」：写入侧跑通（文件写对了、幂等、可撤销）
/// 说明不了客户端会不会理它。没实测还让用户以为配好了，就是一次静默失败——
/// 勾完开关、以为准则生效了，实际什么也没发生，这类「看着成功」的失败最难自查。
const SUPPORTED: [(&str, &str, bool); 4] = [
    // Claude Code 会把 ~/.claude/CLAUDE.md 注入系统提示词，这在本仓开发过程中
    // 反复可见（会话里那份用户全局指令就是它）。
    ("claude", "Claude Code", true),
    // 中间两家**只验证了写入侧**，没验证客户端是否真的读：
    // Codex 的 AGENTS.md 是社区通行约定但未实测；
    ("codex", "Codex CLI", false),
    // Gemini CLI 的落点按官方文档推定，本机连文件都没有；
    ("gemini-cli", "Gemini CLI", false),
    // ZCode 的读取侧在它自带的 agent 内核里核对过（ZCode.app 内的 glm/zcode.cjs）：
    // 上下文装配无条件走 resolveUserInstructions，默认只认 AGENTS.md，用户级候选恒为
    // `<home>/.zcode/AGENTS.md`，存在即以 scope="user" 读进上下文，与工作区那份合并而非被顶掉。
    ("zcode", "ZCode", true),
];

/// 手动投递的操作指引。
///
/// **步骤刻意不写死菜单层级**：各家 UI 随版本漂移，写死了过期得比谁都快，
/// 而用户照着一条过期路径找不到东西，比没有指引更糟。写「在设置里搜什么词」
/// 这类抗漂移的表述，以及多方一致、格式稳定的文件路径。
struct ManualGuide {
    client_type: &'static str,
    label: &'static str,
    /// 一句话说明为什么得手动来。
    reason: &'static str,
    steps: &'static [&'static str],
}

const MANUAL: [ManualGuide; 10] = [
    ManualGuide {
        client_type: "cursor",
        label: "Cursor",
        reason: "Cursor 的全局规则（User Rules）保存在应用内部存储中而非文件，MCPane 无法写入。",
        steps: &[
            "复制右侧的准则文本。",
            "打开 Cursor 设置，找到 Rules（可在设置里搜 “Rules”），粘贴到 User Rules，对所有项目生效。",
            "如仅需对当前项目生效，改为在项目根目录创建 .cursor/rules/mcpane.mdc，文件开头添加 --- / alwaysApply: true / --- frontmatter，其后写入准则正文。",
        ],
    },
    ManualGuide {
        client_type: "vscode",
        label: "VS Code",
        reason: "VS Code 的 Copilot 自定义指令按项目存放，MCPane 不管理项目目录。",
        steps: &[
            "复制右侧的准则文本。",
            "在项目根目录创建 .github/copilot-instructions.md，写入准则文本。",
            "确认 VS Code 设置里启用了「使用指令文件」（可在设置里搜 “instructions”）。",
        ],
    },
    ManualGuide {
        client_type: "cline",
        label: "Cline",
        reason: "Cline 的全局规则目录在各平台位置不一致（官方文档说法不一），建议在客户端内直接打开该目录。",
        steps: &[
            "复制右侧的准则文本。",
            "在项目根目录创建 .clinerules/mcpane.md 并写入准则文本；项目级规则位置在各版本中一致，最为可靠。",
            "如需对所有项目生效，通过 Cline 面板的 Rules 入口打开全局规则目录，在其中创建同样的文件。",
        ],
    },
    ManualGuide {
        client_type: "claude-desktop",
        label: "Claude Desktop",
        reason: "Claude Desktop 没有项目外的规则文件，指令保存在应用内。",
        steps: &[
            "复制右侧的准则文本。",
            "在 Claude Desktop 中打开对应的 Project，将准则写入该 Project 的自定义指令。",
            "不使用 Project 时，每次新对话都需手动附带准则，建议改用 Claude Code。",
        ],
    },
    ManualGuide {
        client_type: "trae",
        label: "Trae",
        reason: "Trae 的规则保存在应用内，MCPane 仅写入其 MCP 配置。",
        steps: &[
            "复制右侧的准则文本。",
            "在 Trae 设置中找到规则（Rules）相关项，粘贴准则文本。",
            "若该版本支持项目规则文件，也可写入项目根目录的规则文件。",
        ],
    },
    ManualGuide {
        client_type: "qoder",
        label: "Qoder",
        reason: "Qoder 的规则保存在应用内，MCPane 仅写入其 MCP 配置。",
        steps: &[
            "复制右侧的准则文本。",
            "在 Qoder 设置中找到规则（Rules）相关项，粘贴准则文本。",
            "若该版本支持项目规则文件，也可写入项目根目录的规则文件。",
        ],
    },
    // Antigravity 的规则机制在它自带的 agent 内核里核对过（Antigravity IDE.app 内的
    // language_server）：规则只有目录级的 GEMINI.md / AGENTS.md，没有用户级全局规则文件。
    ManualGuide {
        client_type: "antigravity",
        label: "Antigravity",
        reason: "Antigravity 的规则仅按目录层级发现（GEMINI.md / AGENTS.md），不存在可写入的全局规则文件。",
        steps: &[
            "复制右侧的准则文本。",
            "在项目根目录创建 AGENTS.md（或 GEMINI.md）并写入准则文本；此类文件无需 frontmatter，对该目录及其子目录始终生效。",
            "跨项目复用需在每个仓库各放一份：Antigravity 没有用户级全局规则文件。",
        ],
    },
    // 下面三家的规则机制**我们没有核实过**。不写具体路径（编一条假的比不写更糟），
    // 但必须列出来：漏掉它们就成了「静默隐藏」，正是本功能一开始要避免的事——
    // 用户找不到自己的客户端，只会以为 MCPane 忘了它。
    ManualGuide {
        client_type: "devin",
        label: "Devin",
        reason: "Devin 的规则存放位置尚未核实，因此不提供具体路径。",
        steps: &[
            "复制右侧的准则文本。",
            "在 Devin 设置中找到规则（Rules）或自定义指令入口，粘贴准则文本。",
            "若支持项目级规则文件，写入项目根目录同样有效。",
        ],
    },
    ManualGuide {
        client_type: "qwen-code",
        label: "Qwen Code",
        reason: "Qwen Code 的规则存放位置尚未核实，因此不提供具体路径。",
        steps: &[
            "复制右侧的准则文本。",
            "在 Qwen Code 设置中找到规则（Rules）或自定义指令入口，粘贴准则文本。",
            "若支持项目级规则文件，写入项目根目录同样有效。",
        ],
    },
    ManualGuide {
        client_type: "codebuddy",
        label: "CodeBuddy",
        reason: "CodeBuddy 的规则存放位置尚未核实，因此不提供具体路径。",
        steps: &[
            "复制右侧的准则文本。",
            "在 CodeBuddy 设置中找到规则（Rules）或自定义指令入口，粘贴准则文本。",
            "若支持项目级规则文件，写入项目根目录同样有效。",
        ],
    },
];

#[tauri::command]
pub async fn guidance_targets(workspace_id: Option<String>) -> Result<Vec<GuidanceTarget>, String> {
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    // 拿当前工作区的准则做对比基准：投递是按工作区走的，状态也得按工作区解释。
    let current = super::workspace::workspace_repo()
        .await?
        .usage_guidance(&workspace)
        .await
        .map_err(|e| e.to_string())?;
    let current = current.trim().to_owned();

    let mut targets: Vec<GuidanceTarget> = SUPPORTED
        .iter()
        .map(|(client_type, label, path_verified)| {
            let path = rules_path(client_type);
            // 读失败（区块残缺）不当成「没投递」——那会诱导用户再写一次，
            // 而写入同样会失败。原样报出来让 UI 拦住。
            let (delivered, stale, error) = match path.as_deref().map(guidance::read_block) {
                Some(Ok(Some(block))) => (true, block.trim() != current, None),
                Some(Ok(None)) => (false, false, None),
                Some(Err(e)) => (false, false, Some(e.to_string())),
                None => (false, false, None),
            };
            GuidanceTarget {
                client_type: (*client_type).to_owned(),
                label: (*label).to_owned(),
                path: path.map(|p| p.display().to_string()),
                supported: true,
                delivered,
                stale,
                unsupported_reason: None,
                steps: Vec::new(),
                path_verified: *path_verified,
                error,
            }
        })
        .collect();
    targets.extend(MANUAL.iter().map(|guide| GuidanceTarget {
        client_type: guide.client_type.to_owned(),
        label: guide.label.to_owned(),
        path: None,
        supported: false,
        // 手动投递没有可靠的「已完成」状态可查：规则可能落在应用内部存储里，
        // MCPane 读不到。与其自报一个会撒谎的状态，不如不报。
        delivered: false,
        stale: false,
        unsupported_reason: Some(guide.reason.to_owned()),
        steps: guide.steps.iter().map(|s| (*s).to_owned()).collect(),
        // 手动投递没有"我们写的落点"，这个标记对它无意义。
        path_verified: true,
        error: None,
    }));
    Ok(targets)
}

/// 把当前工作区的使用准则写进某客户端的规则文件（幂等）。
#[tauri::command]
pub async fn guidance_deliver(
    client_type: String,
    workspace_id: Option<String>,
) -> Result<(), String> {
    let path =
        rules_path(&client_type).ok_or_else(|| format!("{client_type} 不支持规则文件投递"))?;
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    let text = super::workspace::workspace_repo()
        .await?
        .usage_guidance(&workspace)
        .await
        .map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Err("工作区使用准则为空，请先在工作区页填写内容".to_owned());
    }
    guidance::upsert_block(&path, &text).map_err(|e| e.to_string())
}

/// 撤销投递：删掉托管区块，区块外内容原样保留。
#[tauri::command]
pub async fn guidance_revoke(client_type: String) -> Result<(), String> {
    let path =
        rules_path(&client_type).ok_or_else(|| format!("{client_type} 不支持规则文件投递"))?;
    guidance::remove_block(&path).map_err(|e| e.to_string())
}

/// 可直接粘贴的准则文本：自动投递前的预览，也是手动投递的复制内容。
/// 「明示写什么内容」是这个特性能默认开启的前提。
///
/// 带一个 markdown 小标题——贴进规则文件后它仍是一份读得下去的文档；
/// **不带 MCPANE:BEGIN/END 标记**：手动那条路要么独占一个文件、要么进设置
/// 文本框，两种情况都不需要边界标记，带上只是噪音。
#[tauri::command]
pub async fn guidance_preview(workspace_id: Option<String>) -> Result<String, String> {
    let workspace = workspace_id.unwrap_or_else(|| "ws_default".into());
    let text = super::workspace::workspace_repo()
        .await?
        .usage_guidance(&workspace)
        .await
        .map_err(|e| e.to_string())?;
    Ok(pasteable(&text))
}

/// 准则正文 → 可粘贴文档。空准则返回空串，让 UI 据此禁用复制。
fn pasteable(guidance: &str) -> String {
    let body = guidance.trim();
    if body.is_empty() {
        return String::new();
    }
    format!("## MCP 工具使用准则\n\n{body}\n")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// 手动投递的目标必须有原因**和**可照做的步骤——只说「不支持」等于把
    /// 用户丢在半路，这正是本轮要补的缺口。
    #[test]
    fn manual_targets_carry_a_reason_and_actionable_steps() {
        for guide in &MANUAL {
            assert!(
                rules_path(guide.client_type).is_none(),
                "{} 能自动投递就不该出现在手动清单里",
                guide.client_type
            );
            assert!(!guide.reason.is_empty(), "{} 缺原因", guide.client_type);
            assert!(
                guide.steps.len() >= 2,
                "{} 的步骤太少，照着做不出来",
                guide.client_type
            );
            assert!(
                guide.steps[0].contains("复制"),
                "{} 的第一步该是复制准则文本",
                guide.client_type
            );
        }
        let cursor = MANUAL.iter().find(|g| g.client_type == "cursor").unwrap();
        assert!(
            cursor.steps.iter().any(|s| s.contains("User Rules")),
            "Cursor 的步骤要指出 User Rules 这个去处"
        );
    }

    /// 清单必须覆盖**全部**内置客户端。漏一个，它就在 UI 上凭空消失——
    /// 用户只会以为 MCPane 忘了它，而这正是本功能开篇要避免的「静默隐藏」。
    /// 新接一个客户端时这条会转红，逼你顺手决定它走自动还是手动。
    #[test]
    fn every_builtin_client_appears_somewhere_in_the_list() {
        // 整表派生，不手抄：抄漏一个，这条测试照样绿，而那个客户端就在投递
        // 清单里凭空消失了。claude / codex 走专用 adapter，不在 CLIENTS 表里。
        let builtin = ["claude", "codex"]
            .into_iter()
            .chain(crate::mcp_clients::CLIENTS.iter().map(|spec| spec.id));
        for client_type in builtin {
            let auto = SUPPORTED.iter().any(|(id, _, _)| *id == client_type);
            let manual = MANUAL.iter().any(|g| g.client_type == client_type);
            assert!(
                auto || manual,
                "{client_type} 两张表都没有，它会在投递清单里凭空消失"
            );
        }
    }

    /// 自动与手动两张表不得重叠，也不得互相漏掉同一个客户端。
    #[test]
    fn supported_and_manual_tables_do_not_overlap() {
        for (client_type, _, _) in SUPPORTED {
            assert!(
                !MANUAL.iter().any(|g| g.client_type == client_type),
                "{client_type} 同时出现在两张表里"
            );
        }
    }

    /// 可粘贴文本带小标题、不带托管标记（手动场景下标记是噪音）。
    #[test]
    fn pasteable_text_is_a_readable_document_without_block_markers() {
        let out = pasteable("  优先用 Hub 的工具  ");
        assert!(out.starts_with("## MCP 工具使用准则"), "{out}");
        assert!(out.contains("优先用 Hub 的工具"));
        assert!(
            !out.contains("MCPANE:BEGIN"),
            "手动路径不该带边界标记：{out}"
        );
        assert_eq!(pasteable("   "), "", "空准则要给空串，让 UI 禁用复制");
    }

    /// 支持的三家都得解析出具体文件名，别只给目录。
    #[test]
    fn supported_targets_resolve_to_markdown_files() {
        for (client_type, _, _) in SUPPORTED {
            let path = rules_path(client_type).expect(client_type);
            assert_eq!(
                path.extension().and_then(|e| e.to_str()),
                Some("md"),
                "{client_type} 的规则文件应是 markdown：{path:?}"
            );
        }
    }
}

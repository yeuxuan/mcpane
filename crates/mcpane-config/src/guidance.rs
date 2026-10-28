//! 工作区使用准则投递到宿主规则文件（`CLAUDE.md` / `AGENTS.md` / `GEMINI.md`）。
//!
//! Hub 注入的 `instructions` 是建议性通道——是否进模型上下文由 host 决定；
//! 宿主的规则文件则进系统提示词层，是「模型肯不肯用工具」这一轴上权重最高的
//! 通道（ADR-020 主动性轴）。同一份 `workspaces.usage_guidance` 两路投递。
//!
//! 写入只碰托管区块，区块外一字不动：这是用户自己的文件，不是 MCPane 的。
//! 区块残缺（有 BEGIN 无 END）一律报错而不是猜边界——猜错就是吞掉用户内容，
//! 与 ConfigEngine「绝不静默覆盖」同一条原则。

use crate::document::FileFormatting;
use crate::error::ConfigError;
use std::path::Path;

/// 托管区块的起始标记**前缀**：真实行尾还跟着一句给人看的说明，故只匹配前缀。
const BEGIN_PREFIX: &str = "<!-- MCPANE:BEGIN";
const END_MARK: &str = "<!-- MCPANE:END -->";

/// 起始标记整行（写入用）。说明文字直接写在标记里——用户翻开文件第一眼就该
/// 知道这段谁在管、去哪儿改。
const BEGIN_LINE: &str = "<!-- MCPANE:BEGIN 本段由 MCPane 自动生成，请在 MCPane 工作区页修改 -->";

/// 区块正文的小标题：让规则文件本身读起来仍是一份正常的 markdown。
const BLOCK_HEADING: &str = "## MCP 工具使用准则";

/// 定位托管区块在原文中的字节范围。无区块返回 `Ok(None)`；
/// 有头无尾返回 `Err`——残缺区块的边界只能靠猜，而猜错代价是删掉用户内容。
fn locate_block(original: &str) -> Result<Option<(usize, usize)>, ConfigError> {
    let Some(begin) = original.find(BEGIN_PREFIX) else {
        return Ok(None);
    };
    let Some(end_rel) = original[begin..].find(END_MARK) else {
        return Err(ConfigError::ValidationFailed(
            "规则文件中的 MCPane 托管区块只有起始标记、缺少 <!-- MCPANE:END -->，\
             无法安全定位边界；请手动修复或删除该标记后重试"
                .to_owned(),
        ));
    };
    Ok(Some((begin, begin + end_rel + END_MARK.len())))
}

/// 渲染托管区块正文（不含前后空行）。
fn render_block(guidance: &str) -> String {
    format!(
        "{BEGIN_LINE}\n{BLOCK_HEADING}\n\n{}\n{END_MARK}",
        guidance.trim()
    )
}

/// 把 guidance 拼进原文：已有区块整块替换，没有则追加到末尾。
/// 恒定吐 LF、不带 BOM，外壳由调用方按原文还原。
fn splice_block(original: &str, guidance: &str) -> Result<String, ConfigError> {
    let block = render_block(guidance);
    match locate_block(original)? {
        Some((begin, end)) => {
            let mut out = String::with_capacity(original.len() + block.len());
            out.push_str(&original[..begin]);
            out.push_str(&block);
            out.push_str(&original[end..]);
            Ok(out)
        }
        None if original.trim().is_empty() => Ok(format!("{block}\n")),
        None => {
            // 与前文空一行；原文末尾已有的换行不重复堆叠。
            Ok(format!("{}\n\n{block}\n", original.trim_end()))
        }
    }
}

/// 从原文中摘掉托管区块，连同它留下的空行一起收拾干净。
fn strip_block(original: &str) -> Result<String, ConfigError> {
    let Some((begin, end)) = locate_block(original)? else {
        return Ok(original.to_owned());
    };
    let head = original[..begin].trim_end();
    let tail = original[end..].trim_start_matches(['\n', '\r']);
    if head.is_empty() {
        return Ok(tail.to_owned());
    }
    if tail.is_empty() {
        return Ok(format!("{head}\n"));
    }
    Ok(format!("{head}\n\n{tail}"))
}

/// 读出当前托管区块里的 guidance 正文（不含标记与小标题）。
/// 无区块返回 `None`——UI 回显「这台机器上投递了什么」用。
pub fn read_block(path: &Path) -> Result<Option<String>, ConfigError> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(None); // 文件不存在等同于没投递过，不是错误。
    };
    let Some((begin, end)) = locate_block(&raw)? else {
        return Ok(None);
    };
    // 按位置剥壳，不按内容过滤：`filter(line != BLOCK_HEADING)` 会把用户准则
    // 正文里恰好同名的那一行也吞掉，读回的内容与写进去的对不上——投递状态
    // 就会误报「内容已过期」，而用户怎么改都消不掉。
    let inner = &raw[begin..end];
    let mut lines = inner.lines();
    lines.next(); // BEGIN 行
    let mut rest: Vec<&str> = lines.filter(|line| !line.starts_with(END_MARK)).collect();
    // 紧跟其后的小标题（写入时固定在这个位置）只脱这一次。
    if rest.first().map(|l| l.trim()) == Some(BLOCK_HEADING) {
        rest.remove(0);
    }
    Ok(Some(rest.join("\n").trim().to_owned()))
}

/// 投递（或更新）托管区块。幂等：同样的 guidance 写两次结果逐字节相同。
/// 文件不存在时连同父目录一起创建。
pub fn upsert_block(path: &Path, guidance: &str) -> Result<(), ConfigError> {
    if guidance.trim().is_empty() {
        // 空准则等于取消投递：留一个只有标记的空壳既没用又碍眼。
        return remove_block(path);
    }
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let formatting = FileFormatting::detect(&raw);
    let mut next = splice_block(raw.trim_start_matches(crate::document::BOM), guidance)?;
    formatting.restore(&mut next);
    write_text(path, &next)
}

/// 撤销投递：删掉托管区块，区块外内容逐字节保留。文件不存在时为 no-op。
pub fn remove_block(path: &Path) -> Result<(), ConfigError> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let formatting = FileFormatting::detect(&raw);
    let body = raw.trim_start_matches(crate::document::BOM);
    if locate_block(body)?.is_none() {
        return Ok(()); // 没投递过就别去动人家的文件。
    }
    let mut next = strip_block(body)?;
    formatting.restore(&mut next);
    write_text(path, &next)
}

/// 规则文件是用户可读的普通文档，不含凭据：不 harden 权限（收紧成属主独占
/// 反而会让用户的编辑器/同步工具犯难），但 fsync——规则丢一半比没写更糟。
fn write_text(path: &Path, content: &str) -> Result<(), ConfigError> {
    crate::atomic_io::atomic_write(path, content, false, true)
        .map_err(|error| ConfigError::io(format!("写入规则文件 {}", path.display()), error))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn appends_block_to_existing_file_without_touching_original() {
        let original = "# 我的规则\n\n- 永远说中文\n";
        let out = splice_block(original, "优先用 MCPane 的工具").unwrap();
        assert!(
            out.starts_with(original.trim_end()),
            "原文必须原样保留：{out}"
        );
        assert!(out.contains("优先用 MCPane 的工具"));
        assert!(out.contains(BEGIN_LINE) && out.contains(END_MARK));
    }

    #[test]
    fn upsert_is_idempotent_and_replaces_in_place() {
        let original = "# 头\n\n正文\n";
        let once = splice_block(original, "准则 A").unwrap();
        let twice = splice_block(&once, "准则 A").unwrap();
        assert_eq!(once, twice, "同样的准则写两次必须逐字节相同");

        let changed = splice_block(&once, "准则 B").unwrap();
        assert!(changed.contains("准则 B") && !changed.contains("准则 A"));
        assert_eq!(
            changed.matches(BEGIN_PREFIX).count(),
            1,
            "更新只能替换原区块，不能追加第二块：{changed}"
        );
        assert!(changed.starts_with("# 头"), "区块外内容不得被动：{changed}");
    }

    #[test]
    fn strip_restores_original_shape() {
        let original = "# 头\n\n正文\n";
        let spliced = splice_block(original, "准则").unwrap();
        assert_eq!(strip_block(&spliced).unwrap(), original);
    }

    #[test]
    fn strip_keeps_content_after_the_block() {
        let spliced = splice_block("前文\n", "准则").unwrap();
        let with_tail = format!("{spliced}\n# 用户后来加的\n");
        let stripped = strip_block(&with_tail).unwrap();
        assert!(stripped.contains("前文") && stripped.contains("# 用户后来加的"));
        assert!(!stripped.contains(BEGIN_PREFIX));
    }

    /// 有头无尾不猜边界——猜错就是把用户后面的内容全删掉。
    #[test]
    fn truncated_block_is_an_error_not_a_guess() {
        let broken = "# 头\n<!-- MCPANE:BEGIN -->\n准则\n\n用户的重要内容\n";
        assert!(splice_block(broken, "新准则").is_err());
        assert!(strip_block(broken).is_err());
    }

    #[test]
    fn empty_file_gets_only_the_block() {
        let out = splice_block("", "准则").unwrap();
        assert!(out.starts_with(BEGIN_LINE), "{out}");
        assert!(out.ends_with("-->\n"), "{out}");
    }

    #[test]
    fn crlf_and_bom_survive_round_trip() {
        let dir = std::env::temp_dir().join(format!("mcpane-guidance-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("CLAUDE.md");
        std::fs::write(&path, "\u{feff}# 头\r\n\r\n正文\r\n").unwrap();

        upsert_block(&path, "准则").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with('\u{feff}'), "BOM 必须还原");
        assert!(
            raw.contains("\r\n") && !raw.contains("\n\n\r"),
            "CRLF 必须还原：{raw:?}"
        );
        assert!(
            !raw.replace("\r\n", "").contains('\n'),
            "不得混入裸 LF：{raw:?}"
        );
        assert_eq!(read_block(&path).unwrap().as_deref(), Some("准则"));

        remove_block(&path).unwrap();
        let restored = std::fs::read_to_string(&path).unwrap();
        assert_eq!(restored, "\u{feff}# 头\r\n\r\n正文\r\n");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 准则正文里恰好有一行与托管区块的小标题同名时，读回的内容必须原样——
    /// 按内容过滤会把它吞掉，投递状态从此永远误报「内容已过期」，
    /// 而用户改什么都消不掉这个提示。
    #[test]
    fn guidance_containing_the_heading_text_round_trips_intact() {
        let dir = std::env::temp_dir().join(format!("mcpane-guidance-h-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("CLAUDE.md");

        let guidance = "先看这条\n## MCP 工具使用准则\n再看那条";
        upsert_block(&path, guidance).unwrap();
        assert_eq!(read_block(&path).unwrap().as_deref(), Some(guidance));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_guidance_removes_the_block() {
        let dir =
            std::env::temp_dir().join(format!("mcpane-guidance-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("AGENTS.md");
        std::fs::write(&path, "# 头\n").unwrap();
        upsert_block(&path, "准则").unwrap();
        upsert_block(&path, "   ").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# 头\n");
        assert!(read_block(&path).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }
}

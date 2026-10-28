//! 崩溃现场落盘：panic 时把组件、版本、位置与 backtrace 追加进
//! `$MCPANE_HOME/crash.log`，供发布后排查线上闪退。
//!
//! 放在本 crate 而非各自实现：desktop 与 hub 是两个进程，但共用同一个数据目录，
//! 崩溃也该落进同一份 crash.log（`component` 字段区分来源）。这正是本 crate
//! 收敛 `mcpane_home` 的同一个理由——双端各写一份必然漂移。
//!
//! 生产环境两端的 stderr 都无人接收（.app bundle 丢弃、hub 的输出转给
//! desktop 后同样丢弃），故这份文件是发布后唯一的崩溃线索。
//!
//! 设计取舍：
//! - 保留默认 hook（stderr 输出）后再写文件——dev 下行为不变，只是多一份留痕；
//! - 追加而非覆盖：连续崩溃的时序本身就是线索，覆盖会丢掉第一现场；
//! - 超过 `MAX_LOG_BYTES` 整体重置，避免反复崩溃把磁盘写满；
//! - 全程忽略 IO 错误：崩溃路径上再 panic 会掩盖原始现场。

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

/// crash.log 体积上限，超过则下次写入前清空重来。
const MAX_LOG_BYTES: u64 = 256 * 1024;

/// 安装全局 panic hook。应在 `main` 的最开头调用，越早覆盖面越大。
///
/// `component`：崩溃来源（`"desktop"` / `"hub"`），两端共用一份 crash.log。
/// `version`：调用方的 `env!("CARGO_PKG_VERSION")`——必须由调用方传入，
/// 在本 crate 内取会拿到 protocol 自己的版本号，那对排查毫无用处。
pub fn install(component: &'static str, version: &'static str) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 先让默认 hook 打 stderr：即使随后写文件失败，dev 下仍看得见。
        default_hook(info);
        append_record(&render_record(info, component, version));
    }));
}

fn crash_log_path() -> PathBuf {
    crate::mcpane_home().join("crash.log")
}

/// webview 侧错误落盘入口（window.onerror / unhandledrejection / ErrorBoundary）。
///
/// 与 panic 共用一份 crash.log（component="webview"）：生产环境前端异常
/// 否则会随 webview 控制台一起蒸发。`message` 由前端拼好（含 stack），
/// 此处只做体积截断与凭据脱敏——错误文本可能内插了带 Key 的 URL/报文。
pub fn append_webview_record(version: &str, message: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let sanitized = sanitize_secrets(&truncate_chars(message, 8 * 1024));
    let mut record = String::new();
    let _ = writeln!(record, "---- mcpane webview error @ unix {ts} ----");
    let _ = writeln!(record, "版本: {version}");
    let _ = writeln!(record, "信息: {sanitized}");
    append_record(&record);
}

/// 按字符边界截断（错误信息常含中文，字节截断会切出无效 UTF-8）。
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let truncated: String = text.chars().take(max_chars).collect();
    format!("{truncated}…[截断]")
}

/// 凭据脱敏：`sk-`/`Bearer ` 后的长 token 打码。零依赖的朴素扫描——
/// 误伤面按前缀收窄，不追求识别一切密钥形态（日志是内部排查用途）。
fn sanitize_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = find_secret_prefix(rest) {
        let (prefix_end, _) = pos;
        out.push_str(&rest[..prefix_end]);
        let tail = &rest[prefix_end..];
        let token_len = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .count();
        if token_len >= 12 {
            out.push_str("[REDACTED]");
        } else {
            out.push_str(&tail[..token_len]);
        }
        rest = &tail[token_len..];
    }
    out.push_str(rest);
    out
}

/// 返回下一个密钥前缀（`sk-` / `Bearer `）结束处的字节偏移。
fn find_secret_prefix(text: &str) -> Option<(usize, &'static str)> {
    let sk = text.find("sk-").map(|i| (i + 3, "sk-"));
    let bearer = text.find("Bearer ").map(|i| (i + 7, "Bearer "));
    match (sk, bearer) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

/// panic 信息渲染成一条记录。payload 覆盖 `panic!("literal")` 与
/// `panic!("{fmt}")` 两种形态（分别是 `&str` 与 `String`）。
fn render_record(info: &std::panic::PanicHookInfo<'_>, component: &str, version: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<非字符串 payload>".to_owned());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<未知位置>".to_owned());
    // force_capture：不依赖 RUST_BACKTRACE，用户机器上默认没设这个变量。
    let backtrace = std::backtrace::Backtrace::force_capture();

    let mut record = String::new();
    let _ = writeln!(record, "---- mcpane {component} panic @ unix {ts} ----");
    let _ = writeln!(record, "版本: {version}");
    let _ = writeln!(record, "位置: {location}");
    let _ = writeln!(record, "线程: {}", thread_name());
    let _ = writeln!(record, "信息: {payload}");
    let _ = writeln!(record, "backtrace:\n{backtrace}");
    record
}

fn thread_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_owned()
}

fn append_record(record: &str) {
    append_record_to(&crash_log_path(), record);
}

/// 路径注入版：`MCPANE_HOME` 改不得（`set_var` 需 unsafe，本 crate 禁用），
/// 故把落点提成参数以便测试。
fn append_record_to(path: &std::path::Path, record: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // 体积兜底：崩溃循环（如启动即 panic）会被反复触发。
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::remove_file(path);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(record.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// 全局 panic hook 是进程级共享资源：两个装卸 hook 的测试并行会互相顶替
    /// （A 的 panic 触发 B 的 hook，A 的 captured 落空而假失败）。用进程内锁把
    /// 它们串行化。文件类测试各用独立 tag 目录，无需此锁。
    static HOOK_GUARD: Mutex<()> = Mutex::new(());

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("mcpane-crash-{tag}-{nanos}"));
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    /// payload、位置、组件、版本都进得了记录——crash.log 的价值全在这几项。
    /// PanicHookInfo 无公开构造函数，只能借真实 panic 取到。
    #[test]
    fn record_carries_payload_location_component_and_version() {
        let _serial = HOOK_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let captured = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&captured);

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(mut slot) = sink.lock() {
                *slot = render_record(info, "hub", "9.9.9-test");
            }
        }));
        let outcome = std::panic::catch_unwind(|| panic!("触发测试 panic"));
        std::panic::set_hook(previous);
        assert!(outcome.is_err());

        let record = captured.lock().map(|g| g.clone()).unwrap_or_default();
        assert!(record.contains("触发测试 panic"), "payload 丢失: {record}");
        assert!(record.contains("panic_hook.rs"), "位置丢失: {record}");
        // 组件名区分 desktop / hub——两端共用一份 crash.log，认不出来源就没用。
        assert!(record.contains("hub"), "组件丢失: {record}");
        assert!(record.contains("9.9.9-test"), "版本丢失: {record}");
    }

    /// 版本必须来自调用方而非本 crate——写死 env! 会记成 protocol 的版本，
    /// 排查时完全对不上桌面端/hub 的实际版本。
    #[test]
    fn version_comes_from_caller_not_this_crate() {
        let _serial = HOOK_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let captured = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&captured);

        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Ok(mut slot) = sink.lock() {
                *slot = render_record(info, "desktop", "1.2.3-caller");
            }
        }));
        let _ = std::panic::catch_unwind(|| panic!("x"));
        std::panic::set_hook(previous);

        let record = captured.lock().map(|g| g.clone()).unwrap_or_default();
        assert!(record.contains("1.2.3-caller"));
        assert!(record.contains("desktop"));
    }

    /// 追加而非覆盖：连续崩溃的时序本身是线索。
    #[test]
    fn records_append_rather_than_overwrite() {
        let path = temp_dir("append").join("crash.log");
        append_record_to(&path, "第一次崩溃\n");
        append_record_to(&path, "第二次崩溃\n");

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(content.contains("第一次崩溃"));
        assert!(content.contains("第二次崩溃"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(&path));
    }

    /// 崩溃循环不会把磁盘写满：超上限先清空再写。
    #[test]
    fn oversized_log_is_reset_before_append() {
        let path = temp_dir("oversized").join("crash.log");
        let _ = std::fs::write(&path, vec![b'x'; (MAX_LOG_BYTES + 1) as usize]);

        append_record_to(&path, "新记录\n");

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(content, "新记录\n", "旧日志未被清空");
        let _ = std::fs::remove_dir_all(path.parent().unwrap_or(&path));
    }

    /// 凭据脱敏：sk-/Bearer 后的长 token 打码，短值与普通文本不动。
    #[test]
    fn webview_record_redacts_long_tokens_only() {
        let text = "fetch https://api.x/v1?key=sk-abcdef1234567890abcdef failed; \
                    header Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig; \
                    model sk-short ok";
        let out = sanitize_secrets(text);
        assert!(out.contains("sk-[REDACTED]"), "长 sk- token 未打码: {out}");
        assert!(
            out.contains("Bearer [REDACTED]"),
            "Bearer token 未打码: {out}"
        );
        assert!(out.contains("sk-short"), "短值不该误伤: {out}");
        assert!(out.contains("failed"), "上下文文本不得丢失: {out}");
        // 前缀在行尾（token 长度 0）不得死循环。
        assert_eq!(sanitize_secrets("ends with sk-"), "ends with sk-");
    }

    /// 截断按字符边界（中文错误信息字节截断会产出无效 UTF-8）。
    #[test]
    fn webview_record_truncates_by_chars() {
        let long = "错".repeat(9000);
        let out = truncate_chars(&long, 8 * 1024);
        assert!(out.chars().count() < 9000);
        assert!(out.ends_with("…[截断]"));
    }

    /// 目录不存在时自建：首次崩溃可能早于任何写数据目录的操作。
    #[test]
    fn missing_parent_dir_is_created() {
        let path = temp_dir("mkdir").join("nested").join("crash.log");
        append_record_to(&path, "记录\n");
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(path.parent().and_then(|p| p.parent()).unwrap_or(&path));
    }
}

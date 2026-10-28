//! 上游 SSE 字节流的公共解析（两个协议转换器共用）。

/// 从缓冲里取出下一个事件的 data 负载（多行 data 以换行拼接）；无 data 的块跳过。
pub(crate) fn take_data(buffer: &mut String) -> Option<String> {
    loop {
        let (pos, len) = [("\r\n\r\n", 4), ("\n\n", 2)]
            .into_iter()
            .filter_map(|(delim, len)| buffer.find(delim).map(|pos| (pos, len)))
            .min_by_key(|(pos, _)| *pos)?;
        let block = buffer[..pos].to_owned();
        buffer.drain(..pos + len);
        let data = block
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if !data.is_empty() {
            return Some(data);
        }
    }
}

/// 追加字节，跨块被截断的 UTF-8 尾巴留到下一块再解码。
pub(crate) fn append_utf8(buffer: &mut String, remainder: &mut Vec<u8>, bytes: &[u8]) {
    remainder.extend_from_slice(bytes);
    match std::str::from_utf8(remainder) {
        Ok(text) => {
            buffer.push_str(text);
            remainder.clear();
        }
        Err(error) => {
            let valid = error.valid_up_to();
            buffer.push_str(&String::from_utf8_lossy(&remainder[..valid]));
            // 非截断型坏字节直接有损落地，避免卡死。
            if error.error_len().is_some() {
                buffer.push_str(&String::from_utf8_lossy(&remainder[valid..]));
                remainder.clear();
            } else {
                remainder.drain(..valid);
            }
        }
    }
}

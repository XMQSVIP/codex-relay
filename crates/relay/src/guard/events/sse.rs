//! Frames SSE bytes and withholds completion until the entire stream is validated.
use super::{TextEvents, blocked};
use crate::error::{Error, Result};
use serde_json::Value;

#[derive(Default)]
pub struct TextSse {
    events: TextEvents,
    buffer: Vec<u8>,
    bytes: usize,
    completion: Option<String>,
    closed: bool,
}
impl TextSse {
    pub fn for_client(request: &Value) -> Result<Self> {
        let mut names = crate::client_tools::tools(request)?;
        if request["tool_choice"] == "none" {
            names.clear();
        }
        if let Some(name) = request["tool_choice"]["name"].as_str() {
            names.retain(|n, _| n == name);
        }
        Ok(Self {
            events: TextEvents {
                client_tools: Some(names),
                ..Default::default()
            },
            ..Default::default()
        })
    }
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>> {
        if self.closed {
            return Err(Error::new("GUARD_CLOSED", "过滤器已关闭。"));
        }
        let result = self.push_inner(chunk);
        if result.is_err() {
            self.closed = true;
        }
        result
    }
    fn push_inner(&mut self, chunk: &[u8]) -> Result<Vec<String>> {
        self.bytes = self.bytes.checked_add(chunk.len()).ok_or_else(blocked)?;
        if self.bytes > 64 * 1024 * 1024 {
            return Err(Error::new("GUARD_SIZE", "响应超限。"));
        }
        self.buffer.extend_from_slice(chunk);
        let mut output = Vec::new();
        loop {
            let found = self
                .buffer
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| (i, 2));
            let crlf = self
                .buffer
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| (i, 4));
            let found = match (found, crlf) {
                (Some(a), Some(b)) => Some(if a.0 < b.0 { a } else { b }),
                (a, b) => a.or(b),
            };
            let Some((position, separator)) = found else {
                break;
            };
            if position > 1024 * 1024 {
                return Err(Error::new("GUARD_SIZE", "SSE 事件超限。"));
            }
            let raw = self
                .buffer
                .drain(..position + separator)
                .collect::<Vec<_>>();
            let frame = std::str::from_utf8(&raw[..position])
                .map_err(|_| Error::new("GUARD_SSE", "SSE UTF-8 无效。"))?;
            let mut data = Vec::new();
            let mut name = None;
            for line in frame.lines() {
                if let Some(v) = line.strip_prefix("data:") {
                    data.push(v.strip_prefix(' ').unwrap_or(v));
                } else if let Some(v) = line.strip_prefix("event:") {
                    name = Some(v.strip_prefix(' ').unwrap_or(v));
                } else if !line.is_empty()
                    && !line.starts_with(':')
                    && !line.starts_with("id:")
                    && !line.starts_with("retry:")
                {
                    return Err(Error::new("GUARD_SSE", "SSE 字段无效。"));
                }
            }
            if data.is_empty() {
                continue;
            }
            let joined = data.join("\n");
            if joined == "[DONE]" {
                self.events.finish()?;
                continue;
            }
            let value = serde_json::from_str(&joined)
                .map_err(|_| Error::new("GUARD_SSE", "SSE JSON 无效。"))?;
            if let Some(event) = self.events.accept(value, name)? {
                let frame = format!(
                    "event: {}\ndata: {}\n\n",
                    event["type"].as_str().unwrap(),
                    event
                );
                if event["type"] == "response.completed" {
                    self.completion = Some(frame);
                } else {
                    output.push(frame);
                }
            }
        }
        if self.buffer.len() > 1024 * 1024 {
            return Err(Error::new("GUARD_SIZE", "SSE 缓冲超限。"));
        }
        Ok(output)
    }
    pub fn finish(&mut self) -> Result<Vec<String>> {
        if self.closed || !self.buffer.iter().all(u8::is_ascii_whitespace) {
            self.closed = true;
            return Err(Error::new("GUARD_TRUNCATED", "SSE 未完整结束。"));
        }
        self.closed = true;
        self.events.finish()?;
        Ok(vec![self.completion.take().ok_or_else(|| {
            Error::new("GUARD_TRUNCATED", "SSE 缺少完成事件。")
        })?])
    }
}

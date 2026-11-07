//! Frames SSE bytes and withholds completion until the entire stream is validated.
use super::{TextEvents, blocked};
use crate::error::{Error, Result};
use serde_json::Value;

#[derive(Default)]
pub struct TextSse {
    events: TextEvents,
    frames: crate::sse::Frames,
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
        let mut output = Vec::new();
        let frames = self
            .frames
            .push(chunk, 1024 * 1024)
            .map_err(|_| Error::new("GUARD_SIZE", "SSE 事件超限。"))?;
        for raw in frames {
            let frame = std::str::from_utf8(&raw)
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
        Ok(output)
    }
    pub fn finish(&mut self) -> Result<Vec<String>> {
        if self.closed || !self.frames.pending_is_whitespace() {
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

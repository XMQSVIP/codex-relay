//! Optional ingress body capture. Never collects authentication headers.
use crate::{admin::AdminState, operations::Operations};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use uuid::Uuid;

const LIMIT: usize = 32 * 1024 * 1024;

#[derive(Default)]
struct Content {
    bytes: Vec<u8>,
    truncated: bool,
    complete: bool,
}
impl Content {
    fn append(&mut self, bytes: &[u8]) {
        let take = bytes.len().min(LIMIT - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..take]);
        self.truncated |= take < bytes.len();
    }
    fn value(&self) -> Value {
        let (text, encoding) = match std::str::from_utf8(&self.bytes) {
            Ok(text) => (text.to_owned(), "utf8"),
            Err(_) => (STANDARD.encode(&self.bytes), "base64"),
        };
        json!({"body":text,"debug":true,"encoding":encoding,"textOmitted":false,"truncated":self.truncated,"complete":self.complete})
    }
}

pub(crate) struct Capture {
    ops: Arc<Operations>,
    id: Uuid,
    epoch: u64,
    request: Content,
    response: Content,
    transport: String,
    status: Option<u16>,
}
impl Capture {
    pub(crate) fn new(ops: Arc<Operations>, id: Uuid, transport: &str) -> Option<Self> {
        let epoch = ops.debug_epoch()?;
        Some(Self {
            ops,
            id,
            epoch,
            request: Content::default(),
            response: Content::default(),
            transport: transport.into(),
            status: None,
        })
    }
    pub(crate) fn request_frame(&mut self, value: &Value) {
        self.request.append(value.to_string().as_bytes());
        self.request.append(b"\n");
        self.request.complete = true;
    }
    pub(crate) fn response_frame(&mut self, value: &Value) {
        self.response.append(value.to_string().as_bytes());
        self.response.append(b"\n");
    }
    pub(crate) fn complete(&mut self) {
        self.response.complete = true;
    }
    fn save(&self) {
        if self.ops.debug_epoch() != Some(self.epoch) {
            return;
        }
        let mut response = self.response.value();
        let mut request = self.request.value();
        request["debugRevision"] = json!(self.epoch);
        response["debugRevision"] = json!(self.epoch);
        response["transport"] = json!(self.transport);
        response["statusCode"] = json!(self.status);
        self.ops
            .save_debug_content(self.id, self.epoch, &request, &response);
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.save();
    }
}

struct CapturedBody {
    stream: Pin<Box<dyn Stream<Item = Result<Bytes, axum::Error>> + Send>>,
    capture: Arc<Mutex<Capture>>,
    response: bool,
}
impl Stream for CapturedBody {
    type Item = Result<Bytes, axum::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = self.stream.as_mut().poll_next(cx);
        let mut capture = self.capture.lock().unwrap();
        if capture.ops.debug_epoch() != Some(capture.epoch) {
            capture.request.bytes.clear();
            capture.response.bytes.clear();
            return result;
        }
        let content = if self.response {
            &mut capture.response
        } else {
            &mut capture.request
        };
        match &result {
            Poll::Ready(Some(Ok(bytes))) => content.append(bytes),
            Poll::Ready(None) => content.complete = true,
            _ => {}
        }
        result
    }
}
fn wrap(body: Body, capture: Arc<Mutex<Capture>>, response: bool) -> Body {
    Body::from_stream(CapturedBody {
        stream: body.into_data_stream().boxed(),
        capture,
        response,
    })
}
pub(crate) async fn middleware(
    State(state): State<Arc<AdminState>>,
    request: Request,
    next: Next,
) -> Response {
    if request.method() != axum::http::Method::POST {
        return next.run(request).await;
    }
    let Some(context) = request
        .extensions()
        .get::<crate::execution::RequestContext>()
    else {
        return next.run(request).await;
    };
    let Some(capture) = Capture::new(state.operations.clone(), context.id, "http") else {
        return next.run(request).await;
    };
    let capture = Arc::new(Mutex::new(capture));
    let (parts, body) = request.into_parts();
    let response = next
        .run(Request::from_parts(
            parts,
            wrap(body, capture.clone(), false),
        ))
        .await;
    {
        let mut saved = capture.lock().unwrap();
        saved.status = Some(response.status().as_u16());
        if response
            .headers()
            .get("content-type")
            .is_some_and(|value| value.as_bytes().starts_with(b"text/event-stream"))
        {
            saved.transport = "sse".into();
        }
    }
    let (parts, body) = response.into_parts();
    Response::from_parts(parts, wrap(body, capture, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Arc<Operations>, Uuid) {
        let dir = tempfile::tempdir().unwrap();
        let ops = Operations::open(dir.path()).unwrap();
        let id = Uuid::new_v4();
        rusqlite::Connection::open(dir.path().join("operations.sqlite3")).unwrap().execute(
            "INSERT INTO requests(id,started_at,kind,action,status) VALUES(?1,?2,'inference','responses','succeeded')",
            rusqlite::params![id.to_string(),crate::now_ms() as i64],
        ).unwrap();
        (dir, ops, id)
    }
    #[tokio::test]
    async fn streamed_bytes_are_unchanged_and_success_content_is_saved() {
        let (_dir, ops, id) = fixture();
        ops.set_debug(true, 0).unwrap();
        let capture = Arc::new(Mutex::new(Capture::new(ops.clone(), id, "sse").unwrap()));
        let request = b"{ \"input\":\"private prompt\",\"reasoning\":{\"effort\":\"high\"} }";
        let bytes = axum::body::to_bytes(
            wrap(Body::from(request.as_slice()), capture.clone(), false),
            LIMIT,
        )
        .await
        .unwrap();
        assert_eq!(bytes.as_ref(), request);
        let response = "data: {\"delta\":\"hello\"}\n\ndata: [DONE]\n\n";
        let body = Body::from_stream(futures_util::stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"data: {\"delta\":\"hello\"}\n\n")),
            Ok(Bytes::from_static(b"data: [DONE]\n\n")),
        ]));
        let output = axum::body::to_bytes(wrap(body, capture.clone(), true), LIMIT)
            .await
            .unwrap();
        assert_eq!(output.as_ref(), response.as_bytes());
        drop(capture);
        let saved = ops.request_content(id).unwrap();
        assert_eq!(
            saved["request"]["body"],
            std::str::from_utf8(request).unwrap()
        );
        assert_eq!(saved["response"]["body"], response);
        assert_eq!(saved["response"]["complete"], true);
    }
    #[test]
    fn disable_clears_raw_content_and_old_captures_cannot_repopulate_after_reenable() {
        let (_dir, ops, id) = fixture();
        assert!(Capture::new(ops.clone(), id, "http").is_none());
        ops.set_debug(true, 0).unwrap();
        let mut old = Capture::new(ops.clone(), id, "websocket").unwrap();
        old.request_frame(&json!({"input":"private"}));
        old.response_frame(&json!({"delta":"partial"}));
        old.save();
        assert_eq!(
            ops.request_content(id).unwrap()["response"]["complete"],
            false
        );
        assert!(ops.set_debug(false, 0).is_err());
        ops.set_debug(false, 1).unwrap();
        assert!(ops.request_content(id).unwrap()["request"].is_null());
        ops.set_debug(true, 2).unwrap();
        drop(old);
        assert!(ops.request_content(id).unwrap()["response"].is_null());
    }
    #[tokio::test]
    async fn partial_stream_is_delivered_immediately_and_saved_on_drop() {
        let (_dir, ops, id) = fixture();
        ops.set_debug(true, 0).unwrap();
        let capture = Arc::new(Mutex::new(Capture::new(ops.clone(), id, "sse").unwrap()));
        let stream = futures_util::stream::once(async {
            Ok::<_, std::io::Error>(Bytes::from_static(b"data: partial\n\n"))
        })
        .chain(futures_util::stream::pending());
        let mut output = wrap(Body::from_stream(stream), capture, true).into_data_stream();
        let first = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.as_ref(), b"data: partial\n\n");
        drop(output);
        let saved = ops.request_content(id).unwrap();
        assert_eq!(saved["response"]["body"], "data: partial\n\n");
        assert_eq!(saved["response"]["complete"], false);
    }
    #[test]
    fn restart_disables_capture_and_removes_saved_raw_content() {
        let (dir, ops, id) = fixture();
        ops.set_debug(true, 0).unwrap();
        let mut capture = Capture::new(ops.clone(), id, "http").unwrap();
        capture.request_frame(&json!({"input":"private"}));
        drop(capture);
        drop(ops);
        let reopened = Operations::open(dir.path()).unwrap();
        assert_eq!(reopened.debug_settings()["enabled"], false);
        assert!(reopened.request_content(id).unwrap()["request"].is_null());
    }
    #[test]
    fn binary_is_lossless_and_oversized_content_is_marked() {
        let mut content = Content::default();
        content.append(&[0xff, 0, 0x80]);
        let value = content.value();
        assert_eq!(value["encoding"], "base64");
        assert_eq!(
            STANDARD.decode(value["body"].as_str().unwrap()).unwrap(),
            [0xff, 0, 0x80]
        );
        content.append(&vec![b'x'; LIMIT]);
        assert_eq!(content.bytes.len(), LIMIT);
        assert!(content.truncated);
    }
}

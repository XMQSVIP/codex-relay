//! Shared ownership and diagnostics for native HTTP and WebSocket execution.
use crate::{
    error::{Error, Result},
    streaming::StreamSummary,
};
use std::time::Instant;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub id: Uuid,
    pub started: Instant,
    pub endpoint: Option<String>,
    pub websocket: bool,
    pub user_agent: Option<String>,
    pub subagent_kind: Option<String>,
    pub(crate) identity_headers: crate::responses::affinity::identity::Headers,
    pub(crate) native_handoff: std::sync::Arc<crate::profiles::continuations::Handoff>,
    pub(crate) native_observation: std::sync::Arc<crate::profiles::continuations::Observation>,
    pub(crate) admission: Option<std::sync::Arc<Admission>>,
}
pub(crate) struct Admission(pub std::sync::Mutex<Option<crate::operations::Lease>>);
impl std::fmt::Debug for Admission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Admission")
    }
}
impl RequestContext {
    pub(crate) fn take_admission(&self) -> Option<crate::operations::Lease> {
        self.admission
            .as_ref()
            .and_then(|slot| slot.0.lock().unwrap().take())
    }
    pub(crate) fn admit(&mut self, lease: crate::operations::Lease) {
        self.admission = Some(std::sync::Arc::new(Admission(std::sync::Mutex::new(Some(
            lease,
        )))));
    }
}
impl Default for RequestContext {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            started: Instant::now(),
            endpoint: None,
            websocket: false,
            user_agent: None,
            subagent_kind: None,
            identity_headers: Default::default(),
            native_handoff: Default::default(),
            native_observation: Default::default(),
            admission: None,
        }
    }
}

/// Local preparation ends at submission to app-server, before model output.
/// This is not a measurement of when encrypted upstream bytes leave the host.
pub(crate) struct PreflightTiming {
    context: RequestContext,
    last: Instant,
    stages: Vec<serde_json::Value>,
    pub cached_catalog: bool,
    pub reused_process: bool,
    pub native_process_id: Option<u32>,
    pub reused_thread: bool,
}
impl PreflightTiming {
    pub fn new(context: &RequestContext) -> Self {
        Self {
            context: context.clone(),
            last: context.started,
            stages: Vec::new(),
            cached_catalog: false,
            reused_process: false,
            native_process_id: None,
            reused_thread: false,
        }
    }
    pub fn mark(&mut self, stage: &'static str) {
        let now = Instant::now();
        self.stages.push(
            serde_json::json!({"stage":stage,"ms":now.duration_since(self.last).as_millis()}),
        );
        self.last = now;
    }
    pub fn submitted(&self) {
        self.submitted_as("native_turn_start_submit");
    }
    pub fn submitted_as(&self, end: &str) {
        eprintln!(
            "{}",
            serde_json::json!({
                "event":"request_preflight","requestId":self.context.id,
                "measurementEnd":end,
                "totalMs":self.context.started.elapsed().as_millis(),
                "cachedNativeCatalog":self.cached_catalog,"reusedNativeProcess":self.reused_process,"nativeProcessId":self.native_process_id,
                "reusedNativeThread":self.reused_thread,"stages":self.stages
            })
        );
    }
    pub fn acknowledged(&self) {
        eprintln!(
            "{}",
            serde_json::json!({
                "event":"native_turn_start_ack","requestId":self.context.id,
                "totalMs":self.context.started.elapsed().as_millis(),
                "afterSubmitMs":self.last.elapsed().as_millis()
            })
        );
    }
}

/// The model result remains available even when process cleanup or storage fails.
pub struct ExecutionOutcome {
    pub model: Result<StreamSummary>,
    pub cleanup_error: Option<Error>,
}
impl ExecutionOutcome {
    pub fn failed(error: Error) -> Self {
        Self {
            model: Err(error),
            cleanup_error: None,
        }
    }
    pub fn error(&self) -> Option<Error> {
        self.model
            .as_ref()
            .err()
            .cloned()
            .or_else(|| {
                self.model
                    .as_ref()
                    .ok()
                    .filter(|s| s.outcome != "completed")
                    .map(|s| {
                        Error::new(
                            s.error_code.as_deref().unwrap_or("UPSTREAM_REQUEST_FAILED"),
                            s.error_message.as_deref().unwrap_or("原生请求未完成。"),
                        )
                    })
            })
            .or_else(|| self.cleanup_error.clone())
    }
}

pub fn cancelled(code: &str) -> bool {
    matches!(
        code,
        "REQUEST_CANCELLED"
            | "REQUEST_INTERRUPTED"
            | "STREAM_CANCELLED"
            | "STREAM_CONSUMER_CLOSED"
            | "SERVICE_SHUTDOWN"
    )
}

pub fn failure_stage(code: &str) -> &'static str {
    match code {
        "RESPONSE_STORAGE_FULL" => "response_storage",
        "OPERATIONS_STORAGE" => "statistics_storage",
        "STREAM_BACKPRESSURE" | "STREAM_CONSUMER_CLOSED" => "downstream",
        "REQUEST_CANCELLED" | "REQUEST_INTERRUPTED" | "STREAM_CANCELLED" | "SERVICE_SHUTDOWN" => {
            "cancellation"
        }
        _ => "native_execution",
    }
}

/// Classify structured native facts only; arbitrary message text is not a policy input.
pub fn native_failure(value: &serde_json::Value) -> Error {
    // Native errors can carry the observed status inside a tagged enum variant.
    // Never infer a rate limit from arbitrary message text or unrelated fields.
    if [
        "httpConnectionFailed",
        "responseStreamConnectionFailed",
        "responseStreamDisconnected",
        "responseTooManyFailedAttempts",
    ]
    .iter()
    .any(|variant| value["codexErrorInfo"][variant]["httpStatusCode"].as_u64() == Some(429))
    {
        return Error::new("UPSTREAM_RATE_LIMITED", "原生模型请求被上游限流。");
    }
    let code = match value["codexErrorInfo"].as_str() {
        Some("rateLimitExceeded") => "UPSTREAM_RATE_LIMITED",
        Some("usageLimitExceeded" | "sessionBudgetExceeded") => "UPSTREAM_QUOTA_EXHAUSTED",
        Some("unauthorized") => "UPSTREAM_AUTH_FAILED",
        Some("serverOverloaded" | "flexUnavailable") => "UPSTREAM_CAPACITY",
        Some("badRequest" | "contextWindowExceeded") => "UPSTREAM_REQUEST_REJECTED",
        _ => "UPSTREAM_REQUEST_FAILED",
    };
    Error::new(code, "原生模型请求未完成；请使用请求 ID 查看诊断。")
}

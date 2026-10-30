use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: String,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::new("IO_ERROR", "本地读写或进程操作失败。")
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::new("INVALID_JSON", "数据格式不符合协议。")
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self.code.as_str() {
            "NOT_FOUND" => StatusCode::NOT_FOUND,
            "UNAUTHORIZED" | "ORIGIN_REJECTED" | "PUBLIC_ACCESS_DISABLED" => StatusCode::FORBIDDEN,
            "ADMIN_LOGIN_REQUIRED" | "ADMIN_PASSWORD_INVALID" => StatusCode::UNAUTHORIZED,
            "LOCAL_ACCOUNT_NOT_EMPTY"
            | "LOCAL_GROUP_UNAVAILABLE"
            | "LOCAL_INSPECTION_EXPIRED"
            | "BUSY"
            | "LOGIN_PORT_BUSY"
            | "RESET_PENDING"
            | "DESKTOP_RUNNING"
            | "RUNTIME_LOCKED"
            | "CONFIG_CONFLICT"
            | "OPERATIONS_LOCKED"
            | "REQUEST_CANCELLED"
            | "REQUEST_NOT_QUEUED" => StatusCode::CONFLICT,
            "CONCURRENCY_LIMIT" | "QUEUE_FULL" | "ADMIN_LOGIN_RATE_LIMIT" => {
                StatusCode::TOO_MANY_REQUESTS
            }
            "QUEUE_TIMEOUT" | "LOCAL_INSPECTION_TIMEOUT" => StatusCode::GATEWAY_TIMEOUT,
            "LOGIN_EXPIRED" => StatusCode::GONE,
            "INVALID_INPUT"
            | "LOGIN_CALLBACK_INVALID"
            | "PROVIDER_CONFIG"
            | "OAUTH_CREDENTIAL_INVALID"
            | "OAUTH_IMPORT_INCOMPLETE"
            | "OAUTH_IMPORT_REJECTED"
            | "OAUTH_TOKEN_EXPIRED"
            | "OAUTH_IDENTITY_MISMATCH" => StatusCode::BAD_REQUEST,
            "INFERENCE_DISABLED"
            | "DESKTOP_OAUTH_UNAVAILABLE"
            | "DESKTOP_INSTALLATION_STALE"
            | "OAUTH_NOT_SIGNED_IN"
            | "ROUTE_DISABLED"
            | "NO_ELIGIBLE_ACCOUNT"
            | "SERVICE_DRAINING"
            | "SERVICE_SHUTDOWN" => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::BAD_GATEWAY,
        };
        let retry = if self.code == "ADMIN_LOGIN_RATE_LIMIT" {
            "60"
        } else {
            "1"
        };
        let mut response = (status, Json(serde_json::json!({"error":self}))).into_response();
        if status == StatusCode::TOO_MANY_REQUESTS {
            response
                .headers_mut()
                .insert("retry-after", axum::http::HeaderValue::from_static(retry));
        }
        response
    }
}

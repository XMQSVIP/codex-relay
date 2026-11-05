//! Management authorization and public session status.

use super::AdminState;
use crate::error::{Error, Result};
use axum::{
    Json,
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub async fn authorize(
    State(state): State<Arc<AdminState>>,
    request: Request,
    next: Next,
) -> Result<Response> {
    if state.stop_requested.is_cancelled()
        && request.method() != axum::http::Method::GET
        && request.uri().path() != "/service/stop"
    {
        return Err(Error::new("SERVICE_SHUTDOWN", "中转站正在退出。"));
    }
    let headers = request.headers();
    if !state.management_allows(headers) {
        return Err(Error::new(
            "ORIGIN_REJECTED",
            "请求的 Host 或 Origin 不允许。",
        ));
    }
    if let Some(token) = &state.token {
        if headers.get_all("authorization").iter().count() != 1 {
            return Err(Error::new("UNAUTHORIZED", "请提供单一管理令牌。"));
        }
        let actual = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("")
            .as_bytes();
        let expected = token.as_bytes();
        let mut difference = actual.len() ^ expected.len();
        for (i, b) in expected.iter().enumerate() {
            difference |= usize::from(actual.get(i).copied().unwrap_or(0) ^ b);
        }
        if difference != 0 {
            return Err(Error::new(
                "UNAUTHORIZED",
                "管理令牌无效，请使用本次启动输出的管理链接。",
            ));
        }
    }
    if !state.access.authenticated(headers) {
        return Err(Error::new("ADMIN_LOGIN_REQUIRED", "请先登录管理后台。"));
    }
    Ok(next.run(request).await)
}
pub(super) async fn session(
    State(state): State<Arc<AdminState>>,
    request: Request,
) -> Result<Json<Value>> {
    if !state.management_allows(request.headers()) {
        return Err(Error::new(
            "ORIGIN_REJECTED",
            "请求的 Host 或 Origin 不允许。",
        ));
    }
    Ok(Json(
        json!({"requiresToken":state.token.is_some(), "passwordEnabled":state.access.enabled(), "authenticated":state.access.authenticated(request.headers()), "allowPublicAccess":state.access.public()}),
    ))
}

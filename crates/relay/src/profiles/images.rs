//! 图片传输复用独立工具的账户认证及清理边界。
use super::Profiles;
use crate::{
    error::Result,
    images::{ImageRequest, ImageResponse},
    inference::RequestJob,
    operations::Operations,
};
use tokio_util::sync::CancellationToken;

impl Profiles {
    pub(crate) async fn image_request(
        &self,
        job: &RequestJob,
        ops: &Operations,
        request: &ImageRequest,
        cancel: CancellationToken,
    ) -> Result<ImageResponse> {
        if !self.is_api_key(job.account).await?
            && job.access.allows(&job.provider.model)
            && let Ok(plan) = super::native_tools::Request::image(request).await
        {
            let value = match self
                .native_tool_request(job, ops, &plan, cancel.clone())
                .await
            {
                Ok(value) => Some(value),
                Err(error) if error.code == "NATIVE_TOOL_UNAVAILABLE" => None,
                Err(error) => return Err(error),
            };
            if let Some(value) = value {
                let mut headers = axum::http::HeaderMap::new();
                headers.insert(
                    "x-codex-relay-upstream-transport",
                    "native-tools".parse().unwrap(),
                );
                return Ok(ImageResponse {
                    status: axum::http::StatusCode::OK,
                    headers,
                    body: serde_json::to_vec(&value)?.into(),
                    value,
                });
            }
        }
        // Unsupported tool parameters select direct HTTP before any native turn.
        // A native execution error must never enter this path or replay the request.
        self.standalone_request(
            job,
            ops,
            request.operation.path(),
            cancel,
            |client, url, headers| async move {
                let response = client
                    .post(url)
                    .headers(headers)
                    .header(reqwest::header::CONTENT_TYPE, &request.content_type)
                    .timeout(crate::images::TIMEOUT)
                    .body(request.body.clone())
                    .send()
                    .await
                    .map_err(crate::images::http_error)?;
                let mut response = crate::images::read_response(response).await?;
                response.headers.insert(
                    "x-codex-relay-upstream-transport",
                    "direct-http".parse().unwrap(),
                );
                Ok(response)
            },
        )
        .await
    }
}

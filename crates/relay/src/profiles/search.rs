use super::Profiles;
use crate::{
    error::{Error, Result},
    inference::RequestJob,
    operations::Operations,
    search::{SearchRequest, SearchResponse},
};
use tokio_util::sync::CancellationToken;

impl Profiles {
    pub(crate) async fn search_request(
        &self,
        job: &RequestJob,
        ops: &Operations,
        request: &SearchRequest,
        cancel: CancellationToken,
    ) -> Result<SearchResponse> {
        let api_key = self.is_api_key(job.account).await?;
        let plan = super::native_tools::Request::search(request);
        let existing = crate::upstream::search_native_mode(ops, &job.access, &request.binding)?;
        let native = existing.unwrap_or(!api_key && plan.is_ok());
        if native {
            let plan = plan.map_err(|_| {
                Error::new(
                    "INVALID_INPUT",
                    "此搜索会话已使用官方工具；新增参数不受支持，请用新的 id 选择直连。",
                )
            })?;
            let value = match self
                .native_tool_request(job, ops, &plan, cancel.clone())
                .await
            {
                Ok(value) => Some(value),
                Err(error) if error.code == "NATIVE_TOOL_UNAVAILABLE" && existing.is_none() => None,
                Err(error) => return Err(error),
            };
            if let Some(value) = value {
                let mut headers = axum::http::HeaderMap::new();
                headers.insert(
                    "x-codex-relay-upstream-transport",
                    "native-tools".parse().unwrap(),
                );
                return Ok(SearchResponse {
                    status: axum::http::StatusCode::OK,
                    headers,
                    body: serde_json::to_vec(&value)?.into(),
                    value,
                });
            }
        }
        crate::upstream::bind_search(
            ops,
            &job.access,
            &request.binding,
            job.account,
            &job.model,
            &job.provider,
            false,
        )?;
        let send = |client: reqwest::Client, url, mut headers: reqwest::header::HeaderMap| async move {
            // 原生自带 Content-Type；使用 insert 保证不会产生两个冲突的请求头。
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                "application/json".parse().unwrap(),
            );
            let response = client
                .post(url)
                .headers(headers)
                .body(request.body.clone())
                .timeout(crate::search::TIMEOUT)
                .send()
                .await
                .map_err(crate::upstream::http_error)?;
            let mut response = crate::search::read_response(response).await?;
            response.headers.insert(
                "x-codex-relay-upstream-transport",
                "direct-http".parse().unwrap(),
            );
            Ok(response)
        };
        if !api_key {
            return self
                .standalone_request(job, ops, "alpha/search", cancel, send)
                .await;
        }
        let work = async {
            let config = self.upstream(job.account).await?;
            ops.check_inference_route(job.account, &job.access, &config.provider(), &job.model)?;
            let client = self.account_proxy(job.account).resolved().await?.client()?;
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(reqwest::header::AUTHORIZATION, config.authorization()?);
            send(client, config.url("alpha/search")?, headers).await
        };
        tokio::pin!(work);
        let mut checks = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => return Err(Error::new("REQUEST_CANCELLED", "搜索请求已取消。")),
                result = &mut work => return result,
                _ = checks.tick() => ops.check_inference_route(job.account, &job.access, &job.provider, &job.model)?,
            }
        }
    }
}

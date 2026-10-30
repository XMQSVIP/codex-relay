use super::{Profile, Profiles};
use crate::{
    error::Result,
    upstream::{Config, Input},
};
use uuid::Uuid;

impl Profiles {
    pub async fn is_api_key(&self, id: Uuid) -> Result<bool> {
        Ok(self.source(id).await? == "api-key")
    }
    pub(crate) async fn upstream(&self, id: Uuid) -> Result<Config> {
        self.require_source(id, "api-key").await?;
        let bytes = tokio::fs::read(self.root.join(id.to_string()).join("upstream.json")).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
    pub(crate) async fn create_upstream(&self, name: &str, config: Config) -> Result<Profile> {
        let profile = self.create_with_source(name, "api-key").await?;
        if let Err(error) = self.write_upstream(profile.id, &config).await {
            let _ = self.delete(profile.id).await;
            return Err(error);
        }
        Ok(profile)
    }
    pub(crate) async fn write_upstream(&self, id: Uuid, config: &Config) -> Result<()> {
        self.require_source(id, "api-key").await?;
        crate::oauth::save(
            &self.root.join(id.to_string()).join("upstream.json"),
            &serde_json::to_value(config)?,
        )
        .await
    }
    pub(crate) async fn validate_upstream(&self, id: Uuid, input: Input) -> Result<Config> {
        input.validate(Some(&self.upstream(id).await?))
    }
    pub(crate) async fn inspect_upstream(
        &self,
        id: Uuid,
        check: super::RouteCheck,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(
        crate::provider::Provider,
        crate::operations::NativeCapabilities,
        Option<crate::execution::ExecutionOutcome>,
    )> {
        let config = self.upstream(id).await?;
        config.authorization()?;
        let provider = config.provider();
        let client = self.account_proxy(id).resolved().await?.client()?;
        let mut caps = crate::operations::NativeCapabilities {
            provider: crate::operations::runtime::fingerprint(&provider),
            installation: "api-key".into(),
            checked_at: crate::now_ms(),
            ..Default::default()
        };
        let outcome = match check {
            super::RouteCheck::Access => {
                let fetch = async {
                    let response = client
                        .get(config.url("models")?)
                        .header(reqwest::header::AUTHORIZATION, config.authorization()?)
                        .send()
                        .await
                        .map_err(crate::upstream::http_error)?;
                    if !response.status().is_success() {
                        return Err(crate::error::Error::new(
                            "UPSTREAM_MODELS_UNAVAILABLE",
                            "上游暂不提供模型列表，可直接指定模型请求。",
                        ));
                    }
                    let value = crate::upstream::read_json(response).await?;
                    Ok::<_, crate::error::Error>(
                        value["data"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|v| {
                                v["id"]
                                    .as_str()
                                    .is_some_and(|s| !s.is_empty() && s.len() <= 256)
                            })
                            .cloned()
                            .collect::<Vec<_>>(),
                    )
                };
                match tokio::select! {_=cancel.cancelled()=>return Err(crate::error::Error::new("REQUEST_CANCELLED","请求已取消。")),r=fetch=>r}
                {
                    Ok(models) => caps.models = models,
                    Err(error) => {
                        caps.stale = true;
                        caps.error_code = Some(error.code);
                    }
                }
                None
            }
            super::RouteCheck::Connection { model } => {
                let model = model
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| crate::upstream::invalid("API Key 账户连接测试须指定模型。"))?;
                let work = async {
                    let response=client.post(config.url("responses")?).header(reqwest::header::AUTHORIZATION,config.authorization()?).timeout(std::time::Duration::from_secs(1800)).json(&serde_json::json!({"model":model,"input":"Reply OK.","stream":false,"max_output_tokens":16})).send().await.map_err(crate::upstream::http_error)?;
                    if !response.status().is_success() {
                        return Err(crate::upstream::status_error(response.status()));
                    }
                    let value = crate::upstream::read_json(response).await?;
                    if value["status"] == "failed" || value["error"].is_object() {
                        return Err(crate::error::Error::new(
                            "UPSTREAM_REQUEST_FAILED",
                            "上游连接测试失败。",
                        ));
                    }
                    Ok(crate::upstream::summary(&value, None))
                };
                Some(crate::execution::ExecutionOutcome {
                    model: tokio::select! {_=cancel.cancelled()=>Err(crate::error::Error::new("REQUEST_CANCELLED","请求已取消。")),r=work=>r},
                    cleanup_error: None,
                })
            }
        };
        Ok((provider, caps, outcome))
    }
}

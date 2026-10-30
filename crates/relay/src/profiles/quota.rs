use super::Profiles;
use crate::{
    backend::Backend,
    error::{Error, Result},
    operations::{Operations, quota},
    provider::Provider,
};
use serde_json::json;
use uuid::Uuid;

impl Profiles {
    /// The caller serializes quota operations and credential changes. Never stops inference workers.
    pub(crate) async fn read_quota(
        &self,
        id: Uuid,
        ops: &Operations,
        expected: &Provider,
    ) -> Result<()> {
        self.quota_action(id, ops, expected, None).await.map(|_| ())
    }
    pub(crate) async fn consume_quota_reset(
        &self,
        id: Uuid,
        ops: &Operations,
        expected: &Provider,
        request: &quota::ResetRequest,
    ) -> Result<(String, bool)> {
        self.quota_action(id, ops, expected, Some(request))
            .await?
            .ok_or_else(|| Error::new("RESET_UNSUPPORTED", "此账户不支持使用重置卡。"))
    }
    async fn quota_action(
        &self,
        id: Uuid,
        ops: &Operations,
        expected: &Provider,
        reset: Option<&quota::ResetRequest>,
    ) -> Result<Option<(String, bool)>> {
        if !expected.auth.starts_with(crate::oauth::AUTH) && expected.auth != "chatgpt" {
            if reset.is_some() {
                return Err(Error::new(
                    "RESET_UNSUPPORTED",
                    "此提供方不支持 ChatGPT 重置卡。",
                ));
            }
            return ops
                .publish_quota(
                    id,
                    expected,
                    format!("account:{id}"),
                    Err(Error::new(
                        "QUOTA_UNSUPPORTED",
                        "此提供方不支持 ChatGPT 额度。",
                    )),
                )
                .map(|_| None);
        }
        let isolated = self.is_oauth(id).await?;
        let installation = crate::installation::read_with_refresh().await?;
        let dir = self.inspection_directory()?;
        let home = self.home(id);
        let backend = Backend::start_direct_with_proxy(
            &installation,
            dir.path(),
            if isolated { Some(&home) } else { None },
            false,
            &self.account_proxy(id),
        )
        .await?;
        let result = async {
            let (provider, credential) = self
                .read_route(id, &backend, dir.path(), Some(&expected.model))
                .await?;
            if &provider != expected {
                return Err(Error::new(
                    "ROUTE_CHANGED",
                    "账户身份或配置已变化，请重新启用账户。",
                ));
            }
            let identity = if credential.is_some() {
                quota::identity(id, credential.as_ref())
            } else {
                let credential = crate::oauth::native_credential(&backend).await?;
                quota::identity(id, Some(&credential))
            };
            let outcome = if let Some(request) = reset {
                ops.require_reset_context(id, expected, &identity, &request.context)?;
                match consume_native_reset(&backend.rpc, request).await {
                    Ok(outcome) => Some(outcome),
                    Err(error) => {
                        if error.code == "RESET_UNCERTAIN" {
                            ops.quota_read_failed(id, expected, &error.code)?;
                        }
                        return Err(error);
                    }
                }
            } else {
                None
            };
            let reading = read_native_limits(&backend.rpc).await;
            let refreshed = reading.is_ok();
            ops.publish_quota_reading(id, expected, identity, reading)?;
            Ok(outcome.map(|outcome| (outcome, refreshed)))
        }
        .await;
        let stopped = backend.stop().await;
        match result {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) => stopped.map(|_| None),
            Err(error) => Err(error),
        }
    }
}

async fn consume_native_reset(
    rpc: &crate::rpc::Rpc,
    request: &quota::ResetRequest,
) -> Result<String> {
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        rpc.request(
            "account/rateLimitResetCredit/consume",
            json!({"idempotencyKey":request.idempotency_key,"creditId":request.credit_id}),
        ),
    )
    .await;
    let value = match response {
        Ok(Ok(value)) => value,
        Ok(Err(error)) if error.code == "RPC_METHOD_UNSUPPORTED" => {
            return Err(Error::new(
                "RESET_UNSUPPORTED",
                "当前原生版本不支持使用重置卡。",
            ));
        }
        _ => {
            return Err(Error::new(
                "RESET_UNCERTAIN",
                "无法确认重置结果。请使用原请求重试，避免重复消耗。",
            ));
        }
    };
    value["outcome"]
        .as_str()
        .filter(|s| {
            matches!(
                *s,
                "reset" | "alreadyRedeemed" | "nothingToReset" | "noCredit"
            )
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::new(
                "RESET_UNCERTAIN",
                "重置返回结果无法识别，请使用原请求重试。",
            )
        })
}
async fn read_native_limits(rpc: &crate::rpc::Rpc) -> Result<quota::QuotaReading> {
    let raw = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        rpc.request("account/rateLimits/read", json!({})),
    )
    .await
    .map_err(|_| Error::new("QUOTA_TIMEOUT", "额度读取超时。"))?
    .map_err(|e| {
        if e.code == "RPC_METHOD_UNSUPPORTED" {
            Error::new("QUOTA_UNSUPPORTED", "原生版本不支持额度读取。")
        } else {
            e
        }
    })?;
    quota::normalize_reading(&raw, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    #[tokio::test]
    async fn reset_protocol_reuses_key_and_reads_limits_without_inference() {
        let key = Uuid::new_v4();
        for outcome in ["reset", "alreadyRedeemed", "nothingToReset", "noCredit"] {
            let (client, server) = tokio::io::duplex(8192);
            let (read, write) = tokio::io::split(client);
            let rpc = crate::rpc::Rpc::from_io(read, write);
            let server = tokio::spawn(async move {
                let (read, mut write) = tokio::io::split(server);
                let mut lines = BufReader::new(read).lines();
                let request: serde_json::Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(request["method"], "account/rateLimitResetCredit/consume");
                assert_eq!(request["params"]["idempotencyKey"], key.to_string());
                assert_eq!(request["params"]["creditId"], "card-1");
                write
                    .write_all(
                        format!(
                            "{}\n",
                            json!({"id":request["id"],"result":{"outcome":outcome}})
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                let request: serde_json::Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert_eq!(request["method"], "account/rateLimits/read");
                write.write_all(format!("{}\n", json!({"id":request["id"],"result":{"rateLimits":{},"rateLimitResetCredits":{"availableCount":1}}})).as_bytes()).await.unwrap();
            });
            let request = quota::ResetRequest {
                idempotency_key: key,
                context: "a".repeat(64),
                credit_id: Some("card-1".into()),
            };
            assert_eq!(consume_native_reset(&rpc, &request).await.unwrap(), outcome);
            assert_eq!(
                read_native_limits(&rpc)
                    .await
                    .unwrap()
                    .reset_credits
                    .unwrap()
                    .available_count,
                1
            );
            server.await.unwrap();
            rpc.close();
        }
    }
    #[tokio::test]
    async fn unsupported_reset_method_is_reported_without_upstream_text() {
        let (client, server) = tokio::io::duplex(4096);
        let (read, write) = tokio::io::split(client);
        let rpc = crate::rpc::Rpc::from_io(read, write);
        let server = tokio::spawn(async move {
            let (read, mut write) = tokio::io::split(server);
            let line = BufReader::new(read)
                .lines()
                .next_line()
                .await
                .unwrap()
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            write.write_all(format!("{}\n", json!({"id":request["id"],"error":{"code":-32601,"message":"private-upstream-text"}})).as_bytes()).await.unwrap();
        });
        let request = quota::ResetRequest {
            idempotency_key: Uuid::new_v4(),
            context: "a".repeat(64),
            credit_id: None,
        };
        let error = consume_native_reset(&rpc, &request).await.unwrap_err();
        assert_eq!(error.code, "RESET_UNSUPPORTED");
        assert!(!error.message.contains("private-upstream-text"));
        server.await.unwrap();
        rpc.close();
    }
}

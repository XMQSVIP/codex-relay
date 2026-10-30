use super::Request;
use crate::{
    error::{Error, Result},
    rpc::Rpc,
};
use serde_json::{Value, json};

// Native silently omits an unadvertised Ultrafast tier. Check the actual worker
// catalog before submitting a turn or answering any pending tool callback.
pub(super) async fn verify(rpc: &Rpc, request: &Request) -> Result<()> {
    if request.service_tier.as_deref() != Some("ultrafast") {
        return Ok(());
    }
    let unavailable = || {
        Error::new(
            "SERVICE_TIER_UNAVAILABLE",
            "当前原生模型目录未声明 Ultrafast；请刷新账户模型目录并确认模型与账户权限，或选择其他服务等级。",
        )
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut cursor = Value::Null;
        for _ in 0..16 {
            let page = rpc
                .request(
                    "model/list",
                    json!({"limit":100,"includeHidden":true,"cursor":cursor}),
                )
                .await?;
            let models = page["data"].as_array().ok_or_else(unavailable)?;
            if let Some(model) = models
                .iter()
                .find(|model| model["model"] == request.model || model["id"] == request.model)
            {
                return if model["serviceTiers"]
                    .as_array()
                    .is_some_and(|tiers| tiers.iter().any(|tier| tier["id"] == "ultrafast"))
                {
                    Ok(())
                } else {
                    Err(unavailable())
                };
            }
            let next = page["nextCursor"].clone();
            if next.is_null() || next == cursor {
                break;
            }
            cursor = next;
        }
        Err(unavailable())
    })
    .await
    .map_err(|_| {
        Error::new(
            "SERVICE_TIER_UNAVAILABLE",
            "读取原生服务等级目录超时，未发送模型请求。",
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn cancelling_catalog_discovery_never_starts_a_native_turn() {
        let (client, server) = tokio::io::duplex(8192);
        let (r, w) = tokio::io::split(client);
        let rpc = Rpc::from_io(r, w);
        let cancel = tokio_util::sync::CancellationToken::new();
        let server_cancel = cancel.clone();
        let observe = async move {
            let (r, _w) = tokio::io::split(server);
            let mut lines = BufReader::new(r).lines();
            let frame: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(frame["method"], "model/list");
            server_cancel.cancel();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), lines.next_line())
                    .await
                    .is_err()
            );
        };
        let request =
            Request::parse(json!({"model":"m","input":"hi","service_tier":"ultrafast"})).unwrap();
        let provider = crate::provider::Provider {
            execution: Default::default(),
            id: "fixture".into(),
            model: "m".into(),
            base_url: "http://127.0.0.1:9/v1".into(),
            auth: "none".into(),
            forward_headers: vec![],
            required_headers: vec![],
            mcp_servers: vec![],
        };
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        let (result, ()) = tokio::join!(
            super::super::run(
                &rpc,
                &provider,
                std::path::Path::new("."),
                &request,
                &request.input,
                tx,
                cancel
            ),
            observe
        );
        assert_eq!(result.unwrap_err().code, "REQUEST_CANCELLED");
    }

    #[tokio::test]
    async fn ultrafast_requires_the_requested_model_to_advertise_it() {
        for advertised in [true, false] {
            let (client, server) = tokio::io::duplex(8192);
            let (r, w) = tokio::io::split(client);
            let rpc = Rpc::from_io(r, w);
            let task = tokio::spawn(async move {
                let (r, mut w) = tokio::io::split(server);
                let mut lines = BufReader::new(r).lines();
                for page in 0..2 {
                    let frame: Value =
                        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                    assert_eq!(frame["method"], "model/list");
                    assert_eq!(
                        frame["params"]["cursor"],
                        if page == 0 {
                            Value::Null
                        } else {
                            json!("next")
                        }
                    );
                    let result = if page == 0 {
                        json!({"data":[{"model":"other","serviceTiers":[{"id":"ultrafast"}]}],"nextCursor":"next"})
                    } else {
                        json!({"data":[{"model":"m","serviceTiers":[{"id":if advertised {"ultrafast"} else {"priority"}}]}],"nextCursor":null})
                    };
                    w.write_all(
                        format!("{}\n", json!({"id":frame["id"],"result":result})).as_bytes(),
                    )
                    .await
                    .unwrap();
                }
            });
            let request =
                Request::parse(json!({"model":"m","input":"hi","service_tier":"ultrafast"}))
                    .unwrap();
            let result = verify(&rpc, &request).await;
            if advertised {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().code, "SERVICE_TIER_UNAVAILABLE");
            }
            task.await.unwrap();
        }
    }
}

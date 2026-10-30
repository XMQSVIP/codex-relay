use super::*;
use crate::installation::NativeClientInfo;
use serde_json::json;
use std::{sync::atomic::Ordering, time::Duration};

#[tokio::test]
async fn shared_threads_isolate_events_callback_cleanup_and_rejection() {
    let (rpc, mut outgoing) = Rpc::new();
    rpc.enable_client_tools();
    let mut first = rpc.subscribe_thread("first");
    let mut second = rpc.subscribe_thread("second");
    for (id, thread) in [(1, "first"), (2, "second")] {
        rpc.receive(
            &json!({"id":id,"method":"item/tool/call","params":{"threadId":thread,"tool":"echo"}})
                .to_string(),
        )
        .unwrap();
    }
    assert_eq!(first.recv().await.unwrap()["id"], 1);
    assert_eq!(second.recv().await.unwrap()["id"], 2);
    rpc.clear_thread_tool_callbacks("first");
    rpc.answer_tool(json!(2), json!({"success":true}))
        .await
        .unwrap();
    assert_eq!(outgoing.recv().await.unwrap()["id"], 2);
    assert!(rpc.answer_tool(json!(1), json!({})).await.is_err());
    // Flooding a sibling cannot overrun this thread's notification buffer.
    for _ in 0..1024 {
        rpc.receive(
            &json!({"method":"item/agentMessage/delta","params":{"threadId":"first","delta":"x"}})
                .to_string(),
        )
        .unwrap();
    }
    assert!(matches!(
        second.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    rpc.receive(
        &json!({"id":3,"method":"unknown/callback","params":{"threadId":"first"}}).to_string(),
    )
    .unwrap();
    assert!(matches!(
        second.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    rpc.close();
    tokio::time::timeout(Duration::from_millis(100), rpc.closed())
        .await
        .unwrap();
}

#[tokio::test]
async fn host_time_callback_returns_unix_seconds_without_downstream_events() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    for id in [json!(17), json!("time-request")] {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        rpc.receive(
            &json!({"id":id,"method":"currentTime/read","params":{"threadId":"synthetic-thread"}})
                .to_string(),
        )
        .unwrap();
        let reply = messages.recv().await.unwrap();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(reply["id"], id);
        let seconds = reply["result"]["currentTimeAt"].as_u64().unwrap();
        assert!((before..=after).contains(&seconds));
    }
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn malformed_host_callbacks_cannot_become_downstream_notifications() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    for params in [
        json!(null),
        json!({}),
        json!({"threadId":false}),
        json!({"threadId":""}),
    ] {
        rpc.receive(&json!({"id":1,"method":"currentTime/read","params":params}).to_string())
            .unwrap();
        assert_eq!(messages.recv().await.unwrap()["error"]["code"], -32602);
    }
    for method in ["currentTime/read", "attestation/generate"] {
        for id in [None, Some(json!(null)), Some(json!({"secret":"private"}))] {
            let mut request = json!({"method":method,"params":{"secret":"private"}});
            if let Some(id) = id {
                request["id"] = id;
            }
            assert_eq!(
                rpc.receive(&request.to_string()).unwrap_err().code,
                "RPC_PROTOCOL"
            );
        }
    }
    assert!(messages.try_recv().is_err());
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn auth_refresh_is_private_and_cannot_use_a_different_account() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    rpc.set_auth_refresh(|previous| {
        Box::pin(async move {
            if previous.as_deref() != Some("account-a") {
                return Err(Error::new(
                    "OAUTH_IDENTITY_CHANGED",
                    "synthetic private error",
                ));
            }
            Ok(json!({"accessToken":"synthetic-private-token","chatgptAccountId":"account-a"}))
        })
    });
    rpc.receive(r#"{"id":"auth-1","method":"account/chatgptAuthTokens/refresh","params":{"previousAccountId":"account-a"}}"#).unwrap();
    assert_eq!(
        messages.recv().await.unwrap()["result"]["accessToken"],
        "synthetic-private-token"
    );
    assert!(events.try_recv().is_err());
    tokio::task::yield_now().await;
    rpc.receive(r#"{"id":"auth-2","method":"account/chatgptAuthTokens/refresh","params":{"previousAccountId":"account-b"}}"#).unwrap();
    let denied = messages.recv().await.unwrap();
    assert_eq!(denied["error"]["code"], -32001);
    assert!(!denied.to_string().contains("synthetic private"));
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn initializes_with_native_metadata_and_notifies_only_after_acknowledgment() {
    for enabled in [false, true] {
        let (rpc, mut messages) = Rpc::new();
        if enabled {
            rpc.set_attestation_provider(|| Box::pin(async { Ok("synthetic-proof".into()) }));
        }
        let client_info = NativeClientInfo {
            name: "codex_desktop".into(),
            title: "Codex Desktop".into(),
            version: "26.1016.51333".into(),
        };
        let expected = serde_json::to_value(&client_info).unwrap();
        let task_rpc = rpc.clone();
        let task = tokio::spawn(async move { task_rpc.initialize(&client_info).await });
        let request = messages.recv().await.unwrap();
        assert_eq!(request["method"], "initialize");
        assert_eq!(request["params"]["clientInfo"], expected);
        assert_eq!(
            request["params"]["capabilities"],
            json!({"experimentalApi":true,"requestAttestation":enabled})
        );
        assert!(messages.try_recv().is_err());
        rpc.receive(
            &json!({"id":request["id"],"result":{"userAgent":"native-response"}}).to_string(),
        )
        .unwrap();
        assert_eq!(task.await.unwrap().unwrap()["userAgent"], "native-response");
        assert_eq!(messages.recv().await.unwrap()["method"], "initialized");
        assert!(messages.try_recv().is_err());
    }
}

#[tokio::test]
async fn attestation_proof_is_private_and_preserves_callback_id() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    rpc.set_attestation_provider(|| Box::pin(async { Ok("synthetic-private-proof".into()) }));
    for id in [json!(42), json!("proof-request")] {
        rpc.receive(&json!({"id":id,"method":"attestation/generate","params":{}}).to_string())
            .unwrap();
        let response = messages.recv().await.unwrap();
        assert_eq!(
            response,
            json!({"id":id,"result":{"token":"synthetic-private-proof"}})
        );
        tokio::task::yield_now().await;
    }
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn unavailable_and_invalid_attestation_never_reaches_api_events() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    rpc.receive(r#"{"id":1,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    assert_eq!(messages.recv().await.unwrap()["error"]["code"], -32001);
    // A caller cannot submit a token through callback parameters.
    rpc.receive(r#"{"id":2,"method":"attestation/generate","params":{"token":"never-forward"}}"#)
        .unwrap();
    let response = messages.recv().await.unwrap();
    assert_eq!(response["error"]["code"], -32602);
    assert!(!response.to_string().contains("never-forward"));
    for invalid in ["", "secret\r\nheader", &"x".repeat(64 * 1024 + 1)] {
        let invalid = invalid.to_owned();
        rpc.set_attestation_provider(move || {
            let invalid = invalid.clone();
            Box::pin(async move { Ok(invalid) })
        });
        rpc.receive(r#"{"id":3,"method":"attestation/generate","params":{}}"#)
            .unwrap();
        let response = messages.recv().await.unwrap();
        assert_eq!(response["error"]["code"], -32001);
        assert!(!response.to_string().contains("secret"));
        tokio::task::yield_now().await;
    }
    rpc.set_attestation_provider(|| {
        Box::pin(async { Err(Error::new("PROOF", "private provider error")) })
    });
    rpc.receive(r#"{"id":4,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    assert!(
        !messages
            .recv()
            .await
            .unwrap()
            .to_string()
            .contains("private provider error")
    );
    assert!(events.try_recv().is_err());
}

#[tokio::test]
async fn attestation_is_bounded_and_cancelled_with_its_connection() {
    let (rpc, mut messages) = Rpc::new();
    rpc.set_attestation_provider(|| Box::pin(std::future::pending()));
    rpc.receive(r#"{"id":1,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    rpc.receive(r#"{"id":2,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    let busy = messages.recv().await.unwrap();
    assert_eq!(busy["id"], 2);
    assert_eq!(busy["error"]["code"], -32001);
    rpc.close();
    tokio::time::timeout(Duration::from_secs(1), async {
        while rpc.inner.attestation_active.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(messages.try_recv().is_err());
}

#[tokio::test]
async fn attestation_timeout_releases_provider_slot() {
    let (rpc, mut messages) = Rpc::new();
    rpc.set_attestation_provider(|| Box::pin(std::future::pending()));
    rpc.receive(r#"{"id":1,"method":"attestation/generate","params":{}}"#)
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(10), messages.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply["error"]["code"], -32001);
    tokio::task::yield_now().await;
    assert!(!rpc.inner.attestation_active.load(Ordering::SeqCst));
}

#[tokio::test]
async fn correlates_out_of_order_responses_and_redacts_errors() {
    let (rpc, mut messages) = Rpc::new();
    let a = rpc.clone();
    let a = tokio::spawn(async move { a.request("a", json!({})).await });
    let first = messages.recv().await.unwrap();
    let b = rpc.clone();
    let b = tokio::spawn(async move { b.request("b", json!({})).await });
    let second = messages.recv().await.unwrap();
    rpc.receive(&json!({"id":second["id"],"result":{"value":2}}).to_string())
        .unwrap();
    rpc.receive(&json!({"id":first["id"],"error":{"message":"secret"}}).to_string())
        .unwrap();
    assert_eq!(b.await.unwrap().unwrap()["value"], 2);
    assert!(!a.await.unwrap().unwrap_err().to_string().contains("secret"));
    assert!(rpc.inner.pending.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cancellation_timeout_and_disconnect_release_slots_without_replay() {
    let (rpc, mut messages) = Rpc::new();
    let task_rpc = rpc.clone();
    let task = tokio::spawn(async move { task_rpc.request("cancel", json!({})).await });
    messages.recv().await.unwrap();
    task.abort();
    let _ = task.await;
    assert!(rpc.inner.pending.lock().unwrap().is_empty());
    assert_eq!(
        rpc.request_timeout("timeout", json!({}), Duration::from_millis(10))
            .await
            .unwrap_err()
            .code,
        "RPC_TIMEOUT"
    );
    assert_eq!(messages.recv().await.unwrap()["method"], "timeout");
    assert!(messages.try_recv().is_err());
    assert!(rpc.inner.pending.lock().unwrap().is_empty());
    let task_rpc = rpc.clone();
    let task = tokio::spawn(async move { task_rpc.request("disconnect", json!({})).await });
    messages.recv().await.unwrap();
    rpc.close();
    assert_eq!(task.await.unwrap().unwrap_err().code, "RPC_CLOSED");
}

#[tokio::test]
async fn callbacks_are_rejected_and_protocol_failures_close_stdio() {
    let (rpc, mut messages) = Rpc::new();
    let mut events = rpc.subscribe();
    rpc.receive(r#"{"id":1,"method":"account/chatgptAuthTokens/refresh","params":{"secret":"never-forward"}}"#).unwrap();
    let rejected = messages.recv().await.unwrap();
    assert_eq!(rejected["error"]["code"], -32601);
    assert!(
        !events
            .recv()
            .await
            .unwrap()
            .to_string()
            .contains("never-forward")
    );
    let (client, mut server) = tokio::io::duplex(1024);
    let (reader, writer) = tokio::io::split(client);
    let rpc = Rpc::from_io(reader, writer);
    use tokio::io::AsyncWriteExt;
    server.write_all(b"{bad json}\n").await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !rpc.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(rpc.request("after-close", json!({})).await.is_err());
}

#[tokio::test]
async fn quota_subscribers_ignore_tokens_and_claim_each_update_only_once() {
    let (rpc, _messages) = Rpc::new();
    let mut first = rpc.subscribe_quota();
    let mut second = rpc.subscribe_quota();
    for _ in 0..600 {
        rpc.publish(json!({"method":"item/agentMessage/delta","params":{"threadId":"first","delta":"text"}}));
    }
    assert!(matches!(
        first.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    rpc.publish(
        json!({"method":"account/rateLimits/updated","params":{"rateLimits":{"usedPercent":50}}}),
    );
    let one = first.recv().await.unwrap();
    let two = second.recv().await.unwrap();
    assert!(std::sync::Arc::ptr_eq(&one, &two));
    assert!(one.claim());
    assert!(!two.claim());
    rpc.publish(
        json!({"method":"account/rateLimits/updated","params":{"rateLimits":{"usedPercent":60}}}),
    );
    assert!(second.recv().await.unwrap().claim());
}

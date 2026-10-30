//! Real management-router responses exported for the console's typed contracts.
//! Uses isolated storage and synthetic data; no native process or model request.
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use codex_relay::{
    admin::{AdminState, router},
    error::Error,
    operations::{DEFAULT_GROUP, quota::normalize_reading},
    provider::Provider,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

async fn call(app: &axum::Router, path: &str, body: Option<Value>) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(if body.is_some() { "POST" } else { "GET" })
                .uri(path)
                .header("host", "127.0.0.1:4317")
                .header("origin", "http://127.0.0.1:4317")
                .header("content-type", "application/json")
                .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 5 * 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        status == StatusCode::OK || status == StatusCode::CREATED,
        "{path}: {body}"
    );
    body
}

#[tokio::test]
async fn management_responses_cover_console_contract_variants() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("accounts");
    let state = AdminState::new_with_network(4317, root.clone(), None, vec![])
        .await
        .unwrap();
    let app = router(state.clone(), temp.path().join("web"));
    let affinity_default = call(&app, "/api/operations", None).await;
    assert_eq!(affinity_default["nativeAffinityTtlHours"], 24);
    let affinity_changed = call(
        &app,
        "/api/settings/native-affinity",
        Some(json!({
            "revision": affinity_default["revision"], "nativeAffinityTtlHours": 30
        })),
    )
    .await;
    assert_eq!(affinity_changed["nativeAffinityTtlHours"], 30);
    let isolated = call(
        &app,
        "/api/accounts",
        Some(json!({"name":"Contract isolated"})),
    )
    .await;
    call(
        &app,
        "/api/accounts",
        Some(json!({"name":"Contract native","nativeConfig":true})),
    )
    .await;
    call(&app, "/api/accounts", Some(json!({
        "name":"Contract API", "source":"api-key",
        "upstream":{"baseUrl":"http://127.0.0.1:9/v1","apiKey":"synthetic-contract-only","transport":"http-sse"}
    }))).await;
    let id: Uuid = isolated["id"].as_str().unwrap().parse().unwrap();
    let provider = Provider {
        execution: Default::default(),
        id: "contract-provider".into(),
        model: "contract-model".into(),
        base_url: "http://127.0.0.1:9/v1".into(),
        auth: "chatgpt".into(),
        forward_headers: vec![],
        required_headers: vec![],
        mcp_servers: vec![],
    };
    let revision = state.operations.snapshot()["revision"].as_u64().unwrap();
    state
        .operations
        .set_inference_route(id, revision, Some(provider.clone()))
        .unwrap();
    let path = format!("/api/accounts/{id}/quota");
    // Exercise production discovery before checking real management serialization.
    let (client, server) = tokio::io::duplex(8192);
    let (r, w) = tokio::io::split(client);
    let rpc = codex_relay::rpc::Rpc::from_io(r, w);
    let discovery = tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (r, mut w) = tokio::io::split(server);
        let mut lines = BufReader::new(r).lines();
        for method in ["model/list", "modelProvider/capabilities/read"] {
            let request: Value =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(request["method"], method);
            let result = if method == "model/list" {
                json!({"data":[{"model":"contract-model","inputModalities":["text"],"supportedReasoningEfforts":[{"reasoningEffort":"high","description":"High"}],"serviceTiers":[{"id":"ultrafast","name":"Ultrafast","description":"Synthetic tier"}],"defaultServiceTier":"ultrafast"},{"model":"unknown-metadata"}],"nextCursor":null})
            } else {
                json!({"webSearch":true,"imageGeneration":false})
            };
            w.write_all(format!("{}\n", json!({"id":request["id"],"result":result})).as_bytes())
                .await
                .unwrap();
        }
    });
    let capabilities =
        codex_relay::operations::runtime::discover(&rpc, &provider, "synthetic-install").await;
    assert!(!capabilities.stale);
    assert_eq!(capabilities.models[0]["serviceTiers"][0]["id"], "ultrafast");
    assert_eq!(capabilities.models[0]["defaultServiceTier"], "ultrafast");
    assert!(capabilities.models[1]["serviceTiers"].is_null());
    discovery.await.unwrap();
    state
        .operations
        .publish_capabilities(id, capabilities)
        .unwrap();
    let mut quotas = vec![json!({"name":"unknown","value":call(&app, &path, None).await})];
    for (name, code) in [
        ("unsupported", "QUOTA_UNSUPPORTED"),
        ("error-before-reading", "SYNTHETIC_QUOTA_ERROR"),
    ] {
        state
            .operations
            .publish_quota_reading(
                id,
                &provider,
                "contract-identity".into(),
                Err(Error::new(code, "Synthetic contract result")),
            )
            .unwrap();
        quotas.push(json!({"name":name,"value":call(&app, &path, None).await}));
    }
    for (name, used) in [("supported", 25), ("blocked", 100)] {
        let mut reading = normalize_reading(
            &json!({"rateLimits":{
                "limitId":"codex",
                "primary":{"usedPercent":used,"windowDurationMins":300,"resetsAt":codex_relay::now_ms()/1000+3600},
                "secondary":{"usedPercent":null,"windowDurationMins":10080,"resetsAt":null},
                "credits":{"balance":"10.5","hasCredits":used < 100,"unlimited":false},
                "rateLimitReachedType":if used == 100 {Some("usage_limit_reached")} else {None}
            }}),
            true,
        ).unwrap();
        reading.reset_credits = Some(serde_json::from_value(json!({
            "availableCount":1,"checkedAt":codex_relay::now_ms(),
            "credits":[{"id":"synthetic-reset","resetType":"manual","status":"available","expiresAt":null}]
        })).unwrap());
        state
            .operations
            .publish_quota_reading(id, &provider, "contract-identity".into(), Ok(reading))
            .unwrap();
        quotas.push(json!({"name":name,"value":call(&app, &path, None).await}));
    }
    state
        .operations
        .publish_quota_reading(
            id,
            &provider,
            "contract-identity".into(),
            Err(Error::new(
                "SYNTHETIC_QUOTA_ERROR",
                "Synthetic contract result",
            )),
        )
        .unwrap();
    quotas.push(json!({"name":"error-with-stale-reading","value":call(&app, &path, None).await}));

    let now = codex_relay::now_ms() as i64;
    let stats_path = format!("/api/statistics?from={}&to={}", now - 60_000, now + 60_000);
    let empty = call(&app, &stats_path, None).await;
    let db = Connection::open(root.join("operations/operations.sqlite3")).unwrap();
    for (input, output, status, actual_tier) in [
        (None, None, "failed", None),
        (Some(0_i64), Some(0_i64), "succeeded", Some("default")),
        (Some(100), Some(20), "succeeded", Some("priority")),
    ] {
        db.execute(
            "INSERT INTO requests(id,started_at,finished_at,kind,action,account_id,account_name,group_id,
             group_name,key_id,key_name,status,model,input_tokens,output_tokens,cached_input_tokens,wait_ms,duration_ms,service_tier,response_service_tier)
             VALUES(?1,?2,?3,'inference','responses',?4,'Contract isolated',?5,'Default',?6,'Contract key',?7,
             'contract-model',?8,?9,?10,0,10,'priority',?11)",
            params![Uuid::new_v4().to_string(), now - 100, now - 90, id.to_string(), DEFAULT_GROUP.to_string(),
                "30000000-0000-0000-0000-000000000001", status, input, output, input.map(|_| 0), actual_tier],
        ).unwrap();
    }
    db.execute(
        "UPDATE requests SET first_delta_ms=200,wait_ms=50 WHERE input_tokens=100",
        [],
    )
    .unwrap();
    let populated = call(&app, &stats_path, None).await;
    assert_eq!(empty["records"].as_array().unwrap().len(), 0);
    assert_eq!(populated["records"].as_array().unwrap().len(), 3);
    assert_eq!(populated["summary"]["inputTokens"], 100);
    assert_eq!(populated["summary"]["averageFirstResponseMs"], 250.0);
    assert_eq!(populated["summary"]["averageModelFirstResponseMs"], 200.0);
    assert_eq!(
        populated["keyUsage"][0]["averageModelFirstResponseMs"],
        200.0
    );
    let bucket = populated["trend"]
        .as_array()
        .unwrap()
        .iter()
        .find(|bucket| bucket["inferenceRequests"] == 3)
        .unwrap();
    assert_eq!(bucket["cacheKnownRequests"], 2);
    assert_eq!(bucket["cacheKnownInputTokens"], 100);
    assert_eq!(bucket["cachedInputTokens"], 0);
    assert_eq!(bucket["cacheHitRate"], 0.0);
    let overview = call(&app, "/api/overview", None).await;
    assert_eq!(overview["accounts"].as_array().unwrap().len(), 3);
    assert!(
        overview["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|account| account["state"] == "ready")
    );
    let claims = json!({"exp":4102444800u64,"sub":"contract-user","https://api.openai.com/auth":{"chatgpt_account_id":"contract-workspace","chatgpt_user_id":"contract-user"}});
    let credential = json!({"access_token":format!("e30.{}.signature",URL_SAFE_NO_PAD.encode(claims.to_string()))});
    call(
        &app,
        &format!("/api/accounts/{id}/credentials"),
        Some(json!({"document":credential})),
    )
    .await;
    let ids: Vec<_> = overview["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| account["id"].clone())
        .collect();
    let backup = call(
        &app,
        "/api/accounts/export",
        Some(json!({"accountIds":ids})),
    )
    .await;
    let exported = backup["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["document"].is_object())
        .unwrap();
    let imported = call(&app, "/api/accounts/batch", Some(json!({
        "revision":state.operations.snapshot()["revision"],"action":"import",
        "items":[{"name":"Duplicate contract account","source":"isolated","document":exported["document"],"groupIds":[DEFAULT_GROUP],"maxConcurrency":3}]
    }))).await;
    assert_eq!(imported["items"][0]["code"], "DUPLICATE_ACCOUNT");
    let debug_off = call(&app, "/api/settings/debug", None).await;
    let debug_on = call(
        &app,
        "/api/settings/debug",
        Some(json!({"enabled":true,"revision":debug_off["revision"]})),
    )
    .await;
    let raw_id = populated["records"][0]["id"].as_str().unwrap();
    db.execute("UPDATE requests SET request_content=?1,response_content=?2 WHERE id=?3", params![
        json!({"body":"{ \"input\": \"contract prompt\" }","debug":true,"encoding":"utf8","textOmitted":false,"truncated":false,"complete":true}).to_string(),
        json!({"body":"data: [DONE]\n\n","debug":true,"encoding":"utf8","transport":"sse","statusCode":200,"truncated":false,"complete":true}).to_string(), raw_id
    ]).unwrap();
    let raw_content = call(&app, &format!("/api/requests/{raw_id}/content"), None).await;
    let debug_disabled = call(
        &app,
        "/api/settings/debug",
        Some(json!({"enabled":false,"revision":debug_on["revision"]})),
    )
    .await;
    let content_id = populated["records"][0]["id"].as_str().unwrap();
    let debug_content = call(&app, &format!("/api/requests/{content_id}/content"), None).await;
    let installations = ["automatic", "standalone"].map(|mode| {
            let installation: codex_relay::installation::Installation = serde_json::from_value(json!({
                "format":1,"preparedAt":0,"manuallyConfigured":mode != "automatic",
                "desktopExecutable":(mode != "standalone").then(|| temp.path().join("Codex.exe")),
                "appServerExecutable":temp.path().join("codex.exe"),
                "archive":(mode != "standalone").then(|| temp.path().join("app.asar")),"appServerSha256":"fixture",
                "appServerVersion":"codex-cli fixture","sourceFiles":[]
            })).unwrap();
            json!({"name":mode,"value":installation})
        });
    let corpus = json!({
        "overviewInstallation": overview["installation"],
        "installations": installations,
        "debugSettings":[{"name":"off","value":debug_off},{"name":"on","value":debug_on},{"name":"disabled","value":debug_disabled}],
        "requestContents":[{"name":"raw","value":raw_content},{"name":"cleared","value":debug_content}],
        "operations":state.operations.snapshot(),
        "affinitySettings": [{"name":"default","value":affinity_default},{"name":"changed","value":affinity_changed}],
        "accountBackup":backup,
        "accountImport":imported,
        "accounts": overview["accounts"],
        "quotas": quotas,
        "statistics": [{"name":"empty","value":empty},{"name":"populated","value":populated}],
    });
    if let Some(output) = std::env::var_os("BRIDGE_ADMIN_CONTRACT_OUTPUT") {
        std::fs::write(output, serde_json::to_vec_pretty(&corpus).unwrap()).unwrap();
    }
}
